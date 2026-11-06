//! In-process market cache: last price per symbol and wallet balance per
//! account, refreshed in the background so the order path never blocks on
//! Redis.
//!
//! Balances live in a flat `Vec<AtomicU64>` indexed by account, so reading a
//! balance on the hot path is a single relaxed atomic load - no hashing, no
//! locking, no allocation.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use dashmap::DashMap;
use redis::AsyncCommands;
use tokio::sync::watch;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{error, info, warn};

use crate::config::Config;
use crate::math::f64_to_scaled;
use crate::types::TickerData;

#[derive(Clone, Copy)]
struct TickerEntry {
    data: TickerData,
    cached_at: Instant,
}

pub struct MarketCache {
    tickers: DashMap<String, TickerEntry>,
    balances: Vec<AtomicU64>,
    accounts: Vec<String>,
    index: HashMap<String, usize>,
}

impl MarketCache {
    pub fn new(accounts: Vec<String>) -> Arc<Self> {
        let mut index = HashMap::with_capacity(accounts.len());
        for (i, name) in accounts.iter().enumerate() {
            index.insert(name.clone(), i);
        }
        let balances = (0..accounts.len()).map(|_| AtomicU64::new(0)).collect();
        Arc::new(Self {
            tickers: DashMap::with_capacity(512),
            balances,
            accounts,
            index,
        })
    }

    #[inline]
    pub fn account_index(&self, name: &str) -> Option<usize> {
        self.index.get(name).copied()
    }

    pub fn accounts(&self) -> &[String] {
        &self.accounts
    }

    #[inline]
    pub fn balance_scaled(&self, idx: usize) -> u64 {
        self.balances
            .get(idx)
            .map(|v| v.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    #[inline]
    pub fn balance(&self, idx: usize) -> f64 {
        self.balance_scaled(idx) as f64 / crate::math::SCALE as f64
    }

    pub fn set_balance(&self, idx: usize, scaled: u64) {
        if let Some(slot) = self.balances.get(idx) {
            slot.store(scaled, Ordering::Relaxed);
        }
    }

    /// Last known ticker (copy - no allocation, no lock held).
    #[inline]
    pub fn ticker(&self, symbol: &str) -> Option<TickerData> {
        self.tickers.get(symbol).and_then(|e| {
            if e.data.last > 0.0 {
                Some(e.data)
            } else {
                None
            }
        })
    }

    /// Age of the cached ticker, used to detect a stale price before trading.
    #[inline]
    pub fn ticker_age(&self, symbol: &str) -> Option<Duration> {
        self.tickers.get(symbol).map(|e| e.cached_at.elapsed())
    }

    pub fn upsert_ticker(&self, symbol: String, data: TickerData) {
        self.tickers.insert(
            symbol,
            TickerEntry {
                data,
                cached_at: Instant::now(),
            },
        );
    }

    pub fn tickers_len(&self) -> usize {
        self.tickers.len()
    }

    /// Fetches one ticker directly from Redis (used when the cache is stale).
    pub async fn fetch_ticker(
        &self,
        con: &mut redis::aio::MultiplexedConnection,
        hash: &str,
        symbol: &str,
    ) -> Option<TickerData> {
        let raw: Option<String> = con.hget(hash, symbol).await.ok()?;
        let data: TickerData = serde_json::from_str(raw.as_deref()?).ok()?;
        if data.last <= 0.0 {
            return None;
        }
        self.upsert_ticker(symbol.to_string(), data);
        Some(data)
    }
}

async fn open_connection(url: &str) -> Option<redis::aio::Connection> {
    let client = match redis::Client::open(url) {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "invalid Redis URL");
            return None;
        }
    };
    match client.get_async_connection().await {
        Ok(c) => Some(c),
        Err(e) => {
            warn!(error = %e, "Redis connection failed");
            None
        }
    }
}

/// Keeps `futures_tickers` mirrored into the cache.
pub async fn tickers_loop(
    cache: Arc<MarketCache>,
    cfg: Arc<Config>,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut con: Option<redis::aio::Connection> = None;
    let mut ticker = interval(Duration::from_millis(
        cfg.ticker_update_interval_ms.max(10),
    ));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    info!(
        interval_ms = cfg.ticker_update_interval_ms,
        hash = %cfg.tickers_hash,
        "ticker cache updater started"
    );

    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
                continue;
            }
        }

        if con.is_none() {
            con = open_connection(&cfg.redis_url).await;
            if con.is_none() {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        }

        let conn = con.as_mut().expect("connection checked above");
        let raw: redis::RedisResult<Vec<(String, String)>> =
            conn.hgetall(&cfg.tickers_hash).await;

        match raw {
            Ok(entries) => {
                let mut updated = 0usize;
                for (symbol, json) in entries {
                    match serde_json::from_str::<TickerData>(&json) {
                        Ok(data) if data.last > 0.0 => {
                            cache.upsert_ticker(symbol, data);
                            updated += 1;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            warn!(symbol = %symbol, error = %e, "bad ticker payload");
                        }
                    }
                }
                if updated > 0 {
                    tracing::trace!(updated, total = cache.tickers_len(), "tickers refreshed");
                }
            }
            Err(e) => {
                warn!(error = %e, "HGETALL tickers failed, reconnecting");
                con = None;
            }
        }
    }

    info!("ticker cache updater stopped");
    Ok(())
}

/// Keeps wallet balances mirrored into the cache.
///
/// Primary source is `balance:<owner>` (written by `balance_streamer.py`);
/// the legacy `futures_balances` hash is used as a fallback.
pub async fn balances_loop(
    cache: Arc<MarketCache>,
    cfg: Arc<Config>,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let keys: Vec<String> = cache.accounts().iter().map(|a| cfg.balance_key(a)).collect();

    let mut con: Option<redis::aio::Connection> = None;
    let mut ticker = interval(Duration::from_millis(
        cfg.balance_update_interval_ms.max(50),
    ));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    info!(
        interval_ms = cfg.balance_update_interval_ms,
        accounts = keys.len(),
        "balance cache updater started"
    );

    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
                continue;
            }
        }

        if con.is_none() {
            con = open_connection(&cfg.redis_url).await;
            if con.is_none() {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        }

        let conn = con.as_mut().expect("connection checked above");
        if keys.is_empty() {
            continue;
        }

        let values: redis::RedisResult<Vec<Option<String>>> = redis::cmd("MGET")
            .arg(&keys)
            .query_async(conn)
            .await;

        let mut seen_any = false;
        match values {
            Ok(values) => {
                for (idx, value) in values.into_iter().enumerate() {
                    if let Some(raw) = value {
                        if let Ok(balance) = raw.parse::<f64>() {
                            if balance > 0.0 {
                                cache.set_balance(idx, f64_to_scaled(balance));
                                seen_any = true;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "MGET balances failed, reconnecting");
                con = None;
                continue;
            }
        }

        if !seen_any {
            // Legacy layout: single hash of account -> balance.
            let legacy: redis::RedisResult<Vec<(String, String)>> =
                conn.hgetall(&cfg.balances_hash).await;
            if let Ok(entries) = legacy {
                for (owner, raw) in entries {
                    if let (Some(idx), Ok(balance)) =
                        (cache.account_index(&owner), raw.parse::<f64>())
                    {
                        if balance > 0.0 {
                            cache.set_balance(idx, f64_to_scaled(balance));
                        }
                    }
                }
            }
        }
    }

    info!("balance cache updater stopped");
    Ok(())
}

/// Waits until at least one balance is known (or the timeout expires) so the
/// engine does not start by silently skipping every account.
pub async fn wait_for_balances(cache: &Arc<MarketCache>, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if (0..cache.accounts().len()).any(|i| cache.balance_scaled(i) > 0) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Convenience wrapper used by `main` to bootstrap the first connection with a
/// clear error message instead of a silent retry loop.
pub async fn check_redis(url: &str) -> anyhow::Result<()> {
    let client = redis::Client::open(url).context("invalid Redis URL")?;
    let mut con = client
        .get_async_connection()
        .await
        .context("cannot connect to Redis")?;
    let pong: String = redis::cmd("PING")
        .query_async(&mut con)
        .await
        .context("Redis PING failed")?;
    anyhow::ensure!(pong == "PONG", "unexpected Redis reply: {pong}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_reads_and_writes() {
        let cache = MarketCache::new(vec!["alice".into(), "bob".into()]);
        assert_eq!(cache.account_index("alice"), Some(0));
        assert_eq!(cache.account_index("bob"), Some(1));
        assert_eq!(cache.account_index("carol"), None);

        cache.set_balance(0, 1_000 * crate::math::SCALE);
        assert_eq!(cache.balance_scaled(0), 1_000 * crate::math::SCALE);
        assert!((cache.balance(0) - 1_000.0).abs() < 1e-9);
        assert_eq!(cache.balance_scaled(1), 0);
        assert_eq!(cache.balance_scaled(99), 0);

        cache.upsert_ticker(
            "BTCUSDT".into(),
            TickerData {
                last: 50_000.0,
                min_qty: 0.001,
                leverage: 10,
                timestamp: 1.0,
            },
        );
        let t = cache.ticker("BTCUSDT").unwrap();
        assert_eq!(t.last, 50_000.0);
        assert_eq!(t.leverage, 10);
        assert!(cache.ticker("ETHUSDT").is_none());
        assert!(cache.ticker_age("BTCUSDT").is_some());
    }

    #[test]
    fn zero_price_ticker_is_invisible() {
        let cache = MarketCache::new(vec![]);
        cache.upsert_ticker("XXXUSDT".into(), TickerData::default());
        assert!(cache.ticker("XXXUSDT").is_none());
    }
}
