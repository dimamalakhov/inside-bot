//! Signal parsing.
//!
//! Two wire formats reach the engine and both must work unchanged:
//!
//! 1. `telegram_listener.py` pushes the **raw Telegram text**
//!    (`"#BTC Bullish ..."`) into `trade_queue`.
//! 2. `bridge_client.py` / `bridge.rs` send **JSON** (`{"symbol":"BTCUSDT","side":"BUY"}`).
//!
//! Parsing is allocation-light: a single `String` for the symbol plus a `Copy`
//! side, no regex engine in the hot path.

use crate::types::Side;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSignal {
    pub symbol: String,
    pub side: Side,
}

/// Parses either wire format. Returns `None` for noise (non-signal messages).
pub fn parse(raw: &str) -> Option<ParsedSignal> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('{') {
        parse_json(trimmed)
    } else {
        parse_text(trimmed)
    }
}

fn parse_json(raw: &str) -> Option<ParsedSignal> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let symbol = value.get("symbol")?.as_str()?;
    let side = value.get("side")?.as_str()?;
    let side = Side::parse(side)?;
    Some(ParsedSignal {
        symbol: normalize_symbol(symbol)?,
        side,
    })
}

fn parse_text(raw: &str) -> Option<ParsedSignal> {
    let bytes = raw.as_bytes();
    let mut search_from = 0usize;

    while let Some(rel) = find_byte(&bytes[search_from..], b'#') {
        let hash = search_from + rel;
        let coin_start = hash + 1;
        let mut coin_end = coin_start;
        while coin_end < bytes.len() && (bytes[coin_end].is_ascii_alphanumeric()) {
            coin_end += 1;
        }
        if coin_end > coin_start {
            let coin = &raw[coin_start..coin_end];
            if let (Some(symbol), Some(side)) =
                (normalize_symbol(coin), side_in(&bytes[hash..]))
            {
                return Some(ParsedSignal { symbol, side });
            }
        }
        search_from = coin_end.max(hash + 1);
    }

    None
}

/// Finds the first occurrence of `"bullish"` or `"bearish"` (ASCII
/// case-insensitive). `"bullish"` wins when both appear.
fn side_in(haystack: &[u8]) -> Option<Side> {
    if find_ci(haystack, b"bullish").is_some() {
        return Some(Side::Buy);
    }
    if find_ci(haystack, b"bearish").is_some() {
        return Some(Side::Sell);
    }
    None
}

fn find_byte(haystack: &[u8], needle: u8) -> Option<usize> {
    haystack.iter().position(|b| *b == needle)
}

fn find_ci(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let first = needle[0].to_ascii_lowercase();
    for i in 0..=(haystack.len() - needle.len()) {
        if haystack[i].to_ascii_lowercase() != first {
            continue;
        }
        if haystack[i..i + needle.len()]
            .iter()
            .zip(needle.iter())
            .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
        {
            return Some(i);
        }
    }
    None
}

/// Uppercases, strips `#`/`$` and appends `USDT` when the quote asset is missing.
pub fn normalize_symbol(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .trim()
        .trim_start_matches(['#', '$'])
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase();

    if cleaned.is_empty() || cleaned.len() > 20 {
        return None;
    }
    // Reject pure garbage such as a lone digit run.
    if cleaned.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    const QUOTES: [&str; 4] = ["USDT", "USDC", "BUSD", "FDUSD"];
    if QUOTES.iter().any(|q| cleaned.ends_with(q)) {
        Some(cleaned)
    } else {
        Some(format!("{}USDT", cleaned))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_telegram_text() {
        let s = parse("#BTC Bullish setup, entry now 🚀").unwrap();
        assert_eq!(s.symbol, "BTCUSDT");
        assert_eq!(s.side, Side::Buy);

        let s = parse("#eth BEARISH").unwrap();
        assert_eq!(s.symbol, "ETHUSDT");
        assert_eq!(s.side, Side::Sell);
    }

    #[test]
    fn parses_telegram_text_with_pair_suffix() {
        let s = parse("#BTC/USDT Bullish").unwrap();
        assert_eq!(s.symbol, "BTCUSDT");
        let s = parse("#1000SHIBUSDT bearish").unwrap();
        assert_eq!(s.symbol, "1000SHIBUSDT");
    }

    #[test]
    fn parses_json_payloads() {
        let s = parse(r#"{"symbol":"BTCUSDT","side":"BUY","source":"bridge_eu"}"#).unwrap();
        assert_eq!(s.symbol, "BTCUSDT");
        assert_eq!(s.side, Side::Buy);

        let s = parse(r#"{"symbol":"eth","side":"sell"}"#).unwrap();
        assert_eq!(s.symbol, "ETHUSDT");
        assert_eq!(s.side, Side::Sell);
    }

    #[test]
    fn ignores_noise() {
        assert!(parse("just chatting").is_none());
        assert!(parse("").is_none());
        assert!(parse("#BTC").is_none());
        assert!(parse("{}").is_none());
        assert!(parse(r#"{"symbol":"BTCUSDT"}"#).is_none());
        assert!(parse("price moved bullish").is_none());
    }

    #[test]
    fn symbol_normalisation() {
        assert_eq!(normalize_symbol("#btc").unwrap(), "BTCUSDT");
        assert_eq!(normalize_symbol("$ETH").unwrap(), "ETHUSDT");
        assert_eq!(normalize_symbol("SOLUSDC").unwrap(), "SOLUSDC");
        assert!(normalize_symbol("###").is_none());
        assert!(normalize_symbol("12345").is_none());
    }

    #[test]
    fn last_hash_wins_when_first_is_noise() {
        let s = parse("#1 not a signal #ETH Bullish").unwrap();
        assert_eq!(s.symbol, "ETHUSDT");
        assert_eq!(s.side, Side::Buy);
    }
}
