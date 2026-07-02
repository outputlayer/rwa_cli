//! Pure positions math shared by `portfolio` and `close-all`: pricing GM
//! balances against Ondo market data and pre-filtering positions for closing.
//! No I/O — callers fetch balances/assets and only format the results.

use eyre::Result;

use super::gm::{should_skip_position, CloseSkip};
use crate::{amounts, api, jupiter, solana};

pub struct PortfolioPosition {
    pub token: String,
    pub balance: f64,
    pub price: f64,
    pub value_usd: f64,
    pub gm_alloc_pct: f64,
    pub change_pct_24h: f64,
}

pub struct PortfolioUnavailable {
    pub symbol: String,
    pub reason: String,
}

pub struct PortfolioSummary {
    pub positions: Vec<PortfolioPosition>,
    pub unavailable: Vec<PortfolioUnavailable>,
    pub value_usd: f64,
    pub change_24h_usd: f64,
    pub change_24h_pct: f64,
}

/// Price GM balances and compute per-position allocation plus the aggregate
/// 24h P&L. Positions sort by value descending; tokens without market data
/// land in `unavailable` instead of failing the whole portfolio.
#[must_use]
pub fn compute_portfolio(
    balances: &[solana::SolanaTokenBalance],
    assets: &[api::OndoAsset],
) -> PortfolioSummary {
    let mut positions = Vec::new();
    let mut unavailable = Vec::new();
    let mut value_usd = 0.0;
    let mut prev_value_usd = 0.0;

    for tb in balances {
        let (price, pct_24h) = match api::market_snapshot_for_symbol(&tb.symbol, assets) {
            Ok(v) => v,
            Err(e) => {
                unavailable.push(PortfolioUnavailable {
                    symbol: tb.symbol.clone(),
                    reason: format!("market data unavailable: {e}"),
                });
                continue;
            }
        };
        let value = tb.balance * price;
        // Back out the position's value 24h ago from today's value and the
        // 24h change percentage, so the aggregate P&L weights by size.
        let prev_value = if pct_24h.abs() > f64::EPSILON {
            value / (1.0 + pct_24h / 100.0)
        } else {
            value
        };
        value_usd += value;
        prev_value_usd += prev_value;
        positions.push(PortfolioPosition {
            token: tb.symbol.clone(),
            balance: tb.balance,
            price,
            value_usd: value,
            gm_alloc_pct: 0.0,
            change_pct_24h: pct_24h,
        });
    }

    if value_usd.abs() > f64::EPSILON {
        for p in &mut positions {
            p.gm_alloc_pct = (p.value_usd / value_usd) * 100.0;
        }
    }

    positions.sort_by(|a, b| {
        b.value_usd
            .partial_cmp(&a.value_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let change_24h_usd = value_usd - prev_value_usd;
    let change_24h_pct = if prev_value_usd.abs() > f64::EPSILON {
        (change_24h_usd / prev_value_usd) * 100.0
    } else {
        0.0
    };

    PortfolioSummary {
        positions,
        unavailable,
        value_usd,
        change_24h_usd,
        change_24h_pct,
    }
}

/// A tradable, non-tiny position pre-filtered for closing.
pub struct ClosePosition {
    pub symbol: String,
    pub mint: String,
    pub sell_raw: String,
    pub sell_display: String,
}

/// Filter phase for close-all: raw balances → (sellable positions, skipped).
/// Skips zero partial amounts, positions without market data, sub-minimum
/// values, and tokens not tradable in the current session.
pub fn filter_close_positions(
    balances: &[solana::SolanaTokenBalance],
    sell_pct: f64,
    assets: &[api::OndoAsset],
    tradable_set: &std::collections::HashSet<String>,
) -> Result<(Vec<ClosePosition>, Vec<CloseSkip>)> {
    let mut positions = Vec::new();
    let mut skipped = Vec::new();

    for tb in balances {
        let sell_raw = if sell_pct < 100.0 {
            let raw: u128 = tb.raw_amount.parse().map_err(|_| {
                eyre::eyre!(
                    "Invalid on-chain amount for {}: {}",
                    tb.symbol,
                    tb.raw_amount
                )
            })?;
            let partial = amounts::pct_of_u128(raw, sell_pct);
            if partial == 0 {
                continue;
            }
            partial.to_string()
        } else {
            tb.raw_amount.clone()
        };

        let sell_balance = if sell_pct < 100.0 {
            tb.balance * sell_pct / 100.0
        } else {
            tb.balance
        };
        let est_value = match api::market_snapshot_for_symbol(&tb.symbol, assets) {
            Ok((price, _)) => sell_balance * price,
            Err(_) => {
                skipped.push(CloseSkip {
                    token: tb.symbol.clone(),
                    estimated_usd: 0.0,
                    reason: "market data unavailable",
                });
                continue;
            }
        };

        if let Some(skip) = should_skip_position(&tb.symbol, est_value, tradable_set) {
            skipped.push(skip);
            continue;
        }

        let sell_display = amounts::format_amount(&sell_raw, jupiter::GM_SOL_DECIMALS);
        positions.push(ClosePosition {
            symbol: tb.symbol.clone(),
            mint: tb.mint.to_string(),
            sell_raw,
            sell_display,
        });
    }

    Ok((positions, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Mint;

    fn asset(symbol: &str, price: &str, pct_24h: &str) -> api::OndoAsset {
        serde_json::from_value(serde_json::json!({
            "symbol": symbol,
            "assetName": symbol,
            "primaryMarket": {
                "price": price,
                "priceChangePct24h": pct_24h,
            }
        }))
        .expect("valid asset json")
    }

    fn balance(symbol: &str, ui: f64, raw: &str) -> solana::SolanaTokenBalance {
        solana::SolanaTokenBalance {
            symbol: symbol.to_string(),
            mint: Mint::from("So11111111111111111111111111111111111111112"),
            balance: ui,
            raw_amount: raw.to_string(),
        }
    }

    #[test]
    fn compute_portfolio_prices_allocates_and_sorts() {
        let assets = vec![asset("TSLAon", "100", "10"), asset("AAPLon", "50", "0")];
        let balances = vec![balance("AAPLon", 2.0, "2000000000"), balance("TSLAon", 3.0, "3000000000")];
        let s = compute_portfolio(&balances, &assets);

        assert_eq!(s.positions.len(), 2);
        // Sorted by value: TSLA 300 first, AAPL 100 second.
        assert_eq!(s.positions[0].token, "TSLAon");
        assert!((s.positions[0].value_usd - 300.0).abs() < 1e-9);
        assert!((s.positions[0].gm_alloc_pct - 75.0).abs() < 1e-9);
        assert!((s.value_usd - 400.0).abs() < 1e-9);
        // TSLA was ~272.7 yesterday (+10%), AAPL flat → +27.27 (+7.32%).
        assert!((s.change_24h_usd - (300.0 - 300.0 / 1.1)).abs() < 1e-6);
        assert!(s.change_24h_pct > 7.3 && s.change_24h_pct < 7.4);
        assert!(s.unavailable.is_empty());
    }

    #[test]
    fn compute_portfolio_isolates_missing_market_data() {
        let assets = vec![asset("TSLAon", "100", "0")];
        let balances = vec![balance("TSLAon", 1.0, "1000000000"), balance("GHOSTon", 5.0, "5000000000")];
        let s = compute_portfolio(&balances, &assets);
        assert_eq!(s.positions.len(), 1);
        assert_eq!(s.unavailable.len(), 1);
        assert_eq!(s.unavailable[0].symbol, "GHOSTon");
        assert!((s.value_usd - 100.0).abs() < 1e-9);
    }

    #[test]
    fn filter_close_positions_partial_pct_and_skips() {
        let assets = vec![asset("TSLAon", "100", "0"), asset("DUSTon", "0.1", "0")];
        let balances = vec![
            balance("TSLAon", 2.0, "2000000000"),
            balance("DUSTon", 1.0, "1000000000"),   // $0.10 → below minimum
            balance("GHOSTon", 1.0, "1000000000"),  // no market data
        ];
        let tradable = std::collections::HashSet::new();
        let (positions, skipped) = filter_close_positions(&balances, 50.0, &assets, &tradable).unwrap();

        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].symbol, "TSLAon");
        assert_eq!(positions[0].sell_raw, "1000000000"); // 50% of raw
        assert_eq!(positions[0].sell_display, "1");
        assert_eq!(skipped.len(), 2);
        assert!(skipped.iter().any(|s| s.token == "DUSTon" && s.reason.contains("minimum")));
        assert!(skipped.iter().any(|s| s.token == "GHOSTon" && s.reason.contains("market data")));
    }
}
