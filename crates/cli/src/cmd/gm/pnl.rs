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
        let qty = amounts::format_amount(&t.qty_raw.to_string(), gm_dec);
        let qty_f = t.qty_raw as f64 / 1e9;
        let avg_cost = t
            .avg_cost_usdc_raw_per_token(gm_dec)
            .map(|c| c as f64 / 1_000_000.0);
        let market_price = api::market_snapshot_for_symbol(&t.token, &assets)
            .ok()
            .map(|(price, _)| price);
        let market_value = market_price.map(|p| p * qty_f);
        let invested = usdc_f64(t.invested_usdc_raw);
        let unrealized = market_value.map(|v| v - invested);
        total_market_value = match (total_market_value, t.qty_raw, market_value) {
            (Some(acc), 0, _) => Some(acc),
            (Some(acc), _, Some(v)) => Some(acc + v),
            // An open position without market data makes totals unknowable.
            _ => None,
        };
        tokens.push(PnlTokenJson {
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
        });
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
