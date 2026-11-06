//! Shared domain types: signals, tickers, exchange filters, order payloads.

use serde::{Deserialize, Serialize};

/// Order side. Kept as a tiny `Copy` enum instead of `String` so the hot path
/// never allocates or re-parses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        }
    }

    #[inline]
    pub const fn opposite(self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }

    /// Accepts "BUY"/"SELL", "long"/"short", "bullish"/"bearish" (any case).
    #[inline]
    pub fn parse(value: &str) -> Option<Side> {
        let v = value.trim();
        if v.eq_ignore_ascii_case("buy")
            || v.eq_ignore_ascii_case("long")
            || v.eq_ignore_ascii_case("bullish")
        {
            Some(Side::Buy)
        } else if v.eq_ignore_ascii_case("sell")
            || v.eq_ignore_ascii_case("short")
            || v.eq_ignore_ascii_case("bearish")
        {
            Some(Side::Sell)
        } else {
            None
        }
    }
}

impl std::fmt::Display for Side {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Ticker snapshot published by `price_streamer.py` into `futures_tickers`.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, Default)]
pub struct TickerData {
    #[serde(default)]
    pub last: f64,
    #[serde(default)]
    pub min_qty: f64,
    #[serde(default)]
    pub leverage: u32,
    #[serde(default)]
    pub timestamp: f64,
}

// ---------------------------------------------------------------------------
// exchangeInfo
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ExchangeInfoResponse {
    #[serde(default)]
    pub symbols: Vec<SymbolInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SymbolInfo {
    #[serde(default)]
    pub symbol: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub filters: Vec<Filter>,
}

// Several filter fields are parsed only to keep the payload self-documenting;
// they are not needed by the order path.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "filterType")]
pub enum Filter {
    #[serde(rename = "LOT_SIZE")]
    LotSize {
        #[serde(rename = "minQty", default)]
        min_qty: String,
        #[serde(rename = "maxQty", default)]
        max_qty: String,
        #[serde(rename = "stepSize", default)]
        step_size: String,
    },
    #[serde(rename = "PRICE_FILTER")]
    PriceFilter {
        #[serde(rename = "minPrice", default)]
        min_price: String,
        #[serde(rename = "maxPrice", default)]
        max_price: String,
        #[serde(rename = "tickSize", default)]
        tick_size: String,
    },
    #[serde(rename = "MIN_NOTIONAL")]
    MinNotional {
        #[serde(rename = "notional", default)]
        notional: String,
    },
    #[serde(rename = "NOTIONAL")]
    Notional {
        #[serde(rename = "notional", default)]
        notional: String,
    },
    #[serde(other)]
    Other,
}

/// Filters pre-converted to fixed-point integers (`1e8` scale) so that the
/// order path uses pure integer arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SymbolFilters {
    /// `stepSize * 1e8`
    pub step_size: u64,
    /// `minQty * 1e8`
    pub min_qty: u64,
    /// `tickSize * 1e8`
    pub tick_size: u64,
    /// `MIN_NOTIONAL * 1e8` (USDT)
    pub min_notional: u64,
    /// Decimals to print for quantity (derived from `step_size`)
    pub qty_decimals: u8,
    /// Decimals to print for price (derived from `tick_size`)
    pub price_decimals: u8,
}

impl SymbolFilters {
    /// Fixed-point scale used for quantities, notional and prices.
    pub const SCALE: u64 = 100_000_000;
    pub const SCALE_DECIMALS: u8 = 8;

    /// Parses `"0.001"` into `100_000` when `decimals == 8`.
    /// Extra fractional digits beyond the scale are truncated (never rounded up),
    /// which is the safe direction for both price and quantity.
    pub fn parse_scaled(input: &str, decimals: u8) -> u64 {
        let bytes = input.trim().as_bytes();
        let mut int_part: u64 = 0;
        let mut frac: u64 = 0;
        let mut frac_digits: u8 = 0;
        let mut seen_dot = false;
        let mut seen_digit = false;

        for &b in bytes {
            match b {
                b'0'..=b'9' => {
                    seen_digit = true;
                    let d = (b - b'0') as u64;
                    if !seen_dot {
                        int_part = int_part.saturating_mul(10).saturating_add(d);
                    } else if frac_digits < decimals {
                        frac = frac.saturating_mul(10).saturating_add(d);
                        frac_digits += 1;
                    }
                }
                b'.' => {
                    if seen_dot {
                        break;
                    }
                    seen_dot = true;
                }
                _ => break,
            }
        }

        if !seen_digit {
            return 0;
        }

        let pow = pow10(decimals);
        let mut value = int_part.saturating_mul(pow);
        if seen_dot && frac_digits > 0 {
            value = value.saturating_add(frac.saturating_mul(pow10(decimals - frac_digits)));
        }
        value
    }

    /// Builds filters from an exchangeInfo entry.
    ///
    /// Returns `None` only when the symbol cannot be traded safely at all
    /// (no positive `stepSize`/`tickSize`). A missing `MIN_NOTIONAL` falls back
    /// to a conservative 5 USDT, matching Binance's default for linear futures.
    pub fn from_symbol_info(info: &SymbolInfo) -> Option<Self> {
        let mut step_size = 0u64;
        let mut min_qty = 0u64;
        let mut tick_size = 0u64;
        let mut min_notional = 0u64;

        for filter in &info.filters {
            match filter {
                Filter::LotSize {
                    step_size: s,
                    min_qty: m,
                    ..
                } => {
                    step_size = Self::parse_scaled(s, Self::SCALE_DECIMALS);
                    min_qty = Self::parse_scaled(m, Self::SCALE_DECIMALS);
                }
                Filter::PriceFilter { tick_size: t, .. } => {
                    tick_size = Self::parse_scaled(t, Self::SCALE_DECIMALS);
                }
                Filter::MinNotional { notional } | Filter::Notional { notional } => {
                    let parsed = Self::parse_scaled(notional, Self::SCALE_DECIMALS);
                    if parsed > min_notional {
                        min_notional = parsed;
                    }
                }
                Filter::Other => {}
            }
        }

        if step_size == 0 || tick_size == 0 {
            return None;
        }
        if min_notional == 0 {
            min_notional = 5 * Self::SCALE;
        }

        Some(Self {
            step_size,
            min_qty,
            tick_size,
            min_notional,
            qty_decimals: decimals_of(step_size),
            price_decimals: decimals_of(tick_size),
        })
    }
}

/// `10^n` for `n <= 19`, saturating instead of overflowing.
#[inline]
pub fn pow10(n: u8) -> u64 {
    if n >= 20 {
        return u64::MAX;
    }
    10u64.pow(n as u32)
}

/// Number of decimal places encoded in a fixed-point (1e8) value.
#[inline]
fn decimals_of(scaled: u64) -> u8 {
    let mut s = scaled;
    let mut trailing_zeros = 0u8;
    while s > 1 && s % 10 == 0 {
        s /= 10;
        trailing_zeros += 1;
    }
    SymbolFilters::SCALE_DECIMALS.saturating_sub(trailing_zeros)
}

// ---------------------------------------------------------------------------
// Order payloads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrderResponse {
    #[serde(rename = "orderId", default)]
    pub order_id: u64,
    #[serde(default)]
    pub symbol: String,
    #[serde(default)]
    pub side: String,
    #[serde(rename = "type", default)]
    pub order_type: String,
    #[serde(default)]
    pub status: String,
    #[serde(rename = "origQty", default)]
    pub orig_qty: String,
    #[serde(rename = "executedQty", default)]
    pub executed_qty: String,
    #[serde(rename = "avgPrice", default)]
    pub avg_price: String,
    #[serde(rename = "cumQuote", default)]
    pub cum_quote: String,
    #[serde(rename = "timeInForce", default)]
    pub time_in_force: String,
    #[serde(rename = "reduceOnly", default)]
    pub reduce_only: bool,
    #[serde(rename = "clientOrderId", default)]
    pub client_order_id: String,
}

impl OrderResponse {
    /// `(avg_price_scaled, executed_qty_scaled)` if the order has any fill.
    /// `avgPrice` is empty for `ACK` responses, in which case it is derived
    /// from `cumQuote / executedQty` - still without an extra round-trip.
    pub fn fill(&self) -> Option<(u64, u64)> {
        let qty = parse_positive(&self.executed_qty)?;
        let avg = match parse_positive(&self.avg_price) {
            Some(v) => v,
            None => {
                let quote = parse_positive(&self.cum_quote)?;
                quote / qty
            }
        };
        Some((crate::math::price_to_scaled(avg), crate::math::f64_to_scaled(qty)))
    }

    pub fn is_filled(&self) -> bool {
        self.status.eq_ignore_ascii_case("FILLED")
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrderStatus {
    #[serde(rename = "orderId", default)]
    pub order_id: u64,
    #[serde(default)]
    pub symbol: String,
    #[serde(default)]
    pub status: String,
    #[serde(rename = "executedQty", default)]
    pub executed_qty: String,
    #[serde(rename = "avgPrice", default)]
    pub avg_price: String,
    #[serde(rename = "cumQuote", default)]
    pub cum_quote: String,
}

impl OrderStatus {
    pub fn fill(&self) -> Option<(u64, u64)> {
        let qty = parse_positive(&self.executed_qty)?;
        let avg = match parse_positive(&self.avg_price) {
            Some(v) => v,
            None => parse_positive(&self.cum_quote)? / qty,
        };
        Some((crate::math::price_to_scaled(avg), crate::math::f64_to_scaled(qty)))
    }
}

#[inline]
fn parse_positive(raw: &str) -> Option<f64> {
    if raw.is_empty() {
        return None;
    }
    match raw.trim().parse::<f64>() {
        Ok(v) if v > 0.0 && v.is_finite() => Some(v),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Config payloads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AccountConfig {
    pub owner: String,
    pub public: String,
    pub private: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ApiKeysConfig {
    #[serde(default)]
    pub founders: Vec<AccountConfig>,
    #[serde(default)]
    pub friends: Vec<AccountConfig>,
    #[serde(default)]
    pub clients: Vec<AccountConfig>,
    #[serde(default)]
    pub support: Vec<AccountConfig>,
}

impl ApiKeysConfig {
    /// Trading accounts (support accounts are price feeders only) with
    /// duplicate owners removed - a duplicated owner would otherwise send the
    /// same order twice.
    pub fn trading_accounts(&self) -> Vec<AccountConfig> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for acc in self
            .founders
            .iter()
            .chain(self.friends.iter())
            .chain(self.clients.iter())
        {
            if seen.insert(acc.owner.clone()) {
                out.push(acc.clone());
            }
        }
        out
    }
}

/// Take-profit / stop-loss configuration, in basis points.
#[derive(Debug, Clone, Copy)]
pub struct TpSlParams {
    pub tp_bps: u64,
    pub sl_bps: u64,
    pub sl_limit_slippage_bps: u64,
}

impl Default for TpSlParams {
    fn default() -> Self {
        Self {
            tp_bps: 30,                 // 0.30%
            sl_bps: 30,                 // 0.30%
            sl_limit_slippage_bps: 50,  // 0.50%
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_scaled_handles_typical_sizes() {        assert_eq!(SymbolFilters::parse_scaled("0.001", 8), 100_000);
        assert_eq!(SymbolFilters::parse_scaled("1", 8), 100_000_000);
        assert_eq!(SymbolFilters::parse_scaled("1.5", 8), 150_000_000);
        assert_eq!(SymbolFilters::parse_scaled("0.00000001", 8), 1);
        assert_eq!(SymbolFilters::parse_scaled("1000", 8), 100_000_000_000);
        assert_eq!(SymbolFilters::parse_scaled("0.123456789", 8), 12_345_678);
        assert_eq!(SymbolFilters::parse_scaled("", 8), 0);
    }

    #[test]
    fn decimals_derived_from_step() {
        assert_eq!(decimals_of(100_000), 3); // 0.001
        assert_eq!(decimals_of(100_000_000), 0); // 1
        assert_eq!(decimals_of(10_000_000), 1); // 0.1
        assert_eq!(decimals_of(1), 8); // 0.00000001
    }

    #[test]
    fn build_filters_from_symbol_info() {
        let info = SymbolInfo {
            symbol: "BTCUSDT".into(),
            status: "TRADING".into(),
            filters: vec![
                Filter::LotSize {
                    min_qty: "0.001".into(),
                    max_qty: "1000".into(),
                    step_size: "0.001".into(),
                },
                Filter::PriceFilter {
                    min_price: "0.01".into(),
                    max_price: "1000000".into(),
                    tick_size: "0.1".into(),
                },
                Filter::MinNotional {
                    notional: "5".into(),
                },
            ],
        };
        let f = SymbolFilters::from_symbol_info(&info).unwrap();
        assert_eq!(f.step_size, 100_000);
        assert_eq!(f.min_qty, 100_000);
        assert_eq!(f.tick_size, 10_000_000);
        assert_eq!(f.min_notional, 500_000_000);
        assert_eq!(f.qty_decimals, 3);
        assert_eq!(f.price_decimals, 1);
    }

    #[test]
    fn missing_min_notional_falls_back() {
        let info = SymbolInfo {
            symbol: "XYZUSDT".into(),
            status: "TRADING".into(),
            filters: vec![
                Filter::LotSize {
                    min_qty: "1".into(),
                    max_qty: "1000".into(),
                    step_size: "1".into(),
                },
                Filter::PriceFilter {
                    min_price: "0.01".into(),
                    max_price: "1000".into(),
                    tick_size: "0.01".into(),
                },
            ],
        };
        let f = SymbolFilters::from_symbol_info(&info).unwrap();
        assert_eq!(f.min_notional, 5 * SymbolFilters::SCALE);
    }

    #[test]
    fn side_parsing() {
        assert_eq!(Side::parse("BUY"), Some(Side::Buy));
        assert_eq!(Side::parse("sell"), Some(Side::Sell));
        assert_eq!(Side::parse("Bullish"), Some(Side::Buy));
        assert_eq!(Side::parse("short"), Some(Side::Sell));
        assert_eq!(Side::parse("nope"), None);
        assert_eq!(Side::Buy.opposite(), Side::Sell);
    }

    #[test]
    fn fill_derives_avg_price_from_cum_quote() {
        let resp = OrderResponse {
            order_id: 7,
            symbol: "BTCUSDT".into(),
            side: "BUY".into(),
            order_type: "MARKET".into(),
            status: "FILLED".into(),
            orig_qty: "0.06".into(),
            executed_qty: "0.06".into(),
            avg_price: String::new(),
            cum_quote: "3000".into(),
            time_in_force: "GTC".into(),
            reduce_only: false,
            client_order_id: String::new(),
        };
        let (price, qty) = resp.fill().unwrap();
        assert_eq!(price, 50_000 * 100_000_000);
        assert_eq!(qty, 6_000_000);
    }

    /// The exact shape `price_streamer.py` writes into `futures_tickers`.
    #[test]
    fn parses_price_streamer_ticker_payload() {
        let raw = r#"{"last":62000.5,"min_qty":0.001,"leverage":10,"timestamp":1730000000.25}"#;
        let ticker: TickerData = serde_json::from_str(raw).unwrap();
        assert!((ticker.last - 62_000.5).abs() < 1e-9);
        assert!((ticker.min_qty - 0.001).abs() < 1e-12);
        assert_eq!(ticker.leverage, 10);
        assert!((ticker.timestamp - 1_730_000_000.25).abs() < 1e-6);

        // A partial payload must not abort the whole cache refresh.
        let ticker: TickerData = serde_json::from_str(r#"{"last":1.5}"#).unwrap();
        assert_eq!(ticker.last, 1.5);
        assert_eq!(ticker.leverage, 0);
    }

    /// The exact shape of `config/api_keys.json`; `support` must never trade.
    #[test]
    fn parses_api_keys_config_and_excludes_support() {
        let raw = r#"{
            "founders": [{"owner":"alice","public":"P1","private":"S1"}],
            "friends":  [],
            "clients":  [{"owner":"bob","public":"P2","private":"S2"}],
            "support":  [{"owner":"feeder","public":"P3","private":"S3"}]
        }"#;
        let config: ApiKeysConfig = serde_json::from_str(raw).unwrap();
        let trading = config.trading_accounts();
        assert_eq!(trading.len(), 2);
        assert_eq!(trading[0].owner, "alice");
        assert_eq!(trading[1].owner, "bob");
        assert!(trading.iter().all(|account| account.owner != "feeder"));
    }

    /// A duplicated owner would otherwise place the same order twice.
    #[test]
    fn duplicate_owners_are_collapsed() {
        let raw = r#"{
            "founders": [{"owner":"dup","public":"P","private":"S"}],
            "friends":  [{"owner":"dup","public":"P","private":"S"}],
            "clients":  [{"owner":"dup","public":"P","private":"S"}]
        }"#;
        let config: ApiKeysConfig = serde_json::from_str(raw).unwrap();
        assert_eq!(config.trading_accounts().len(), 1);
    }
}
