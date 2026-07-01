use eyre::{Result, eyre};
use serde::Deserialize;

use crate::{amounts, token_list::GmTokenEntry, USDC_MINT};
use crate::types::Mint;
use super::rpc::{rpc_call_simple, rpc_batch_with_retry, rpc_urls, RpcMode, RpcRequest, RpcResponse};
use super::transfer::{derive_ata, TOKEN_PROGRAM};

/// Ondo GM tokens use Token-2022 (Token Extensions) on Solana.
pub(super) const TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

// ── RPC deserialization types ────────────────────────────────

#[derive(Deserialize)]
pub(super) struct GetTokenAccountsResult {
    #[serde(default)]
    value: Vec<serde_json::Value>,
}

impl GetTokenAccountsResult {
    /// Parse token account entries, skipping any that don't have the expected structure.
    /// Some RPC nodes return malformed entries in batch responses.
    pub(super) fn accounts(&self) -> Vec<TokenAccountInfo> {
        self.value.iter()
            .filter_map(|v| serde_json::from_value::<TokenAccountInfo>(v.clone()).ok())
            .collect()
    }
}

#[derive(Deserialize)]
pub(super) struct TokenAccountInfo {
    pub(super) pubkey: Option<String>,
    pub(super) account: AccountData,
}

#[derive(Deserialize)]
pub(super) struct AccountData {
    pub(super) lamports: Option<u64>,
    pub(super) data: ParsedData,
}

#[derive(Deserialize)]
pub(super) struct ParsedData {
    pub(super) parsed: ParsedTokenData,
}

#[derive(Deserialize)]
pub(super) struct ParsedTokenData {
    pub(super) info: TokenInfo,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TokenInfo {
    pub(super) mint: String,
    pub(super) token_amount: TokenAmount,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TokenAmount {
    pub(super) ui_amount: Option<f64>,
    pub(super) amount: String,
    pub(super) decimals: u8,
}

#[derive(Deserialize)]
struct GetBalanceResult {
    value: u64,
}

// ── getMultipleAccounts response ─────────────────────────────

#[derive(Deserialize)]
struct GetMultipleAccountsResult {
    value: Vec<Option<MultiAccountInfo>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MultiAccountInfo {
    #[serde(default)]
    lamports: u64,
    data: serde_json::Value,
}

impl MultiAccountInfo {
    /// Try to extract token amount from parsed SPL token data.
    fn token_ui_amount(&self) -> Option<f64> {
        let token_amount = self.data
            .get("parsed")
            .and_then(|p| p.get("info"))
            .and_then(|i| i.get("tokenAmount"))?;

        token_amount.get("uiAmount")
            .and_then(|v| v.as_f64())
            .or_else(|| {
                let raw = token_amount.get("amount")?.as_str()?;
                let decimals = token_amount.get("decimals")?.as_u64()?;
                raw_amount_to_f64(raw, decimals as u8).ok()
            })
    }
}

// ── Balance queries ──────────────────────────────────────────

/// Get SOL balance in SOL (not lamports).
pub async fn get_sol_balance(wallet: &str, rpc_url: Option<&str>) -> Result<f64> {
    let raw = get_sol_balance_raw(wallet, rpc_url).await?;
    let lamports: u64 = raw
        .parse()
        .map_err(|_| eyre!("Invalid on-chain SOL amount: {raw}"))?;
    Ok(lamports as f64 / 1_000_000_000.0)
}

/// Get SOL balance in raw lamports.
pub async fn get_sol_balance_raw(wallet: &str, rpc_url: Option<&str>) -> Result<String> {
    let result: GetBalanceResult = rpc_call_simple(
        "getBalance",
        serde_json::json!([wallet, { "commitment": "confirmed" }]),
        rpc_url,
        RpcMode::Race,
    ).await?;
    Ok(result.value.to_string())
}

/// Get USDC balance for a wallet on Solana.
pub async fn get_usdc_balance(wallet: &str, rpc_url: Option<&str>) -> Result<f64> {
    let (ui, _) = get_usdc_balance_raw(wallet, rpc_url).await?;
    Ok(ui)
}

/// Get raw USDC balance as (ui_amount, raw_amount_string).
/// The raw amount avoids float precision loss for exact transfers.
pub async fn get_usdc_balance_raw(wallet: &str, rpc_url: Option<&str>) -> Result<(f64, String)> {
    let accounts: GetTokenAccountsResult = rpc_call_simple(
        "getTokenAccountsByOwner",
        serde_json::json!([wallet, { "mint": USDC_MINT }, { "encoding": "jsonParsed", "commitment": "confirmed" }]),
        rpc_url,
        RpcMode::Race,
    ).await?;
    let parsed = accounts.accounts();
    sum_matching_raw_amounts(&parsed, USDC_MINT)
}

/// A Solana SPL token balance.
#[derive(Debug, Clone)]
pub struct SolanaTokenBalance {
    pub symbol: String,
    pub mint: Mint,
    pub balance: f64,
    /// Raw on-chain amount as string (no float precision loss).
    pub raw_amount: String,
}

fn raw_amount_to_f64(raw: &str, decimals: u8) -> Result<f64> {
    amounts::format_amount(raw, decimals)
        .parse()
        .map_err(|_| eyre!("Invalid on-chain amount: {raw}"))
}

/// Build a mint → token entry lookup from a token list.
fn build_mint_map(tokens: &[GmTokenEntry]) -> std::collections::HashMap<&str, &GmTokenEntry> {
    tokens.iter()
        .filter_map(|t| t.solana_address.map(|sol| (sol, t)))
        .collect()
}

/// Parse GM token balances from parsed token account entries, matching against known tokens.
fn parse_gm_balances(accounts: &[TokenAccountInfo], mint_map: &std::collections::HashMap<&str, &GmTokenEntry>) -> Vec<SolanaTokenBalance> {
    let mut balances: std::collections::BTreeMap<String, SolanaTokenBalance> = std::collections::BTreeMap::new();
    for acc in accounts {
        let info = &acc.account.data.parsed.info;
        let raw_amount: u128 = match info.token_amount.amount.parse() {
            Ok(raw) => raw,
            Err(_) => continue,
        };
        if raw_amount == 0 {
            continue;
        }
        let amount = match info.token_amount.ui_amount {
            Some(amount) => amount,
            None => match raw_amount_to_f64(&info.token_amount.amount, info.token_amount.decimals) {
                Ok(amount) => amount,
                Err(_) => continue,
            },
        };
        if let Some(entry) = mint_map.get(info.mint.as_str()) {
            let balance = balances.entry(entry.symbol.to_string()).or_insert_with(|| SolanaTokenBalance {
                symbol: entry.symbol.to_string(),
                mint: Mint::from(info.mint.as_str()),
                balance: 0.0,
                raw_amount: "0".to_string(),
            });
            let current_raw: u128 = match balance.raw_amount.parse() {
                Ok(raw) => raw,
                Err(_) => continue,
            };
            balance.balance += amount;
            balance.raw_amount = current_raw.saturating_add(raw_amount).to_string();
        }
    }
    balances.into_values().collect()
}

fn sum_matching_raw_amounts(accounts: &[TokenAccountInfo], mint: &str) -> Result<(f64, String)> {
    let mut ui_total = 0.0;
    let mut raw_total = 0u128;

    for acc in accounts {
        let info = &acc.account.data.parsed.info;
        if info.mint != mint {
            continue;
        }
        let raw: u128 = info.token_amount.amount.parse()
            .map_err(|_| eyre!("Invalid on-chain amount for mint {mint}: {}", info.token_amount.amount))?;
        if raw == 0 {
            continue;
        }
        let ui = match info.token_amount.ui_amount {
            Some(ui) => ui,
            None => raw_amount_to_f64(&info.token_amount.amount, info.token_amount.decimals)?,
        };
        ui_total += ui;
        raw_total = raw_total
            .checked_add(raw)
            .ok_or_else(|| eyre!("On-chain amount overflow while summing balances for mint {mint}"))?;
    }

    Ok((ui_total, raw_total.to_string()))
}

/// Fetch all GM token balances for a Solana wallet.
pub async fn get_all_balances(
    wallet: &str,
    tokens: &[GmTokenEntry],
    rpc_url: Option<&str>,
) -> Result<Vec<SolanaTokenBalance>> {
    let accounts: GetTokenAccountsResult = rpc_call_simple(
        "getTokenAccountsByOwner",
        serde_json::json!([wallet, { "programId": TOKEN_2022_PROGRAM }, { "encoding": "jsonParsed", "commitment": "confirmed" }]),
        rpc_url,
        RpcMode::Race,
    ).await?;
    let mint_map = build_mint_map(tokens);
    Ok(parse_gm_balances(&accounts.accounts(), &mint_map))
}

/// Fetch balance for a specific GM token on Solana.
pub async fn get_balance(
    wallet: &str,
    mint: &Mint,
    rpc_url: Option<&str>,
) -> Result<SolanaTokenBalance> {
    let accounts: GetTokenAccountsResult = rpc_call_simple(
        "getTokenAccountsByOwner",
        serde_json::json!([wallet, { "mint": mint.as_ref() }, { "encoding": "jsonParsed", "commitment": "confirmed" }]),
        rpc_url,
        RpcMode::Race,
    ).await?;

    let parsed = accounts.accounts();
    let (balance, raw_amount) = sum_matching_raw_amounts(&parsed, mint)?;
    if raw_amount == "0" {
        return Err(NoTokenAccount { mint: mint.to_string() }.into());
    }
    Ok(SolanaTokenBalance {
        symbol: String::new(),
        mint: mint.clone(),
        balance,
        raw_amount,
    })
}

/// Raised when the wallet has no associated token account for a mint — for
/// agents this means "no position", not an RPC failure. Typed so
/// `classify_error` can label it instead of leaving `error_kind: null`.
#[derive(Debug)]
pub struct NoTokenAccount {
    pub mint: String,
}

impl std::fmt::Display for NoTokenAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "No token account found for mint {}", self.mint)
    }
}

impl std::error::Error for NoTokenAccount {}

/// Where a `PortfolioBalances` came from. `Jupiter` means the Solana RPC read
/// failed and we fell back to the Jupiter Ultra holdings API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BalanceSource {
    #[default]
    SolanaRpc,
    Jupiter,
}

/// Portfolio balances — SOL + USDC + all GM tokens.
/// Uses sequential RPC calls to avoid rate limiting on public Solana endpoints.
#[derive(Debug)]
pub struct PortfolioBalances {
    pub sol: f64,
    pub usdc: f64,
    pub gm_tokens: Vec<SolanaTokenBalance>,
    pub source: BalanceSource,
}

pub async fn get_portfolio_balances(
    wallet: &str,
    tokens: &[GmTokenEntry],
    rpc_url: Option<&str>,
) -> Result<PortfolioBalances> {
    let urls = rpc_urls(rpc_url);
    portfolio_balances_against(wallet, tokens, &urls).await
}

/// Test-only: run portfolio balance fetching against a caller-supplied URL list.
/// Enables multi-URL race testing in integration tests.
#[cfg(any(test, feature = "test-util"))]
pub async fn get_portfolio_balances_with_urls(
    wallet: &str,
    tokens: &[GmTokenEntry],
    urls: &[&str],
) -> Result<PortfolioBalances> {
    portfolio_balances_against(wallet, tokens, urls).await
}

/// True if the error (or any source in its chain) is a Solana RPC `Unavailable`
/// — the "all endpoints failed" signal that warrants the Jupiter fallback
/// (applied one layer up, in `usecases::gm::fetch_portfolio_balances`).
pub(crate) fn is_rpc_unavailable(err: &eyre::Error) -> bool {
    use super::rpc::{SolanaRpcError, SolanaRpcErrorKind};
    err.chain().any(|c| {
        c.downcast_ref::<SolanaRpcError>()
            .is_some_and(|e| e.kind == SolanaRpcErrorKind::Unavailable)
    })
}

async fn portfolio_balances_against(
    wallet: &str,
    tokens: &[GmTokenEntry],
    urls: &[&str],
) -> Result<PortfolioBalances> {
    let client = &*crate::HTTP;

    // Compute USDC ATA address deterministically (no RPC needed).
    let wallet_bytes = bs58::decode(wallet).into_vec()
        .map_err(|e| eyre!("Invalid wallet address: {e}"))?;
    let usdc_mint_bytes = bs58::decode(USDC_MINT).into_vec()
        .map_err(|e| eyre!("Invalid USDC mint: {e}"))?;
    let usdc_ata = derive_ata(&wallet_bytes, &usdc_mint_bytes, &TOKEN_PROGRAM)?;
    let usdc_ata_b58 = bs58::encode(&usdc_ata).into_string();

    // Batch 2 RPC calls into 1 HTTP request:
    //   1. getMultipleAccounts([wallet, usdc_ata]) → SOL + USDC in one call
    //   2. getTokenAccountsByOwner(Token-2022) → all GM tokens in one call
    let reqs = vec![
        RpcRequest {
            jsonrpc: "2.0", id: 1, method: "getMultipleAccounts",
            params: serde_json::json!([
                [wallet, usdc_ata_b58],
                { "encoding": "jsonParsed", "commitment": "confirmed" }
            ]),
        },
        RpcRequest {
            jsonrpc: "2.0", id: 2, method: "getTokenAccountsByOwner",
            params: serde_json::json!([wallet, { "programId": TOKEN_2022_PROGRAM }, { "encoding": "jsonParsed", "commitment": "confirmed" }]),
        },
    ];

    let results = rpc_batch_with_retry(client, urls, &reqs, RpcMode::Race).await?;

    // Parse SOL + USDC from getMultipleAccounts
    let multi_resp: RpcResponse<GetMultipleAccountsResult> = serde_json::from_value(results[0].clone())
        .map_err(|e| eyre!("Failed to parse accounts: {e}"))?;
    let (sol, usdc) = if let Some(multi) = multi_resp.result {
        // First account = wallet → lamports = SOL balance
        let sol = multi.value.first()
            .and_then(|v| v.as_ref())
            .map(|a| a.lamports as f64 / 1_000_000_000.0)
            .unwrap_or(0.0);
        // Second account = USDC ATA → parsed token data
        let usdc = multi.value.get(1)
            .and_then(|v| v.as_ref())
            .and_then(|a| a.token_ui_amount())
            .unwrap_or(0.0);
        (sol, usdc)
    } else {
        (0.0, 0.0)
    };

    // Parse GM token balances (Token-2022)
    let gm_resp: RpcResponse<GetTokenAccountsResult> = serde_json::from_value(results[1].clone())
        .map_err(|e| eyre!("Failed to parse GM token balances: {e}"))?;

    let mint_map = build_mint_map(tokens);
    let gm_tokens = gm_resp.result
        .map(|accounts| parse_gm_balances(&accounts.accounts(), &mint_map))
        .unwrap_or_default();

    Ok(PortfolioBalances { sol, usdc, gm_tokens, source: BalanceSource::SolanaRpc })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_rpc_unavailable_detects_unavailable_through_chain() {
        use eyre::WrapErr;
        use super::super::rpc::{SolanaRpcError, SolanaRpcErrorKind};

        // Unavailable, even wrapped, is detected via the chain walk.
        let err: eyre::Error = SolanaRpcError::new(
            SolanaRpcErrorKind::Unavailable, None, None, None::<reqwest::StatusCode>, None, "all endpoints failed",
        )
        .into();
        let wrapped = Err::<(), eyre::Error>(err).wrap_err("portfolio batch").unwrap_err();
        assert!(super::is_rpc_unavailable(&wrapped));

        // A DIFFERENT kind must NOT match (guards against checking the wrong variant).
        let net: eyre::Error = SolanaRpcError::new(
            SolanaRpcErrorKind::Network, None, None, None::<reqwest::StatusCode>, None, "net blip",
        )
        .into();
        assert!(!super::is_rpc_unavailable(&net));

        // An opaque non-RPC error must NOT match.
        let other: eyre::Error = eyre::eyre!("some other failure");
        assert!(!super::is_rpc_unavailable(&other));
    }

    #[test]
    fn parse_gm_balances_merges_multiple_accounts_for_same_mint() {
        let accounts: GetTokenAccountsResult = serde_json::from_value(serde_json::json!({
            "value": [
                {
                    "pubkey": "Account1111111111111111111111111111111111111",
                    "account": {
                        "lamports": 1,
                        "data": {
                            "parsed": {
                                "info": {
                                    "mint": "Mint11111111111111111111111111111111111111",
                                    "tokenAmount": { "uiAmount": 1.25, "amount": "1250000000", "decimals": 9 }
                                }
                            }
                        }
                    }
                },
                {
                    "pubkey": "Account2222222222222222222222222222222222222",
                    "account": {
                        "lamports": 1,
                        "data": {
                            "parsed": {
                                "info": {
                                    "mint": "Mint11111111111111111111111111111111111111",
                                    "tokenAmount": { "uiAmount": 0.75, "amount": "750000000", "decimals": 9 }
                                }
                            }
                        }
                    }
                }
            ]
        })).unwrap();
        let entry = GmTokenEntry {
            symbol: "TESTon",
            solana_address: Some("Mint11111111111111111111111111111111111111"),
        };
        let mint_map = std::collections::HashMap::from([("Mint11111111111111111111111111111111111111", &entry)]);

        let balances = parse_gm_balances(&accounts.accounts(), &mint_map);
        assert_eq!(balances.len(), 1);
        assert_eq!(balances[0].symbol, "TESTon");
        assert!((balances[0].balance - 2.0).abs() < 1e-9);
        assert_eq!(balances[0].raw_amount, "2000000000");
    }

    #[test]
    fn sum_matching_raw_amounts_aggregates_all_accounts() {
        let accounts: GetTokenAccountsResult = serde_json::from_value(serde_json::json!({
            "value": [
                {
                    "pubkey": "Account1111111111111111111111111111111111111",
                    "account": {
                        "lamports": 1,
                        "data": {
                            "parsed": {
                                "info": {
                                    "mint": USDC_MINT,
                                    "tokenAmount": { "uiAmount": 1.5, "amount": "1500000", "decimals": 6 }
                                }
                            }
                        }
                    }
                },
                {
                    "pubkey": "Account2222222222222222222222222222222222222",
                    "account": {
                        "lamports": 1,
                        "data": {
                            "parsed": {
                                "info": {
                                    "mint": USDC_MINT,
                                    "tokenAmount": { "uiAmount": 2.25, "amount": "2250000", "decimals": 6 }
                                }
                            }
                        }
                    }
                }
            ]
        })).unwrap();

        let (ui, raw) = sum_matching_raw_amounts(&accounts.accounts(), USDC_MINT).unwrap();
        assert!((ui - 3.75).abs() < 1e-9);
        assert_eq!(raw, "3750000");
    }

    #[test]
    fn parse_gm_balances_derives_ui_amount_when_rpc_omits_it() {
        let accounts: GetTokenAccountsResult = serde_json::from_value(serde_json::json!({
            "value": [
                {
                    "pubkey": "Account1111111111111111111111111111111111111",
                    "account": {
                        "lamports": 1,
                        "data": {
                            "parsed": {
                                "info": {
                                    "mint": "Mint11111111111111111111111111111111111111",
                                    "tokenAmount": { "uiAmount": null, "amount": "1250000000", "decimals": 9 }
                                }
                            }
                        }
                    }
                }
            ]
        })).unwrap();
        let entry = GmTokenEntry {
            symbol: "TESTon",
            solana_address: Some("Mint11111111111111111111111111111111111111"),
        };
        let mint_map = std::collections::HashMap::from([("Mint11111111111111111111111111111111111111", &entry)]);

        let balances = parse_gm_balances(&accounts.accounts(), &mint_map);
        assert_eq!(balances.len(), 1);
        assert!((balances[0].balance - 1.25).abs() < 1e-9);
        assert_eq!(balances[0].raw_amount, "1250000000");
    }

    #[test]
    fn sum_matching_raw_amounts_derives_ui_amount_when_rpc_omits_it() {
        let accounts: GetTokenAccountsResult = serde_json::from_value(serde_json::json!({
            "value": [
                {
                    "pubkey": "Account1111111111111111111111111111111111111",
                    "account": {
                        "lamports": 1,
                        "data": {
                            "parsed": {
                                "info": {
                                    "mint": USDC_MINT,
                                    "tokenAmount": { "uiAmount": null, "amount": "1500000", "decimals": 6 }
                                }
                            }
                        }
                    }
                }
            ]
        })).unwrap();

        let (ui, raw) = sum_matching_raw_amounts(&accounts.accounts(), USDC_MINT).unwrap();
        assert!((ui - 1.5).abs() < 1e-9);
        assert_eq!(raw, "1500000");
    }

    #[test]
    fn multi_account_info_derives_ui_amount_when_missing() {
        let account: MultiAccountInfo = serde_json::from_value(serde_json::json!({
            "lamports": 0,
            "data": {
                "parsed": {
                    "info": {
                        "tokenAmount": {
                            "uiAmount": null,
                            "amount": "4200000",
                            "decimals": 6
                        }
                    }
                }
            }
        })).unwrap();

        assert_eq!(account.token_ui_amount(), Some(4.2));
    }

    #[test]
    fn solana_token_balance_mint_is_mint_newtype() {
        // This test only compiles once mint: Mint (not String).
        // Also verifies Deref coercion to &str still works.
        let b = SolanaTokenBalance {
            symbol: String::new(),
            mint: Mint::from("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"),
            balance: 0.0,
            raw_amount: "0".to_string(),
        };
        assert_eq!(&*b.mint, "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
    }
}
