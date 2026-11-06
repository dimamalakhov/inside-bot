//! exchangeInfo snapshot -> per-symbol fixed-point filters.

use std::sync::Arc;

use anyhow::{Context, Result};
use dashmap::DashMap;
use reqwest::Client;
use tracing::{info, warn};

use crate::types::{ExchangeInfoResponse, SymbolFilters};

pub struct ExchangeInfoCache {
    filters: DashMap<String, SymbolFilters>,
}

impl ExchangeInfoCache {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            filters: DashMap::with_capacity(1024),
        })
    }

    /// Downloads and indexes the exchange rules. Retries transient failures so
    /// a flaky network cannot block startup.
    pub async fn load(client: &Client, base_url: &str) -> Result<Arc<Self>> {
        let url = format!("{}/fapi/v1/exchangeInfo", base_url.trim_end_matches('/'));
        let mut last_error = None;

        for attempt in 1..=3u32 {
            info!(url = %url, attempt, "loading exchange info");
            match client.get(&url).send().await {
                Ok(resp) => {
                    let resp = match resp.error_for_status() {
                        Ok(r) => r,
                        Err(e) => {
                            last_error = Some(anyhow::Error::new(e).context("exchangeInfo HTTP status"));
                            tokio::time::sleep(std::time::Duration::from_millis(300 * attempt as u64))
                                .await;
                            continue;
                        }
                    };
                    let parsed: ExchangeInfoResponse = resp
                        .json()
                        .await
                        .context("failed to parse exchangeInfo JSON")?;

                    let cache = Self::new();
                    let mut loaded = 0usize;
                    let mut skipped = 0usize;
                    for symbol_info in parsed.symbols {
                        if !symbol_info.status.eq_ignore_ascii_case("TRADING") {
                            skipped += 1;
                            continue;
                        }
                        match SymbolFilters::from_symbol_info(&symbol_info) {
                            Some(filters) => {
                                cache.filters.insert(symbol_info.symbol, filters);
                                loaded += 1;
                            }
                            None => {
                                warn!(symbol = %symbol_info.symbol, "unusable filters, symbol skipped");
                                skipped += 1;
                            }
                        }
                    }

                    if loaded == 0 {
                        return Err(anyhow::anyhow!(
                            "exchangeInfo returned no tradable symbols"
                        ));
                    }

                    info!(loaded, skipped, "exchange info ready");
                    return Ok(cache);
                }
                Err(e) => {
                    warn!(error = %e, attempt, "exchangeInfo request failed");
                    last_error = Some(anyhow::Error::new(e));
                    tokio::time::sleep(std::time::Duration::from_millis(300 * attempt as u64)).await;
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("exchangeInfo unavailable")))
    }

    #[inline]
    pub fn get(&self, symbol: &str) -> Option<SymbolFilters> {
        self.filters.get(symbol).map(|entry| *entry)
    }

    pub fn len(&self) -> usize {
        self.filters.len()
    }
}
