//! Signal -> order orchestration.
//!
//! The hot path is: parse (no allocation beyond the symbol) -> local dedup ->
//! one signed `order.place` per account, all in parallel -> protective bracket
//! from the fill data that came back in the same response.
//!
//! Nothing in this path waits for Redis except the optional cross-process
//! duplicate lock, which runs inside the per-signal task rather than inline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{debug, error, info, warn};

use crate::binance_ws::{BinanceWsClient, OrderSpec, TpSlManager};
use crate::cache::MarketCache;
use crate::config::Config;
use crate::dedup::Deduplicator;
use crate::exchange_info::ExchangeInfoCache;
use crate::math::{
    adjust_quantity_down, calculate_notional, format_quantity, price_scaled_to_f64,
    price_to_scaled, quantity_from_notional, scaled_to_f64, validate_order,
};
use crate::metrics::Metrics;
use crate::signal::{self, ParsedSignal};
use crate::types::{Side, SymbolFilters, TickerData, TpSlParams};

/// One tradable account: its WS session, protective-order manager and the
/// index of its balance slot in the cache.
#[derive(Clone)]
pub struct AccountHandle {
    pub name: String,
    pub index: usize,
    pub client: Arc<BinanceWsClient>,
    pub tp_sl: Arc<TpSlManager>,
}

pub struct TradeExecutor {
    cfg: Arc<Config>,
    cache: Arc<MarketCache>,
    exchange: Arc<ExchangeInfoCache>,
    accounts: Vec<AccountHandle>,
    redis: redis::aio::MultiplexedConnection,
    dedup: Arc<Deduplicator>,
    metrics: Arc<Metrics>,
}

impl TradeExecutor {
    pub fn new(
        cfg: Arc<Config>,
        cache: Arc<MarketCache>,
        exchange: Arc<ExchangeInfoCache>,
        accounts: Vec<(String, Arc<BinanceWsClient>)>,
        redis: redis::aio::MultiplexedConnection,
        metrics: Arc<Metrics>,
    ) -> Arc<Self> {
        let tp_sl_params = TpSlParams {
            tp_bps: cfg.tp_bps,
            sl_bps: cfg.sl_bps,
            sl_limit_slippage_bps: cfg.sl_limit_slippage_bps,
        };

        let accounts = accounts
            .into_iter()
            .map(|(name, client)| {
                let index = cache.account_index(&name).unwrap_or(0);
                let tp_sl = TpSlManager::new(client.clone(), tp_sl_params);
                AccountHandle {
                    name,
                    index,
                    client,
                    tp_sl,
                }
            })
            .collect();

        Arc::new(Self {
            dedup: Deduplicator::new(cfg.duplicate_signal_lock_sec),
            cfg,
            cache,
            exchange,
            accounts,
            redis,
            metrics,
        })
    }

    pub fn accounts(&self) -> &[AccountHandle] {
        &self.accounts
    }

    pub fn deduplicator(&self) -> &Arc<Deduplicator> {
        &self.dedup
    }

    /// Entry point for a raw queue/bridge payload. Returns immediately: the
    /// actual work runs in a spawned task so the queue consumer keeps draining.
    pub async fn handle_signal(self: &Arc<Self>, raw: String) {
        let Some(parsed) = signal::parse(&raw) else {
            Metrics::inc(&self.metrics.invalid_signals);
            debug!(payload = %raw, "ignored non-signal payload");
            return;
        };

        Metrics::inc(&self.metrics.signals);

        if !self.dedup.check_and_mark(&parsed.symbol, parsed.side) {
            Metrics::inc(&self.metrics.duplicates);
            debug!(symbol = %parsed.symbol, side = %parsed.side, "duplicate signal suppressed");
            return;
        }
        self.dedup.remove_opposite(&parsed.symbol, parsed.side);

        let executor = self.clone();
        tokio::spawn(async move { executor.process(parsed).await });
    }

    async fn process(self: Arc<Self>, parsed: ParsedSignal) {
        let symbol: Arc<str> = Arc::from(parsed.symbol.as_str());
        let side = parsed.side;

        // Cross-process / cross-restart lock, identical key to the Python engine.
        if self.cfg.redis_dedup {
            match self.acquire_redis_lock(&symbol, side).await {
                Ok(true) => self.release_redis_lock_opposite(&symbol, side),
                Ok(false) => {
                    Metrics::inc(&self.metrics.duplicates);
                    debug!(symbol = %symbol, side = %side, "duplicate signal (redis lock)");
                    return;
                }
                Err(e) => {
                    // Redis trouble must not stop trading: the local lock already
                    // filtered the signal.
                    warn!(error = %e, "redis dedup lock failed, continuing");
                }
            }
        }

        let Some(ticker) = self.resolve_ticker(&symbol).await else {
            Metrics::inc(&self.metrics.no_ticker);
            warn!(symbol = %symbol, "no ticker data, signal skipped");
            return;
        };

        let Some(filters) = self.exchange.get(&symbol) else {
            Metrics::inc(&self.metrics.no_filters);
            warn!(symbol = %symbol, "symbol is not in exchangeInfo, signal skipped");
            return;
        };

        let started = Instant::now();
        let mut tasks = Vec::with_capacity(self.accounts.len());
        for account in &self.accounts {
            let account = account.clone();
            let executor = self.clone();
            let symbol = symbol.clone();
            tasks.push(tokio::spawn(async move {
                executor
                    .execute_for_account(account, symbol, side, ticker, filters)
                    .await;
            }));
        }

        for task in tasks {
            if let Err(e) = task.await {
                error!(error = %e, "account task panicked");
            }
        }

        debug!(
            symbol = %symbol,
            side = %side,
            accounts = self.accounts.len(),
            fanout_us = started.elapsed().as_micros() as u64,
            "signal fan-out complete"
        );
    }

    async fn execute_for_account(
        self: Arc<Self>,
        account: AccountHandle,
        symbol: Arc<str>,
        side: Side,
        ticker: TickerData,
        filters: SymbolFilters,
    ) {
        let balance = self.cache.balance_scaled(account.index);
        if balance == 0 {
            Metrics::inc(&self.metrics.no_balance);
            debug!(account = %account.name, symbol = %symbol, "zero balance, account skipped");
            return;
        }

        let price = price_to_scaled(ticker.last);
        if price == 0 {
            Metrics::inc(&self.metrics.no_ticker);
            return;
        }

        let leverage = ticker.leverage.max(1);
        let notional = calculate_notional(balance, self.cfg.trade_allocation_bps, leverage);
        let qty = adjust_quantity_down(quantity_from_notional(notional, price), filters.step_size);

        if !validate_order(notional, qty, &filters) {
            Metrics::inc(&self.metrics.below_min);
            debug!(
                account = %account.name,
                symbol = %symbol,
                notional_usdt = scaled_to_f64(notional),
                qty = %format_quantity(qty, &filters),
                min_notional_usdt = scaled_to_f64(filters.min_notional),
                "order below exchange minimums, skipped"
            );
            return;
        }

        let quantity = format_quantity(qty, &filters);

        if self.cfg.dry_run {
            Metrics::inc(&self.metrics.dry_runs);
            info!(
                account = %account.name,
                symbol = %symbol,
                side = %side,
                qty = %quantity,
                price = price_scaled_to_f64(price),
                notional_usdt = scaled_to_f64(notional),
                leverage,
                "[dry-run] market order suppressed"
            );
            return;
        }

        let started = Instant::now();
        let result = account
            .client
            .place(OrderSpec::market(&symbol, side, quantity.clone()))
            .await;

        match result {
            Ok(order) => {
                let rtt_us = started.elapsed().as_micros() as u64;
                Metrics::inc(&self.metrics.orders_ok);
                self.metrics.observe_latency(rtt_us);

                info!(
                    account = %account.name,
                    symbol = %symbol,
                    side = %side,
                    qty = %quantity,
                    order_id = order.order_id,
                    status = %order.status,
                    filled = order.is_filled(),
                    avg_price = %order.avg_price,
                    rtt_us,
                    "order accepted"
                );

                let fill = match order.fill() {
                    Some(fill) => {
                        Metrics::inc(&self.metrics.fast_fills);
                        Some(fill)
                    }
                    None => {
                        // Filled asynchronously (rare for MARKET): resolve the
                        // price out of band before arming the bracket.
                        let polled = self.poll_fill(&account, &symbol, order.order_id).await;
                        if polled.is_some() {
                            Metrics::inc(&self.metrics.polled_fills);
                        }
                        polled
                    }
                };

                match fill {
                    Some((avg_price, executed_qty)) => {
                        self.spawn_bracket(account, symbol, side, avg_price, executed_qty, filters)
                    }
                    None => warn!(
                        account = %account.name,
                        symbol = %symbol,
                        order_id = order.order_id,
                        "fill not reported, TP/SL not armed - check manually"
                    ),
                }
            }
            Err(e) => {
                Metrics::inc(&self.metrics.orders_failed);
                error!(
                    account = %account.name,
                    symbol = %symbol,
                    side = %side,
                    qty = %quantity,
                    error = %e,
                    "order rejected"
                );
            }
        }
    }

    /// Places TP and SL without blocking the order path.
    fn spawn_bracket(
        &self,
        account: AccountHandle,
        symbol: Arc<str>,
        side: Side,
        avg_price: u64,
        executed_qty: u64,
        filters: SymbolFilters,
    ) {
        let metrics = self.metrics.clone();
        let tp_sl = account.tp_sl.clone();
        let account_name = account.name.clone();

        tokio::spawn(async move {
            let bracket = tp_sl
                .place_bracket(&symbol, side, avg_price, executed_qty, &filters)
                .await;

            if bracket.all_ok() {
                Metrics::inc(&metrics.tpsl_ok);
                info!(
                    account = %account_name,
                    symbol = %symbol,
                    avg_price = price_scaled_to_f64(avg_price),
                    qty = %format_quantity(executed_qty, &filters),
                    "TP/SL armed"
                );
            } else if bracket.tp_ok || bracket.sl_ok {
                Metrics::inc(&metrics.tpsl_partial);
                error!(
                    account = %account_name,
                    symbol = %symbol,
                    tp_error = ?bracket.tp_error,
                    sl_error = ?bracket.sl_error,
                    "TP/SL only partially armed - naked position risk"
                );
            } else {
                Metrics::inc(&metrics.tpsl_failed);
                error!(
                    account = %account_name,
                    symbol = %symbol,
                    tp_error = ?bracket.tp_error,
                    sl_error = ?bracket.sl_error,
                    "TP/SL placement failed - naked position risk"
                );
            }
        });
    }

    async fn poll_fill(
        &self,
        account: &AccountHandle,
        symbol: &str,
        order_id: u64,
    ) -> Option<(u64, u64)> {
        for attempt in 0..self.cfg.fill_poll_attempts.max(1) {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(self.cfg.fill_poll_interval_ms)).await;
            }
            match account.client.get_order_status(symbol, order_id).await {
                Ok(status) => {
                    if let Some(fill) = status.fill() {
                        return Some(fill);
                    }
                    debug!(
                        order_id,
                        status = %status.status,
                        executed = %status.executed_qty,
                        "waiting for fill"
                    );
                }
                Err(e) => debug!(order_id, error = %e, "order.status failed"),
            }
        }
        None
    }

    /// Fast path: use the cached price when fresh, otherwise do a single HGET.
    async fn resolve_ticker(&self, symbol: &str) -> Option<TickerData> {
        let max_age = Duration::from_millis(self.cfg.max_ticker_age_ms);
        if let Some(ticker) = self.cache.ticker(symbol) {
            match self.cache.ticker_age(symbol) {
                Some(age) if age <= max_age => return Some(ticker),
                _ => debug!(symbol, "cached ticker is stale, refetching"),
            }
        }
        let mut connection = self.redis.clone();
        self.cache
            .fetch_ticker(&mut connection, &self.cfg.tickers_hash, symbol)
            .await
    }

    /// `SET processed_signal:<symbol>_<side> 1 NX EX <ttl>`.
    async fn acquire_redis_lock(&self, symbol: &str, side: Side) -> redis::RedisResult<bool> {
        let key = format!("processed_signal:{}_{}", symbol, side.as_str());
        let mut connection = self.redis.clone();
        let reply: Option<String> = redis::cmd("SET")
            .arg(&key)
            .arg(1u8)
            .arg("NX")
            .arg("EX")
            .arg(self.cfg.duplicate_signal_lock_sec)
            .query_async(&mut connection)
            .await?;
        Ok(reply.is_some())
    }

    fn release_redis_lock_opposite(&self, symbol: &str, side: Side) {
        let key = format!("processed_signal:{}_{}", symbol, side.opposite().as_str());
        let mut connection = self.redis.clone();
        tokio::spawn(async move {
            let result: redis::RedisResult<i64> =
                redis::cmd("DEL").arg(&key).query_async(&mut connection).await;
            if let Err(e) = result {
                debug!(error = %e, "could not clear opposite dedup key");
            }
        });
    }

    pub async fn shutdown(&self) {
        for account in &self.accounts {
            account.client.shutdown().await;
        }
        info!("trade executor stopped");
    }
}
