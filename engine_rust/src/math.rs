//! Fixed-point math for the order hot path.
//!
//! Everything is integer arithmetic on a `1e8` scale, so no floating point
//! rounding surprises can leak into quantities or prices. Intermediate
//! multiplications use `u128` - a naive `u64` implementation overflows for
//! realistic balances (e.g. `3000 USDT * 1e8 * 1e8`).

use crate::types::{Side, SymbolFilters};

/// Quantity scale: 1 = 1e-8.
pub const SCALE: u64 = 100_000_000;
/// Price scale: 1 = 1e-8.
pub const PRICE_SCALE: u64 = 100_000_000;
/// Basis points: 100% = 10_000 bps.
pub const BPS_SCALE: u64 = 10_000;

/// `f64 -> 1e8` fixed point (rounded to nearest, negatives clamped to 0).
#[inline]
pub fn f64_to_scaled(value: f64) -> u64 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    let scaled = (value * SCALE as f64).round();
    if scaled >= u64::MAX as f64 {
        u64::MAX
    } else {
        scaled as u64
    }
}

/// `f64 price -> 1e8` fixed point.
#[inline]
pub fn price_to_scaled(value: f64) -> u64 {
    f64_to_scaled(value)
}

#[inline]
pub fn scaled_to_f64(value: u64) -> f64 {
    value as f64 / SCALE as f64
}

#[inline]
pub fn price_scaled_to_f64(value: u64) -> f64 {
    value as f64 / PRICE_SCALE as f64
}

/// Rounds quantity **down** to the symbol step size.
#[inline]
pub fn adjust_quantity_down(raw_qty: u64, step_size: u64) -> u64 {
    if step_size == 0 {
        return 0;
    }
    (raw_qty / step_size) * step_size
}

/// Rounds price to the symbol tick size, up (`ceil`) or down (`floor`).
#[inline]
pub fn adjust_price(price: u64, tick_size: u64, round_up: bool) -> u64 {
    if tick_size == 0 {
        return price;
    }
    if round_up {
        match price.checked_add(tick_size - 1) {
            Some(v) => (v / tick_size) * tick_size,
            None => price,
        }
    } else {
        (price / tick_size) * tick_size
    }
}

/// Minimum-viability check for an order.
#[inline]
pub fn validate_order(notional: u64, qty: u64, filters: &SymbolFilters) -> bool {
    qty >= filters.min_qty && notional >= filters.min_notional && qty > 0
}

/// `notional = balance * allocation_bps * leverage / 10_000`
#[inline]
pub fn calculate_notional(balance_scaled: u64, allocation_bps: u64, leverage: u32) -> u64 {
    let value = balance_scaled as u128 * allocation_bps as u128 * leverage.max(1) as u128
        / BPS_SCALE as u128;
    if value >= u64::MAX as u128 {
        u64::MAX
    } else {
        value as u64
    }
}

/// `qty = notional / price`, result on the same `1e8` scale.
#[inline]
pub fn quantity_from_notional(notional: u64, price_scaled: u64) -> u64 {
    if price_scaled == 0 || notional == 0 {
        return 0;
    }
    let value = notional as u128 * SCALE as u128 / price_scaled as u128;
    if value >= u64::MAX as u128 {
        u64::MAX
    } else {
        value as u64
    }
}

/// Moves `price` by `bps` basis points. `up = true` adds, `false` subtracts.
#[inline]
pub fn price_after_bps(price: u64, bps: u64, up: bool) -> u64 {
    let factor = if up {
        BPS_SCALE.saturating_add(bps)
    } else {
        BPS_SCALE.saturating_sub(bps)
    };
    let value = price as u128 * factor as u128 / BPS_SCALE as u128;
    if value >= u64::MAX as u128 {
        u64::MAX
    } else {
        value as u64
    }
}

/// Take-profit price for a position opened on `entry_side`.
/// Long: above entry (round away from entry), short: below entry.
#[inline]
pub fn take_profit_price(avg_price: u64, tp_bps: u64, entry_side: Side) -> u64 {
    match entry_side {
        Side::Buy => price_after_bps(avg_price, tp_bps, true),
        Side::Sell => price_after_bps(avg_price, tp_bps, false),
    }
}

/// Stop-loss trigger price for a position opened on `entry_side`.
#[inline]
pub fn stop_loss_price(avg_price: u64, sl_bps: u64, entry_side: Side) -> u64 {
    match entry_side {
        Side::Buy => price_after_bps(avg_price, sl_bps, false),
        Side::Sell => price_after_bps(avg_price, sl_bps, true),
    }
}

/// Limit price for the stop-loss, padded by `slippage_bps` beyond the trigger
/// so the stop can actually cross.
#[inline]
pub fn stop_limit_price(stop_price: u64, slippage_bps: u64, entry_side: Side) -> u64 {
    match entry_side {
        Side::Buy => price_after_bps(stop_price, slippage_bps, false),
        Side::Sell => price_after_bps(stop_price, slippage_bps, true),
    }
}

/// Formats a fixed-point value with at most `decimals` fractional digits,
/// dropping trailing zeros (Binance accepts both forms).
pub fn format_scaled(value: u64, decimals: u8) -> String {
    let int_part = value / SCALE;
    let decimals = decimals.min(8);
    if decimals == 0 {
        return int_part.to_string();
    }

    let divisor = crate::types::pow10(8 - decimals);
    let frac_part = if divisor == 0 {
        0
    } else {
        (value % SCALE) / divisor
    };
    if frac_part == 0 {
        return int_part.to_string();
    }

    let mut frac = format!("{:0width$}", frac_part, width = decimals as usize);
    while frac.ends_with('0') {
        frac.pop();
    }
    if frac.is_empty() {
        int_part.to_string()
    } else {
        format!("{}.{}", int_part, frac)
    }
}

/// Quantity string for the API, rounding is already baked in.
#[inline]
pub fn format_quantity(qty: u64, filters: &SymbolFilters) -> String {
    format_scaled(qty, filters.qty_decimals)
}

/// Price string for the API.
#[inline]
pub fn format_price(price: u64, filters: &SymbolFilters) -> String {
    format_scaled(price, filters.price_decimals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Filter, SymbolInfo, SymbolFilters};

    fn btc_filters() -> SymbolFilters {
        SymbolFilters::from_symbol_info(&SymbolInfo {
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
                    max_price: "1e7".into(),
                    tick_size: "0.1".into(),
                },
                Filter::MinNotional {
                    notional: "5".into(),
                },
            ],
        })
        .unwrap()
    }

    #[test]
    fn conversion_round_trip() {
        assert_eq!(f64_to_scaled(1.0), 100_000_000);
        assert_eq!(f64_to_scaled(0.5), 50_000_000);
        assert_eq!(f64_to_scaled(0.001), 100_000);
        assert_eq!(f64_to_scaled(123.456789), 12_345_678_900);
        assert_eq!(f64_to_scaled(-1.0), 0);
        assert_eq!(f64_to_scaled(f64::NAN), 0);
    }

    #[test]
    fn quantity_rounding_is_down() {
        let step = 100_000; // 0.001
        assert_eq!(adjust_quantity_down(1_500_000, step), 1_500_000);
        assert_eq!(adjust_quantity_down(1_550_000, step), 1_500_000);
        assert_eq!(adjust_quantity_down(99_999, step), 0);
        assert_eq!(adjust_quantity_down(123, 0), 0);
    }

    #[test]
    fn price_rounding_modes() {
        let tick = 10_000; // 0.0001
        let base = 50_000 * PRICE_SCALE; // 50000.0
        assert_eq!(adjust_price(base, tick, false), base);
        assert_eq!(adjust_price(base, tick, true), base);
        assert_eq!(adjust_price(base + 5, tick, false), base);
        assert_eq!(adjust_price(base + 5, tick, true), base + tick);
    }

    #[test]
    fn notional_and_quantity() {
        // 1000 USDT balance, 30% allocation, 10x leverage -> 3000 USDT notional
        let notional = calculate_notional(1000 * SCALE, 3000, 10);
        assert_eq!(notional, 3000 * SCALE);

        // 3000 USDT at 50_000 -> 0.06 BTC (must not overflow)
        let qty = quantity_from_notional(notional, 50_000 * PRICE_SCALE);
        assert_eq!(qty, 6_000_000);
        assert_eq!(quantity_from_notional(notional, 0), 0);
    }

    #[test]
    fn notional_does_not_overflow_for_large_balances() {
        // 1_000_000 USDT, 100% allocation, 20x -> 20_000_000 USDT notional
        let notional = calculate_notional(1_000_000 * SCALE, 10_000, 20);
        assert_eq!(notional, 20_000_000 * SCALE);
        // and quantity computation on top of it
        let qty = quantity_from_notional(notional, 60_000 * PRICE_SCALE);
        assert!(qty > 0);
    }

    #[test]
    fn take_profit_and_stop_loss_directions() {
        let entry = 50_000 * PRICE_SCALE;
        let tp_long = take_profit_price(entry, 30, Side::Buy);
        let sl_long = stop_loss_price(entry, 30, Side::Buy);
        assert!(tp_long > entry);
        assert!(sl_long < entry);
        assert_eq!(tp_long, 50_150 * PRICE_SCALE);
        assert_eq!(sl_long, 49_850 * PRICE_SCALE);

        let tp_short = take_profit_price(entry, 30, Side::Sell);
        let sl_short = stop_loss_price(entry, 30, Side::Sell);
        assert!(tp_short < entry);
        assert!(sl_short > entry);
    }

    #[test]
    fn stop_limit_is_padded_beyond_trigger() {
        let entry = 50_000 * PRICE_SCALE;
        let sl = stop_loss_price(entry, 30, Side::Buy);
        let limit = stop_limit_price(sl, 50, Side::Buy);
        assert!(limit < sl, "long stop limit must sit below the trigger");

        let sl_s = stop_loss_price(entry, 30, Side::Sell);
        let limit_s = stop_limit_price(sl_s, 50, Side::Sell);
        assert!(limit_s > sl_s, "short stop limit must sit above the trigger");
    }

    #[test]
    fn quantity_formatting() {
        let f = btc_filters();
        assert_eq!(format_quantity(1_500_000, &f), "0.015");
        assert_eq!(format_quantity(100_000_000, &f), "1");
        assert_eq!(format_quantity(6_000_000, &f), "0.06");
        assert_eq!(format_quantity(0, &f), "0");
    }

    #[test]
    fn price_formatting() {
        let f = btc_filters();
        assert_eq!(format_price(50_000 * PRICE_SCALE, &f), "50000");
        assert_eq!(format_price(50_150 * PRICE_SCALE, &f), "50150");
        // 0.1 tick size -> one decimal
        assert_eq!(format_price(50_155 * PRICE_SCALE, &f), "50155");
    }

    #[test]
    fn format_scaled_edge_cases() {
        assert_eq!(format_scaled(0, 3), "0");
        assert_eq!(format_scaled(1, 8), "0.00000001");
        assert_eq!(format_scaled(10_000_000, 2), "0.1");
        assert_eq!(format_scaled(123_450_000_000, 2), "1234.5");
    }
}
