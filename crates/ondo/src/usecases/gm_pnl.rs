//! P&L engine over the trade ledger — pure math, no I/O.
//!
//! Deliberately built from `buy`/`sell` events ONLY: entry prices, average
//! cost, realized and total P&L all derive from the CLI's own trades.
//! Deposits, withdrawals, transfers and gas are ignored by design — they are
//! cash movements, not trading results. Average-cost method in raw integer
//! units: each buy adds quantity and cost basis; each sell realizes
//! `proceeds − avg_cost × qty` and reduces the basis proportionally. Sells
//! beyond the ledger-known position (acquired outside the CLI) are tracked as
//! `oversold_qty_raw` and excluded from realized P&L rather than silently
//! mispriced.

use crate::ledger::LedgerEvent;
use std::collections::BTreeMap;

/// Per-token accounting derived from the ledger's buys and sells.
#[derive(Debug, Default, Clone)]
pub struct TokenPnl {
    pub token: String,
    /// Open position per the ledger (raw token units).
    pub qty_raw: u128,
    /// Cost basis of the open position (raw USDC).
    pub invested_usdc_raw: u128,
    /// Realized P&L from sells, raw USDC (negative = loss).
    pub realized_usdc_raw: i128,
    /// Lifetime totals, for the "how much went through" view.
    pub bought_usdc_raw: u128,
    pub sold_usdc_raw: u128,
    /// Sells that exceeded the ledger position — acquired outside the CLI,
    /// so their basis is unknown and excluded from `realized_usdc_raw`.
    pub oversold_qty_raw: u128,
}

impl TokenPnl {
    /// Average entry price in raw USDC per whole token (`10^decimals` raw
    /// units), `None` for an empty position.
    #[must_use]
    pub fn avg_cost_usdc_raw_per_token(&self, decimals: u8) -> Option<u128> {
        if self.qty_raw == 0 {
            return None;
        }
        Some(
            self.invested_usdc_raw
                .saturating_mul(10u128.pow(decimals as u32))
                / self.qty_raw,
        )
    }
}

/// Wallet-level trading summary.
#[derive(Debug, Default, Clone)]
pub struct PnlSummary {
    /// Per-token positions, sorted by symbol.
    pub tokens: Vec<TokenPnl>,
}

impl PnlSummary {
    /// Total realized P&L across tokens (raw USDC).
    #[must_use]
    pub fn total_realized_usdc_raw(&self) -> i128 {
        self.tokens.iter().map(|t| t.realized_usdc_raw).sum()
    }

    /// Cost basis currently deployed in open positions (raw USDC).
    #[must_use]
    pub fn total_invested_usdc_raw(&self) -> u128 {
        self.tokens.iter().map(|t| t.invested_usdc_raw).sum()
    }
}

fn parse_raw(s: &str) -> u128 {
    s.parse().unwrap_or(0)
}

/// Fold the ledger's buy/sell events into per-token positions.
#[must_use]
pub fn compute_pnl(events: &[LedgerEvent]) -> PnlSummary {
    let mut tokens: BTreeMap<String, TokenPnl> = BTreeMap::new();

    for e in events {
        let qty = parse_raw(&e.qty_raw);
        let usdc = e.usdc_raw.as_deref().map(parse_raw).unwrap_or(0);
        match e.kind.as_str() {
            "buy" if e.token != "SOL" => {
                let t = tokens.entry(e.token.clone()).or_insert_with(|| TokenPnl {
                    token: e.token.clone(),
                    ..TokenPnl::default()
                });
                t.qty_raw += qty;
                t.invested_usdc_raw += usdc;
                t.bought_usdc_raw += usdc;
            }
            "sell" => {
                let t = tokens.entry(e.token.clone()).or_insert_with(|| TokenPnl {
                    token: e.token.clone(),
                    ..TokenPnl::default()
                });
                t.sold_usdc_raw += usdc;
                // Match against the known position; anything beyond it was
                // acquired outside the CLI (unknown basis).
                let matched = qty.min(t.qty_raw);
                let oversold = qty - matched;
                if matched > 0 {
                    // Proportional shares, integer math.
                    let cost_of_matched = t.invested_usdc_raw * matched / t.qty_raw;
                    let proceeds_of_matched = usdc * matched / qty;
                    t.realized_usdc_raw +=
                        proceeds_of_matched as i128 - cost_of_matched as i128;
                    t.invested_usdc_raw -= cost_of_matched;
                    t.qty_raw -= matched;
                }
                t.oversold_qty_raw += oversold;
            }
            // Everything else (gas refuels, transfers, reclaims, deposits) is
            // ignored by design: P&L is built from trades alone.
            _ => {}
        }
    }

    PnlSummary {
        tokens: tokens.into_values().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: &str, token: &str, qty: &str, usdc: Option<&str>) -> LedgerEvent {
        LedgerEvent {
            ts: "2026-07-02T00:00:00Z".into(),
            sig: None,
            kind: kind.into(),
            token: token.into(),
            qty_raw: qty.into(),
            usdc_raw: usdc.map(str::to_string),
        }
    }

    #[test]
    fn average_cost_realized_and_open_basis() {
        // Buy 1.0 TSLA for 5 USDC, then 1.0 more for 7 → avg 6.
        // Sell 1.0 for 8 → realized 8 − 6 = +2; basis left: 6 for 1.0.
        let events = vec![
            ev("buy", "TSLAon", "1000000000", Some("5000000")),
            ev("buy", "TSLAon", "1000000000", Some("7000000")),
            ev("sell", "TSLAon", "1000000000", Some("8000000")),
        ];
        let s = compute_pnl(&events);
        let t = &s.tokens[0];
        assert_eq!(t.qty_raw, 1_000_000_000);
        assert_eq!(t.invested_usdc_raw, 6_000_000);
        assert_eq!(t.realized_usdc_raw, 2_000_000);
        assert_eq!(t.avg_cost_usdc_raw_per_token(9), Some(6_000_000));
        assert_eq!(t.bought_usdc_raw, 12_000_000);
        assert_eq!(t.sold_usdc_raw, 8_000_000);
        assert_eq!(t.oversold_qty_raw, 0);
        assert_eq!(s.total_realized_usdc_raw(), 2_000_000);
        assert_eq!(s.total_invested_usdc_raw(), 6_000_000);
    }

    #[test]
    fn realized_loss_is_negative() {
        // Buy 2.0 for 10, sell 2.0 for 9 → realized −1, position closed.
        let events = vec![
            ev("buy", "NVDAon", "2000000000", Some("10000000")),
            ev("sell", "NVDAon", "2000000000", Some("9000000")),
        ];
        let s = compute_pnl(&events);
        let t = &s.tokens[0];
        assert_eq!(t.realized_usdc_raw, -1_000_000);
        assert_eq!(t.qty_raw, 0);
        assert_eq!(t.invested_usdc_raw, 0);
        assert_eq!(t.avg_cost_usdc_raw_per_token(9), None);
    }

    #[test]
    fn oversell_beyond_ledger_is_isolated_not_mispriced() {
        // Ledger knows 1.0 bought for 5; wallet sells 3.0 for 15 (2.0 came
        // from outside the CLI). Only the known 1.0 realizes P&L:
        // proceeds 15 × 1/3 = 5, basis 5 → realized 0; 2.0 flagged oversold.
        let events = vec![
            ev("buy", "SPYon", "1000000000", Some("5000000")),
            ev("sell", "SPYon", "3000000000", Some("15000000")),
        ];
        let s = compute_pnl(&events);
        let t = &s.tokens[0];
        assert_eq!(t.realized_usdc_raw, 0);
        assert_eq!(t.oversold_qty_raw, 2_000_000_000);
        assert_eq!(t.qty_raw, 0);
    }

    #[test]
    fn non_trade_events_are_ignored_by_design() {
        let events = vec![
            ev("buy", "TSLAon", "1000000000", Some("5000000")),
            ev("gas_refuel", "SOL", "25000000", Some("5000000")),
            ev("send_out", "USDC", "10000000", None),
            ev("send_out", "TSLAon", "500000000", None),
            ev("reclaim", "SOL", "4273440", None),
        ];
        let s = compute_pnl(&events);
        assert_eq!(s.tokens.len(), 1, "only the trade shapes P&L");
        let t = &s.tokens[0];
        assert_eq!(t.qty_raw, 1_000_000_000, "transfers don't touch the position");
        assert_eq!(t.invested_usdc_raw, 5_000_000);
    }
}
