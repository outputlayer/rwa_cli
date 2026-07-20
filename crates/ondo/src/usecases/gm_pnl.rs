//! P&L engine over the trade ledger — pure math, no I/O.
//!
//! Prices and P&L derive from the CLI's own `buy`/`sell` trades. Cash
//! movements (deposits, USDC/SOL transfers, gas) are ignored by design —
//! they are not trading results. A `send_out` of a GM token, however,
//! reduces the position at average cost with no realized impact: the CLI
//! recorded that transfer itself, and keeping the qty would show phantom
//! holdings (and "unrealized" P&L) on tokens the wallet no longer holds.
//! Inbound transfers stay invisible — the CLI cannot see them or price
//! their basis. Average-cost method in raw integer units: each buy adds
//! quantity and cost basis; each sell realizes `proceeds − avg_cost × qty`
//! and reduces the basis proportionally. Sells beyond the ledger-known
//! position (acquired outside the CLI) are tracked as `oversold_qty_raw`
//! and excluded from realized P&L rather than silently mispriced.

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
            // A GM token leaving via the CLI's own `send` is no longer held:
            // shrink the position at average cost, realize nothing (a
            // transfer is not a sale). Without this the row shows phantom
            // qty and "unrealized" P&L on tokens the wallet doesn't hold.
            // USDC/SOL sends stay ignored below — those are cash movements.
            "send_out" if e.token != "SOL" && e.token != "USDC" => {
                if let Some(t) = tokens.get_mut(&e.token) {
                    let matched = qty.min(t.qty_raw);
                    if matched > 0 {
                        let cost_of_matched = t.invested_usdc_raw * matched / t.qty_raw;
                        t.invested_usdc_raw -= cost_of_matched;
                        t.qty_raw -= matched;
                    }
                }
            }
            // Everything else (gas refuels, cash transfers, reclaims,
            // deposits) is ignored by design: P&L is built from trades alone.
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
            prev: None,
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
    fn cash_movements_are_ignored_by_design() {
        // USDC/SOL legs (gas, cash transfers, reclaimed rent) are cash
        // movements, not trading results — they must not shape P&L.
        let events = vec![
            ev("buy", "TSLAon", "1000000000", Some("5000000")),
            ev("gas_refuel", "SOL", "25000000", Some("5000000")),
            ev("send_out", "USDC", "10000000", None),
            ev("reclaim", "SOL", "4273440", None),
        ];
        let s = compute_pnl(&events);
        assert_eq!(s.tokens.len(), 1, "only the trade shapes P&L");
        let t = &s.tokens[0];
        assert_eq!(t.qty_raw, 1_000_000_000, "cash legs don't touch the position");
        assert_eq!(t.invested_usdc_raw, 5_000_000);
    }

    #[test]
    fn send_out_of_gm_token_reduces_position_at_avg_cost() {
        // Buy 2.0 for 10 (avg 5), send 0.5 away via the CLI's own `send`:
        // the wallet no longer holds it, so the position must shrink — qty
        // 1.5, basis 7.5 — with NO realized P&L (a transfer is not a sale).
        // Phantom-holdings regression: pnl used to keep qty at 2.0 and show
        // "unrealized" on tokens the wallet no longer held.
        let events = vec![
            ev("buy", "PFEon", "2000000000", Some("10000000")),
            ev("send_out", "PFEon", "500000000", None),
        ];
        let s = compute_pnl(&events);
        let t = &s.tokens[0];
        assert_eq!(t.qty_raw, 1_500_000_000);
        assert_eq!(t.invested_usdc_raw, 7_500_000);
        assert_eq!(t.realized_usdc_raw, 0, "a transfer must not realize P&L");
        assert_eq!(t.avg_cost_usdc_raw_per_token(9), Some(5_000_000), "avg cost unchanged");
    }

    #[test]
    fn send_out_beyond_ledger_position_clamps_to_zero() {
        // Ledger knows 1.0 bought for 5; 3.0 leaves (2.0 acquired outside
        // the CLI). The known position closes cleanly; the excess has no
        // basis to remove and must not underflow or realize anything.
        let events = vec![
            ev("buy", "TSLAon", "1000000000", Some("5000000")),
            ev("send_out", "TSLAon", "3000000000", None),
        ];
        let s = compute_pnl(&events);
        let t = &s.tokens[0];
        assert_eq!(t.qty_raw, 0);
        assert_eq!(t.invested_usdc_raw, 0);
        assert_eq!(t.realized_usdc_raw, 0);
    }

    #[test]
    fn send_out_of_unknown_token_creates_no_position() {
        // Sending a token the ledger never bought must not invent a row.
        let events = vec![ev("send_out", "AAPLon", "1000000000", None)];
        let s = compute_pnl(&events);
        assert!(s.tokens.is_empty());
    }

    #[test]
    fn buy_of_sol_is_excluded_from_pnl() {
        // Defensive guard (`"buy" if e.token != "SOL"`): a buy whose token is
        // SOL — e.g. an auto-gas USDC→SOL swap that ever got recorded as a buy —
        // must NOT become a priced position; SOL is the fee asset, not a tracked
        // holding. Dropping the `!= "SOL"` guard would give SOL a fake cost basis.
        let events = vec![
            ev("buy", "SOL", "25000000", Some("5000000")),
            ev("buy", "TSLAon", "1000000000", Some("5000000")),
        ];
        let s = compute_pnl(&events);
        assert_eq!(s.tokens.len(), 1, "SOL buy must not create a position");
        assert_eq!(s.tokens[0].token, "TSLAon");
        assert!(!s.tokens.iter().any(|t| t.token == "SOL"), "SOL must be absent from P&L");
    }
}
