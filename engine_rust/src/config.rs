//! Environment configuration.
//!
//! Deliberately tolerant: every value has a production-safe default and no
//! variable is mandatory, so the engine behaves the same whether it is fed by
//! the legacy `.env` (percent/fraction style, `REDIS_QUEUE`) or by the newer
//! bps-style variables.

use std::env;

fn var(name: &str) -> Option<String> {
    match env::var(name) {
        Ok(v) if !v.trim().is_empty() => Some(v.trim().to_string()),
        _ => None,
    }
}

fn var_or(name: &str, default: &str) -> String {
    var(name).unwrap_or_else(|| default.to_string())
}

fn parse_u64(name: &str, default: u64) -> u64 {
    match var(name) {
        Some(raw) => raw.parse::<u64>().unwrap_or_else(|_| {
            tracing::warn!(var = name, value = %raw, "invalid number, using default");
            default
        }),
        None => default,
    }
}

fn parse_f64(name: &str) -> Option<f64> {
    var(name).and_then(|raw| raw.parse::<f64>().ok())
}

fn parse_bool(name: &str, default: bool) -> bool {
    match var(name) {
        Some(raw) => matches!(
            raw.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on" | "y"
        ),
        None => default,
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    // ---- Redis -----------------------------------------------------------
    pub redis_url: String,
    pub queue_name: String,
    pub tickers_hash: String,
    pub balances_hash: String,
    pub balance_key_prefix: String,
    pub redis_dedup: bool,
    pub duplicate_signal_lock_sec: u64,

    // ---- Binance ---------------------------------------------------------
    pub testnet: bool,
    pub ws_api_url: String,
    pub rest_base_url: String,
    pub api_keys_path: String,
    pub recv_window_ms: u64,
    pub order_timeout_ms: u64,

    // ---- Trading ---------------------------------------------------------
    pub trade_allocation_bps: u64,
    pub tp_bps: u64,
    pub sl_bps: u64,
    pub sl_limit_slippage_bps: u64,
    pub dry_run: bool,

    // ---- Bridge (EU -> Tokyo UDP) ----------------------------------------
    pub bridge_port: u16,
    pub bridge_forward_redis: bool,

    // ---- Cache -----------------------------------------------------------
    pub ticker_update_interval_ms: u64,
    pub balance_update_interval_ms: u64,
    pub max_ticker_age_ms: u64,
    pub fill_poll_attempts: u32,
    pub fill_poll_interval_ms: u64,

    pub mode: String,
}

impl Config {
    pub fn from_env() -> Self {
        let redis_url = match var("REDIS_URL") {
            Some(url) => url,
            None => {
                let host = var_or("REDIS_HOST", "127.0.0.1");
                let port = var_or("REDIS_PORT", "6379");
                let db = var_or("REDIS_DB", "0");
                format!("redis://{}:{}/{}", host, port, db)
            }
        };

        let testnet = parse_bool("BINANCE_TESTNET", parse_bool("TESTNET", false));

        // ---- allocation: fraction (0.3) or explicit bps (3000) ----------
        let trade_allocation_bps = if let Some(bps) = parse_f64("TRADE_ALLOCATION_BPS") {
            bps.round().max(0.0) as u64
        } else {
            let fraction = parse_f64("TRADE_ALLOCATION").unwrap_or(0.3);
            // Accept both 0.3 and 30 for "30%".
            let fraction = if fraction > 1.0 { fraction / 100.0 } else { fraction };
            (fraction * 10_000.0).round().clamp(1.0, 10_000.0) as u64
        };

        // ---- take profit / stop loss: percent (0.3%) or bps (30) --------
        let tp_bps = if let Some(bps) = parse_f64("TP_PERCENT_BPS") {
            bps.round().max(0.0) as u64
        } else {
            let percent = parse_f64("TP_PERCENT").unwrap_or(0.3);
            (percent * 100.0).round().clamp(1.0, 5_000.0) as u64
        };
        let sl_bps = if let Some(bps) = parse_f64("SL_PERCENT_BPS") {
            bps.round().max(0.0) as u64
        } else {
            let percent = parse_f64("SL_PERCENT").unwrap_or(0.3);
            (percent * 100.0).round().clamp(1.0, 5_000.0) as u64
        };
        let sl_limit_slippage_bps = if let Some(bps) = parse_f64("SL_LIMIT_SLIPPAGE_BPS") {
            bps.round().max(0.0) as u64
        } else {
            let percent = parse_f64("SL_LIMIT_SLIPPAGE_PERCENT").unwrap_or(0.5);
            (percent * 100.0).round().clamp(1.0, 2_000.0) as u64
        };

        let ws_api_url = var("BINANCE_WS_API_URL").unwrap_or_else(|| {
            if testnet {
                "wss://testnet.binancefuture.com/ws-fapi/v1".to_string()
            } else {
                "wss://ws-fapi.binance.com/ws-fapi/v1".to_string()
            }
        });
        let rest_base_url = var("BINANCE_REST_URL").unwrap_or_else(|| {
            if testnet {
                "https://testnet.binancefuture.com".to_string()
            } else {
                "https://fapi.binance.com".to_string()
            }
        });

        let bridge_port = var("BRIDGE_PORT")
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(5005);

        Self {
            redis_url,
            queue_name: var("REDIS_QUEUE")
                .or_else(|| var("REDIS_QUEUE_NAME"))
                .unwrap_or_else(|| "trade_queue".to_string()),
            tickers_hash: var_or("REDIS_TICKERS_HASH", "futures_tickers"),
            balances_hash: var_or("REDIS_BALANCES_HASH", "futures_balances"),
            balance_key_prefix: var_or("BALANCE_KEY_PREFIX", "balance:"),
            redis_dedup: parse_bool("REDIS_DEDUP", true),
            duplicate_signal_lock_sec: parse_u64("DUPLICATE_SIGNAL_LOCK_SEC", 60),

            testnet,
            ws_api_url,
            rest_base_url,
            api_keys_path: var_or("API_KEYS_FILE", "config/api_keys.json"),
            recv_window_ms: parse_u64("RECV_WINDOW_MS", 5_000),
            order_timeout_ms: parse_u64("ORDER_TIMEOUT_MS", 3_000),

            trade_allocation_bps,
            tp_bps,
            sl_bps,
            sl_limit_slippage_bps,
            dry_run: parse_bool("TRADE_DRY_RUN", false),

            bridge_port,
            bridge_forward_redis: parse_bool("BRIDGE_FORWARD_REDIS", false),

            ticker_update_interval_ms: parse_u64("TICKER_UPDATE_INTERVAL_MS", 100),
            balance_update_interval_ms: parse_u64("BALANCE_UPDATE_INTERVAL_MS", 500),
            max_ticker_age_ms: parse_u64("MAX_TICKER_AGE_MS", 2_000),
            fill_poll_attempts: parse_u64("FILL_POLL_ATTEMPTS", 10) as u32,
            fill_poll_interval_ms: parse_u64("FILL_POLL_INTERVAL_MS", 150),

            mode: var_or("TRADE_MODE", if testnet { "TEST" } else { "PROD" }),
        }
    }

    /// Human-readable summary printed on startup (never contains secrets).
    pub fn summary(&self) -> String {
        format!(
            "mode={} testnet={} dry_run={} | allocation={}bps tp={}bps sl={}bps sl_slip={}bps | \
             redis={} queue={} tickers={} balances={}{} | ws={} | bridge_udp={}",
            self.mode,
            self.testnet,
            self.dry_run,
            self.trade_allocation_bps,
            self.tp_bps,
            self.sl_bps,
            self.sl_limit_slippage_bps,
            self.redis_url,
            self.queue_name,
            self.tickers_hash,
            self.balance_key_prefix,
            "*",
            self.ws_api_url,
            self.bridge_port,
        )
    }

    pub fn balance_key(&self, owner: &str) -> String {
        format!("{}{}", self.balance_key_prefix, owner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        // These only exercise the parsing helpers, not the environment.
        assert!(parse_bool("DSH_TEST_NOT_SET_FLAG", true));
        assert!(!parse_bool("DSH_TEST_NOT_SET_FLAG", false));
        assert_eq!(parse_u64("DSH_TEST_NOT_SET_NUM", 42), 42);
    }

    #[test]
    fn balance_key_uses_prefix() {
        let cfg = Config {
            redis_url: String::new(),
            queue_name: "trade_queue".into(),
            tickers_hash: "futures_tickers".into(),
            balances_hash: "futures_balances".into(),
            balance_key_prefix: "balance:".into(),
            redis_dedup: true,
            duplicate_signal_lock_sec: 60,
            testnet: false,
            ws_api_url: String::new(),
            rest_base_url: String::new(),
            api_keys_path: String::new(),
            recv_window_ms: 5_000,
            order_timeout_ms: 3_000,
            trade_allocation_bps: 3_000,
            tp_bps: 30,
            sl_bps: 30,
            sl_limit_slippage_bps: 50,
            dry_run: false,
            bridge_port: 5005,
            bridge_forward_redis: false,
            ticker_update_interval_ms: 100,
            balance_update_interval_ms: 500,
            max_ticker_age_ms: 2_000,
            fill_poll_attempts: 10,
            fill_poll_interval_ms: 150,
            mode: "TEST".into(),
        };
        assert_eq!(cfg.balance_key("alice"), "balance:alice");
    }
}
