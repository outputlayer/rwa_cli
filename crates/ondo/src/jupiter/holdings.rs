//! Jupiter Ultra holdings — read-balance fallback when Solana RPC is unavailable.
//! Uses the Ultra v1 API (`api.jup.ag/ultra/v1/holdings/{address}`); the Swap V2
//! API used for trading has no holdings endpoint. Honors `RWA_JUPITER_API_KEY`
//! via `with_jupiter_headers`.

use std::collections::HashMap;

use serde::Deserialize;

use crate::solana::{BalanceSource, PortfolioBalances, SolanaTokenBalance};
use crate::token_list::GmTokenEntry;
use crate::types::Mint;
use crate::USDC_MINT;

#[derive(Debug, Deserialize)]
pub(crate) struct HoldingsResponse {
    #[serde(default)]
    amount: String,
    #[serde(default)]
    tokens: HashMap<String, Vec<HoldingAccount>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct HoldingAccount {
    #[serde(default)]
    amount: String,
    #[serde(default)]
    decimals: u8,
}

/// Sum the ui-amount of a set of token accounts using each account's decimals.
fn sum_ui(accts: &[HoldingAccount]) -> f64 {
    accts
        .iter()
        .filter_map(|a| {
            let raw: u128 = a.amount.parse().ok()?;
            Some(raw as f64 / 10f64.powi(a.decimals as i32))
        })
        .sum()
}

/// Pure: map an Ultra holdings response into `PortfolioBalances`. SOL from the
/// top-level lamports `amount`; USDC from the USDC mint's accounts; GM tokens
/// matched against the token list (mints not in the list are ignored).
pub(crate) fn holdings_to_balances(
    resp: &HoldingsResponse,
    tokens: &[GmTokenEntry],
) -> PortfolioBalances {
    let sol = resp.amount.parse::<u64>().unwrap_or(0) as f64 / 1_000_000_000.0;
    let usdc = resp.tokens.get(USDC_MINT).map(|a| sum_ui(a)).unwrap_or(0.0);

    let mut gm_tokens = Vec::new();
    for t in tokens {
        let Some(mint) = t.solana_address else { continue };
        let Some(accts) = resp.tokens.get(mint) else { continue };
        let raw: u128 = accts.iter().filter_map(|a| a.amount.parse::<u128>().ok()).sum();
        if raw == 0 {
            continue;
        }
        gm_tokens.push(SolanaTokenBalance {
            symbol: t.symbol.to_string(),
            mint: Mint::from(mint),
            balance: sum_ui(accts),
            raw_amount: raw.to_string(),
        });
    }

    PortfolioBalances { sol, usdc, gm_tokens, source: BalanceSource::Jupiter }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> HoldingsResponse {
        serde_json::from_value(serde_json::json!({
            "amount": "2500000000",
            "tokens": {
                USDC_MINT: [{ "amount": "1500000", "decimals": 6 }],
                "Mint1111111111111111111111111111111111111": [
                    { "amount": "1000000000", "decimals": 9 },
                    { "amount": "250000000", "decimals": 9 }
                ],
                "UnknownMint11111111111111111111111111111111": [{ "amount": "9", "decimals": 0 }],
                "ZeroMint1111111111111111111111111111111111": [{ "amount": "0", "decimals": 9 }]
            }
        }))
        .unwrap()
    }

    #[test]
    fn holdings_to_balances_maps_sol_usdc_and_known_gm_only() {
        let toks = [
            GmTokenEntry { symbol: "TESTon", solana_address: Some("Mint1111111111111111111111111111111111111") },
            GmTokenEntry { symbol: "ZEROon", solana_address: Some("ZeroMint1111111111111111111111111111111111") },
        ];
        let b = holdings_to_balances(&sample(), &toks);
        assert!((b.sol - 2.5).abs() < 1e-9);
        assert!((b.usdc - 1.5).abs() < 1e-9);
        assert_eq!(b.source, BalanceSource::Jupiter);
        assert_eq!(b.gm_tokens.len(), 1);
        assert_eq!(b.gm_tokens[0].symbol, "TESTon");
        assert_eq!(b.gm_tokens[0].raw_amount, "1250000000");
        assert!((b.gm_tokens[0].balance - 1.25).abs() < 1e-9);
    }
}
