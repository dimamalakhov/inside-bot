//! Offline smoke test (`rust_trader --selftest`).
//!
//! Runs the whole decision chain on synthetic data - signal parsing, filter
//! parsing, position sizing, bracket pricing - without touching Redis, Binance
//! or the network. Useful to verify a build on a server before going live.

use anyhow::{bail, Result};

use crate::math::{
    adjust_price, adjust_quantity_down, calculate_notional, format_price, format_quantity,
    price_after_bps, price_scaled_to_f64, price_to_scaled, quantity_from_notional,
    stop_limit_price, stop_loss_price, take_profit_price, validate_order,
};
use crate::signal;
use crate::types::{Filter, Side, SymbolInfo, SymbolFilters};

pub fn run() -> Result<()> {
    println!("rust_trader self-test");
    println!("=====================");

    // ---- 1. signal parsing ------------------------------------------------
    let cases = [
        ("#BTC Bullish setup, entry 62k", "BTCUSDT", Side::Buy),
        ("#ETH bearish now", "ETHUSDT", Side::Sell),
        (r#"{"symbol":"SOLUSDT","side":"BUY"}"#, "SOLUSDT", Side::Buy),
    ];
    for (raw, expected_symbol, expected_side) in cases {
        match signal::parse(raw) {
            Some(parsed) => {
                if parsed.symbol != expected_symbol || parsed.side != expected_side {
                    bail!(
                        "signal parse mismatch for {raw:?}: got {} {:?}",
                        parsed.symbol,
                        parsed.side
                    );
                }
                println!("  signal ok  : {raw:?} -> {} {}", parsed.symbol, parsed.side);
            }
            None => bail!("signal parse failed for {raw:?}"),
        }
    }

    if signal::parse("hello world").is_some() {
        bail!("noise was parsed as a signal");
    }
    println!("  noise ok   : plain chatter is ignored");

    // ---- 2. exchange filters ---------------------------------------------
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
    let filters = SymbolFilters::from_symbol_info(&info)
        .ok_or_else(|| anyhow::anyhow!("filters failed to parse"))?;
    println!(
        "  filters    : step={} tick={} minNotional={} qtyDec={} priceDec={}",
        filters.step_size,
        filters.tick_size,
        filters.min_notional,
        filters.qty_decimals,
        filters.price_decimals
    );

    // ---- 3. position sizing ----------------------------------------------
    let balance_scaled = 1_000 * crate::math::SCALE; // 1000 USDT wallet
    let allocation_bps = 3_000; // 30%
    let leverage = 10u32;
    let last_price = 50_000.0f64;

    let price = price_to_scaled(last_price);
    let notional = calculate_notional(balance_scaled, allocation_bps, leverage);
    let raw_qty = quantity_from_notional(notional, price);
    let qty = adjust_quantity_down(raw_qty, filters.step_size);

    println!(
        "  sizing     : balance=1000 USDT alloc=30% lev={leverage}x -> notional={} USDT qty={}",
        crate::math::scaled_to_f64(notional),
        format_quantity(qty, &filters)
    );

    if !validate_order(notional, qty, &filters) {
        bail!("computed order is below exchange minimums");
    }
    if qty == 0 {
        bail!("computed quantity is zero");
    }

    // ---- 4. bracket -------------------------------------------------------
    for side in [Side::Buy, Side::Sell] {
        let tp = adjust_price(
            take_profit_price(price, 30, side),
            filters.tick_size,
            side == Side::Buy,
        );
        let sl = adjust_price(
            stop_loss_price(price, 30, side),
            filters.tick_size,
            side == Side::Sell,
        );
        let sl_limit = adjust_price(
            stop_limit_price(sl, 50, side),
            filters.tick_size,
            side == Side::Sell,
        );

        println!(
            "  bracket {:?}: entry={} tp={} sl={} sl_limit={} qty={}",
            side,
            format_price(price, &filters),
            format_price(tp, &filters),
            format_price(sl, &filters),
            format_price(sl_limit, &filters),
            format_quantity(qty, &filters)
        );

        match side {
            Side::Buy => {
                if !(tp > price && sl < price && sl_limit < sl) {
                    bail!("long bracket prices are inconsistent");
                }
            }
            Side::Sell => {
                if !(tp < price && sl > price && sl_limit > sl) {
                    bail!("short bracket prices are inconsistent");
                }
            }
        }
    }

    // ---- 5. rounding helpers ---------------------------------------------
    let rounded_up = price_after_bps(50_000 * crate::math::PRICE_SCALE, 30, true);
    if price_scaled_to_f64(rounded_up) != 50_150.0 {
        bail!("price_after_bps produced {}", price_scaled_to_f64(rounded_up));
    }
    println!("  math ok    : +30bps of 50000 = {}", price_scaled_to_f64(rounded_up));

    println!();
    println!("all checks passed");
    Ok(())
}
