//! Ultra-low-latency Binance Futures signal executor.
//!
//! Startup sequence:
//!   1. read `.env` + `config/api_keys.json`
//!   2. verify Redis, load exchangeInfo
//!   3. open one WebSocket API session per trading account
//!   4. warm the price/balance caches
//!   5. enter the dispatch loop (Redis queue + UDP bridge in, orders out)

mod binance_ws;
mod bridge;
mod cache;
mod config;
mod dedup;
mod exchange_info;
mod executor;
mod math;
mod metrics;
mod queue;
mod selftest;
mod signal;
mod types;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::binance_ws::BinanceWsClient;
use crate::cache::{balances_loop, check_redis, tickers_loop, wait_for_balances, MarketCache};
use crate::config::Config;
use crate::exchange_info::ExchangeInfoCache;
use crate::executor::TradeExecutor;
use crate::metrics::Metrics;
use crate::types::ApiKeysConfig;

const SIGNAL_CHANNEL_CAPACITY: usize = 4_096;

#[tokio::main]
async fn main() -> Result<()> {
    dotenv::dotenv().ok();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--selftest") {
        return selftest::run();
    }

    init_tracing();

    let cfg = Arc::new(Config::from_env());
    print_banner(&cfg);

    // ---- Redis ----------------------------------------------------------
    check_redis(&cfg.redis_url)
        .await
        .with_context(|| format!("Redis is not reachable at {}", cfg.redis_url))?;
    info!("✅ Redis reachable");

    let redis_client = redis::Client::open(cfg.redis_url.as_str()).context("invalid Redis URL")?;

    // ---- accounts -------------------------------------------------------
    let accounts = load_accounts(&cfg)?;
    let account_names: Vec<String> = accounts.iter().map(|a| a.owner.clone()).collect();
    info!(
        count = accounts.len(),
        names = ?account_names,
        "✅ trading accounts loaded"
    );

    // ---- exchangeInfo ---------------------------------------------------
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("failed to build HTTP client")?;
    let exchange_info = ExchangeInfoCache::load(&http, &cfg.rest_base_url).await?;

    // ---- WS sessions ----------------------------------------------------
    let mut clients: Vec<(String, Arc<BinanceWsClient>)> = Vec::with_capacity(accounts.len());
    for account in &accounts {
        let client = BinanceWsClient::new(
            account.public.clone(),
            account.private.clone(),
            cfg.ws_api_url.clone(),
            Duration::from_millis(cfg.order_timeout_ms),
            cfg.recv_window_ms,
        );
        clients.push((account.owner.clone(), client));
    }
    wait_for_connections(&clients, Duration::from_secs(10)).await;

    // ---- caches ---------------------------------------------------------
    let cache = MarketCache::new(account_names);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    tokio::spawn(tickers_loop(cache.clone(), cfg.clone(), shutdown_rx.clone()));
    tokio::spawn(balances_loop(
        cache.clone(),
        cfg.clone(),
        shutdown_rx.clone(),
    ));

    if wait_for_balances(&cache, Duration::from_secs(5)).await {
        info!("✅ balances cached");
        for (idx, name) in cache.accounts().iter().enumerate() {
            info!(account = %name, balance_usdt = cache.balance(idx), "account balance");
        }
    } else {
        warn!("⚠️ no balances yet - orders will be skipped until `balance:<owner>` appears in Redis");
    }

    // ---- executor -------------------------------------------------------
    let redis_conn = redis_client
        .get_multiplexed_tokio_connection()
        .await
        .context("failed to open a multiplexed Redis connection")?;

    let metrics = Metrics::new();
    let executor = TradeExecutor::new(
        cfg.clone(),
        cache.clone(),
        exchange_info.clone(),
        clients.clone(),
        redis_conn,
        metrics.clone(),
    );

    spawn_background_tasks(
        &cfg,
        &executor,
        &metrics,
        &cache,
        &exchange_info,
        &shutdown_rx,
    )
    .await?;

    // ---- input pumps ----------------------------------------------------
    let (signal_tx, mut signal_rx) = mpsc::channel::<String>(SIGNAL_CHANNEL_CAPACITY);

    tokio::spawn(queue::run(
        cfg.redis_url.clone(),
        cfg.queue_name.clone(),
        signal_tx.clone(),
        shutdown_rx.clone(),
    ));

    let forward = if cfg.bridge_forward_redis {
        let connection = redis_client
            .get_multiplexed_tokio_connection()
            .await
            .context("failed to open Redis connection for the UDP bridge")?;
        Some(bridge::RedisForward {
            connection,
            queue: cfg.queue_name.clone(),
        })
    } else {
        None
    };

    {
        let bridge_port = cfg.bridge_port;
        let bridge_tx = signal_tx.clone();
        let bridge_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            if let Err(e) =
                bridge::run(bridge_port, bridge_tx, forward, bridge_shutdown).await
            {
                error!(error = %e, "UDP bridge stopped - signals now arrive via Redis only");
            }
        });
    }
    // Keep only the pumps as senders so `recv()` ends when both stop.
    drop(signal_tx);

    info!(
        queue = %cfg.queue_name,
        udp_port = cfg.bridge_port,
        accounts = executor.accounts().len(),
        "🎯 engine fully operational - waiting for signals"
    );

    // ---- dispatch loop --------------------------------------------------
    loop {
        tokio::select! {
            payload = signal_rx.recv() => {
                match payload {
                    Some(raw) => executor.handle_signal(raw).await,
                    None => {
                        warn!("all signal sources stopped");
                        break;
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("🛑 shutdown requested");
                break;
            }
        }
    }

    // ---- shutdown -------------------------------------------------------
    let _ = shutdown_tx.send(true);
    executor.shutdown().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    info!(metrics = %metrics.snapshot(), "✅ shutdown complete");
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .try_init();
}

fn print_help() {
    println!(
        "rust_trader - Binance Futures signal executor\n\n\
         USAGE:\n    rust_trader [--selftest]\n\n\
         OPTIONS:\n    \
         --selftest   run offline sanity checks and exit\n    \
         --help       show this message\n\n\
         ENVIRONMENT (see .env):\n    \
         REDIS_HOST / REDIS_PORT / REDIS_DB or REDIS_URL\n    \
         REDIS_QUEUE (default trade_queue), REDIS_TICKERS_HASH (futures_tickers)\n    \
         BALANCE_KEY_PREFIX (balance:) or REDIS_BALANCES_HASH (futures_balances)\n    \
         TRADE_ALLOCATION (0.3) or TRADE_ALLOCATION_BPS (3000)\n    \
         TP_PERCENT (0.3) / SL_PERCENT (0.3) or *_BPS variants\n    \
         SL_LIMIT_SLIPPAGE_PERCENT / SL_LIMIT_SLIPPAGE_BPS\n    \
         BINANCE_TESTNET (false), BINANCE_WS_API_URL, BINANCE_REST_URL\n    \
         TRADE_DRY_RUN (false), BRIDGE_PORT (5005), BRIDGE_FORWARD_REDIS (false)\n    \
         DEDUP: DUPLICATE_SIGNAL_LOCK_SEC (60), REDIS_DEDUP (true)\n    \
         RUST_LOG (info)"
    );
}

fn print_banner(cfg: &Config) {
    info!("🚀 rust_trader {} - ultra-low-latency mode", env!("CARGO_PKG_VERSION"));
    info!("{}", cfg.summary());
    info!(
        "   allocation={:.2}%  tp={:.2}%  sl={:.2}%  sl_slippage={:.2}%",
        cfg.trade_allocation_bps as f64 / 100.0,
        cfg.tp_bps as f64 / 100.0,
        cfg.sl_bps as f64 / 100.0,
        cfg.sl_limit_slippage_bps as f64 / 100.0
    );
}

fn load_accounts(cfg: &Config) -> Result<Vec<types::AccountConfig>> {
    let raw = std::fs::read_to_string(&cfg.api_keys_path).with_context(|| {
        format!(
            "cannot read API keys at {} (override with API_KEYS_FILE)",
            cfg.api_keys_path
        )
    })?;
    let keys: ApiKeysConfig =
        serde_json::from_str(&raw).context("cannot parse API keys JSON")?;
    let accounts = keys.trading_accounts();
    if accounts.is_empty() {
        anyhow::bail!(
            "no trading accounts found in {} (founders/friends/clients are empty)",
            cfg.api_keys_path
        );
    }
    Ok(accounts)
}

async fn wait_for_connections(clients: &[(String, Arc<BinanceWsClient>)], timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if clients.iter().all(|(_, client)| client.is_connected()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let connected = clients
        .iter()
        .filter(|(_, client)| client.is_connected())
        .count();
    if connected == clients.len() {
        info!(connected, "✅ all WS API sessions connected");
    } else {
        warn!(
            connected,
            total = clients.len(),
            "⚠️ some WS API sessions are not connected yet (they reconnect in the background)"
        );
        for (name, client) in clients {
            if !client.is_connected() {
                warn!(account = %name, "WS API session pending");
            }
        }
    }
}

async fn spawn_background_tasks(
    cfg: &Arc<Config>,
    executor: &Arc<TradeExecutor>,
    metrics: &Arc<Metrics>,
    cache: &Arc<MarketCache>,
    exchange_info: &Arc<ExchangeInfoCache>,
    shutdown_rx: &watch::Receiver<bool>,
) -> Result<()> {
    // Dedup TTL sweeper.
    {
        let dedup = executor.deduplicator().clone();
        let mut shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(30));
            loop {
                tokio::select! {
                    _ = tick.tick() => dedup.sweep(),
                    _ = shutdown.changed() => {
                        if *shutdown.borrow() { break; }
                    }
                }
            }
        });
    }

    // Heartbeat: counters plus cache/connection health.
    {
        let metrics = metrics.clone();
        let executor = executor.clone();
        let cache = cache.clone();
        let symbol_count = exchange_info.len();
        let mut shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        let mut requests = 0u64;
                        let mut responses = 0u64;
                        let mut pending = 0usize;
                        for account in executor.accounts() {
                            let (sent, seen) = account.client.stats();
                            requests += sent;
                            responses += seen;
                            pending += account.client.pending_requests();
                        }
                        info!(
                            metrics = %metrics.snapshot(),
                            symbols = symbol_count,
                            tickers = cache.tickers_len(),
                            dedup_entries = executor.deduplicator().len(),
                            ws_pending = pending,
                            ws_requests = requests,
                            ws_responses = responses,
                            "heartbeat"
                        );
                    }
                    _ = shutdown.changed() => {
                        if *shutdown.borrow() { break; }
                    }
                }
            }
        });
    }

    info!(
        bridge_forward = cfg.bridge_forward_redis,
        "✅ background tasks started"
    );
    Ok(())
}
