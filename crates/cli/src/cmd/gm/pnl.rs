//! `gm pnl` — cost basis and P&L from the CLI's own trade ledger.
//!
//! Built exclusively from recorded buys/sells (by design: deposits and
//! withdrawals are cash movements, not trading results). Market prices come
//! from the Ondo assets API; a token without market data still shows its
//! basis and realized P&L, just no unrealized figure.

use eyre::Result;
use rwa_ondo::{amounts, api, jupiter, ledger, usecases};

use super::*;

fn usdc_f64(raw: u128) -> f64 {
    raw as f64 / 1_000_000.0
}

/// Shape one ledger position into its JSON view: raw-token quantity
/// conversion (`qty_raw / 10^decimals`), market value (`price * qty`), and
/// None-propagation when the token has no market price (an open position
/// still shows its cost basis and realized P&L, just no unrealized figure).
/// Pure — no I/O — so the USD pipeline is unit-testable without a wallet or
/// network access.
fn shape_token_pnl(t: &usecases::gm::TokenPnl, market_price: Option<f64>, gm_dec: u8) -> PnlTokenJson {
    let qty = amounts::format_amount(&t.qty_raw.to_string(), gm_dec);
    let qty_f = t.qty_raw as f64 / 1e9;
    let avg_cost = t
        .avg_cost_usdc_raw_per_token(gm_dec)
        .map(|c| c as f64 / 1_000_000.0);
    let market_value = market_price.map(|p| p * qty_f);
    let invested = usdc_f64(t.invested_usdc_raw);
    let unrealized = market_value.map(|v| v - invested);
    PnlTokenJson {
        token: t.token.clone(),
        qty,
        avg_cost,
        market_price,
        invested_usdc: invested,
        market_value_usdc: market_value,
        unrealized_usdc: unrealized,
        realized_usdc: t.realized_usdc_raw as f64 / 1_000_000.0,
        oversold_qty: (t.oversold_qty_raw > 0)
            .then(|| amounts::format_amount(&t.oversold_qty_raw.to_string(), gm_dec)),
    }
}

/// Fold one token's market value into the wallet-level total: a closed
/// position (`qty_raw == 0`) never blocks the total even without market
/// data, but an OPEN position with no market price makes the total
/// unknowable (`None`) — sticky once it happens.
fn fold_market_value(acc: Option<f64>, qty_raw: u128, market_value: Option<f64>) -> Option<f64> {
    match (acc, qty_raw, market_value) {
        (Some(acc), 0, _) => Some(acc),
        (Some(acc), _, Some(v)) => Some(acc + v),
        _ => None,
    }
}

pub async fn pnl(json: bool, selected: Option<&str>) -> Result<()> {
    let w = load_wallet(selected)?;
    let pubkey = w.pubkey();

    let events = ledger::read_all(&pubkey);
    let trades_recorded = events
        .iter()
        .filter(|e| e.kind == "buy" || e.kind == "sell")
        .count();
    let summary = usecases::gm::compute_pnl(&events);
    let assets = api::fetch_assets().await.unwrap_or_default();

    let gm_dec = jupiter::GM_SOL_DECIMALS;
    let mut tokens = Vec::new();
    let mut total_market_value: Option<f64> = Some(0.0);
    for t in &summary.tokens {
        // Skip tokens with no open position and no history worth showing.
        if t.qty_raw == 0 && t.realized_usdc_raw == 0 && t.oversold_qty_raw == 0 {
            continue;
        }
        let market_price = api::market_snapshot_for_symbol(&t.token, &assets)
            .ok()
            .map(|(price, _)| price);
        let shaped = shape_token_pnl(t, market_price, gm_dec);
        total_market_value = fold_market_value(total_market_value, t.qty_raw, shaped.market_value_usdc);
        tokens.push(shaped);
    }

    let invested_total = usdc_f64(summary.total_invested_usdc_raw());
    let realized_total = summary.total_realized_usdc_raw() as f64 / 1_000_000.0;
    let unrealized_total = total_market_value.map(|v| v - invested_total);
    let totals = PnlTotalsJson {
        invested_usdc: invested_total,
        market_value_usdc: total_market_value,
        unrealized_usdc: unrealized_total,
        realized_usdc: realized_total,
        total_pnl_usdc: unrealized_total.map(|u| u + realized_total),
    };

    if json {
        return json_out(&PnlJson {
            wallet: pubkey,
            trades_recorded,
            tokens,
            totals,
        });
    }

    println!("P&L for {pubkey} (from {trades_recorded} CLI trades)\n");
    if tokens.is_empty() {
        println!("No trades recorded yet — P&L starts with your first `rwa gm buy`.");
        return Ok(());
    }
    println!(
        "{:<10} {:>12} {:>10} {:>10} {:>11} {:>11} {:>10}",
        "TOKEN", "QTY", "AVG COST", "PRICE", "INVESTED", "UNREALIZED", "REALIZED"
    );
    println!("{}", "-".repeat(80));
    for t in &tokens {
        println!(
            "{:<10} {:>12} {:>10} {:>10} {:>11.2} {:>11} {:>+10.2}",
            t.token,
            t.qty,
            t.avg_cost.map_or("-".into(), |v| format!("{v:.2}")),
            t.market_price.map_or("-".into(), |v| format!("{v:.2}")),
            t.invested_usdc,
            t.unrealized_usdc
                .map_or("-".into(), |v| format!("{v:+.2}")),
            t.realized_usdc,
        );
        if let Some(over) = &t.oversold_qty {
            println!(
                "  note: {over} {} sold beyond CLI-recorded buys (acquired elsewhere) — excluded from P&L",
                t.token
            );
        }
    }
    println!("{}", "-".repeat(80));
    println!(
        "{:<10} {:>12} {:>10} {:>10} {:>11.2} {:>11} {:>+10.2}",
        "TOTAL",
        "",
        "",
        "",
        totals.invested_usdc,
        totals
            .unrealized_usdc
            .map_or("-".into(), |v| format!("{v:+.2}")),
        totals.realized_usdc,
    );
    if let Some(total) = totals.total_pnl_usdc {
        println!("\nTotal P&L (unrealized + realized): {total:+.2} USDC");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use usecases::gm::TokenPnl;

    const GM_DEC: u8 = 9;

    fn token_pnl(qty_raw: u128, invested_usdc_raw: u128, realized_usdc_raw: i128, oversold_qty_raw: u128) -> TokenPnl {
        TokenPnl {
            token: "TSLAon".into(),
            qty_raw,
            invested_usdc_raw,
            realized_usdc_raw,
            oversold_qty_raw,
            ..TokenPnl::default()
        }
    }

    /// User story: «трейдер купил дважды, продал раз» — buy 1.0 for 5, buy
    /// 1.0 more for 7 (avg cost 6), sell 1.0 for 8 (realized +2), leaving 1.0
    /// open at avg cost 6. Mirrors `gm_pnl::average_cost_realized_and_open_basis`'s
    /// end state; this test covers the USD shaping layer built on top of it.
    #[test]
    fn buy_twice_sell_once_shapes_correct_usd_numbers() {
        let t = token_pnl(1_000_000_000, 6_000_000, 2_000_000, 0);
        let shaped = shape_token_pnl(&t, Some(6.5), GM_DEC);

        assert_eq!(shaped.qty, "1");
        assert_eq!(shaped.avg_cost, Some(6.0));
        assert_eq!(shaped.market_price, Some(6.5));
        assert_eq!(shaped.invested_usdc, 6.0);
        // market_value = qty (1.0) * price (6.5)
        assert_eq!(shaped.market_value_usdc, Some(6.5));
        // unrealized = market_value (6.5) - invested (6.0)
        assert_eq!(shaped.unrealized_usdc, Some(0.5));
        assert_eq!(shaped.realized_usdc, 2.0);
        assert_eq!(shaped.oversold_qty, None);
    }

    /// A token with no market data (absent from the Ondo assets response)
    /// still shows its basis and realized P&L — only the market-dependent
    /// figures are None.
    #[test]
    fn missing_market_price_propagates_none_but_keeps_basis_and_realized() {
        let t = token_pnl(1_000_000_000, 6_000_000, 2_000_000, 0);
        let shaped = shape_token_pnl(&t, None, GM_DEC);

        assert_eq!(shaped.market_price, None);
        assert_eq!(shaped.market_value_usdc, None);
        assert_eq!(shaped.unrealized_usdc, None);
        // Ledger-derived figures don't depend on market data.
        assert_eq!(shaped.invested_usdc, 6.0);
        assert_eq!(shaped.realized_usdc, 2.0);
        assert_eq!(shaped.avg_cost, Some(6.0));
    }

    /// Selling beyond the CLI-recorded position flags `oversold_qty` and
    /// leaves it out of `realized_usdc` — the unmatched quantity's basis is
    /// unknown, so it must not be folded into a priced P&L number.
    #[test]
    fn oversold_position_is_flagged_and_excluded_from_realized() {
        // Ledger knows a smaller position than what was sold: realized only
        // reflects the matched portion (3.0 usdc), the rest (2.0 tokens) is
        // oversold and carried separately.
        let t = token_pnl(0, 0, 3_000_000, 2_000_000_000);
        let shaped = shape_token_pnl(&t, None, GM_DEC);

        assert_eq!(shaped.oversold_qty, Some("2".to_string()));
        assert_eq!(shaped.realized_usdc, 3.0, "oversold qty must not corrupt realized P&L");
        assert_eq!(shaped.qty, "0");
    }

    #[test]
    fn fold_market_value_closed_position_never_blocks_total_even_without_price() {
        // A closed position (qty_raw == 0) contributes nothing and never
        // makes the running total unknowable, market data or not.
        assert_eq!(fold_market_value(Some(6.5), 0, None), Some(6.5));
        assert_eq!(fold_market_value(Some(6.5), 0, Some(999.0)), Some(6.5));
    }

    #[test]
    fn fold_market_value_open_position_without_price_makes_total_unknowable() {
        assert_eq!(fold_market_value(Some(6.5), 1_000_000_000, None), None);
    }

    #[test]
    fn fold_market_value_sticky_once_none() {
        // Once the total is unknowable it stays unknowable, even if a later
        // token has full market data.
        assert_eq!(fold_market_value(None, 1_000_000_000, Some(100.0)), None);
    }

    #[test]
    fn fold_market_value_sums_priced_open_positions() {
        let acc = fold_market_value(Some(0.0), 1_000_000_000, Some(6.5));
        let acc = fold_market_value(acc, 2_000_000_000, Some(20.0));
        assert_eq!(acc, Some(26.5));
    }

    /// Wallet-level totals: two open positions, both priced — the totals
    /// pipeline (`shape_token_pnl` + `fold_market_value` + the
    /// invested/unrealized aggregation in `pnl()`) sums correctly.
    #[test]
    fn totals_aggregate_across_multiple_priced_tokens() {
        let tokens = [
            token_pnl(1_000_000_000, 6_000_000, 2_000_000, 0), // 1.0 TSLAon, invested 6, price 6.5
            token_pnl(2_000_000_000, 20_000_000, 0, 0),        // 2.0 NVDAon, invested 20, price 10
        ];
        let prices = [Some(6.5), Some(10.0)];

        let mut total_market_value = Some(0.0);
        let mut invested_total = 0.0;
        for (t, price) in tokens.iter().zip(prices) {
            let shaped = shape_token_pnl(t, price, GM_DEC);
            total_market_value = fold_market_value(total_market_value, t.qty_raw, shaped.market_value_usdc);
            invested_total += shaped.invested_usdc;
        }
        // market values: 1.0*6.5 + 2.0*10.0 = 26.5
        assert_eq!(total_market_value, Some(26.5));
        assert_eq!(invested_total, 26.0);
        let unrealized_total = total_market_value.map(|v| v - invested_total);
        assert_eq!(unrealized_total, Some(0.5));
    }

    /// One open position with no market price makes the wallet-level total
    /// unknowable, even though the other position is fully priced.
    #[test]
    fn totals_are_none_when_any_open_position_lacks_market_price() {
        let tokens = [
            token_pnl(1_000_000_000, 6_000_000, 2_000_000, 0),
            token_pnl(2_000_000_000, 20_000_000, 0, 0),
        ];
        let prices = [Some(6.5), None];

        let mut total_market_value = Some(0.0);
        for (t, price) in tokens.iter().zip(prices) {
            let shaped = shape_token_pnl(t, price, GM_DEC);
            total_market_value = fold_market_value(total_market_value, t.qty_raw, shaped.market_value_usdc);
        }
        assert_eq!(total_market_value, None);
    }
}
