//! Jupiter Ultra holdings — read-balance fallback when Solana RPC is unavailable.
//! Uses the Ultra v1 API (`api.jup.ag/ultra/v1/holdings/{address}`); the Swap V2
//! API used for trading has no holdings endpoint. Honors `RWA_JUPITER_API_KEY`
//! via `with_jupiter_headers`.

use std::collections::HashMap;

use eyre::{eyre, Result};
use serde::Deserialize;

use super::{with_jupiter_headers, ULTRA_LITE_API_BASE};
use crate::solana::{BalanceSource, PortfolioBalances, SolanaTokenBalance};
use crate::token_list::GmTokenEntry;
use crate::types::Mint;
use crate::HTTP;
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
            // Ultra returns raw on-chain amounts — already the canonical raw
            // frame. No Scaled-UI value available from this source.
            balance: sum_ui(accts),
            ui_balance: None,
            raw_amount: raw.to_string(),
        });
    }

    PortfolioBalances { sol, usdc, gm_tokens, source: BalanceSource::Jupiter }
}

/// Fetch Ultra holdings for `wallet` and map to balances. Public entry uses the
/// real Ultra base; `_at` takes the base for tests.
pub(crate) async fn get_holdings_balances(
    wallet: &str,
    tokens: &[GmTokenEntry],
) -> Result<PortfolioBalances> {
    get_holdings_balances_at(ULTRA_LITE_API_BASE, wallet, tokens).await
}

async fn get_holdings_balances_at(
    base: &str,
    wallet: &str,
    tokens: &[GmTokenEntry],
) -> Result<PortfolioBalances> {
    let url = format!("{base}/holdings/{wallet}");
    let resp = with_jupiter_headers(HTTP.get(&url))
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| eyre!("Jupiter holdings request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(eyre!("Jupiter holdings HTTP {}", resp.status()));
    }
    let body: HoldingsResponse = resp
        .json()
        .await
        .map_err(|e| eyre!("Failed to parse Jupiter holdings: {e}"))?;
    Ok(holdings_to_balances(&body, tokens))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn get_holdings_balances_fetches_and_maps() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let _m = server.mock_async(|when, then| {
            when.method(GET).path("/holdings/WALLET123");
            then.status(200).header("content-type", "application/json").json_body(serde_json::json!({
                "amount": "1000000000",
                "tokens": { USDC_MINT: [{ "amount": "2000000", "decimals": 6 }] }
            }));
        }).await;

        let b = get_holdings_balances_at(&server.base_url(), "WALLET123", &[]).await.unwrap();
        assert!((b.sol - 1.0).abs() < 1e-9);
        assert!((b.usdc - 2.0).abs() < 1e-9);
        assert_eq!(b.source, BalanceSource::Jupiter);
    }

    #[tokio::test]
    async fn get_holdings_balances_errors_on_http_500() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let _m = server.mock_async(|when, then| { when.method(GET); then.status(500); }).await;
        assert!(get_holdings_balances_at(&server.base_url(), "W", &[]).await.is_err());
    }

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
