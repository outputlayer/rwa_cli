mod rpc;
mod fee;
mod transaction;
mod transfer;

use eyre::{Result, eyre};
use serde::Deserialize;

use crate::token_list::GmTokenEntry;
use rpc::{rpc_call_simple, rpc_batch_with_retry, rpc_urls, RpcRequest, RpcResponse};
use transfer::{derive_ata, TOKEN_PROGRAM};

// Re-export public API
pub use crate::USDC_MINT;
pub use fee::{estimate_tx_fee, estimate_gas_needed};
pub use transaction::{TransactionResult, confirm_transaction};
pub use transfer::{
    transfer_sol, transfer_spl,
    EmptyTokenAccount, get_empty_token_accounts, close_empty_accounts,
};

/// Ondo GM tokens use Token-2022 (Token Extensions) on Solana.
const TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

/// Validate a Solana base58 address (32-44 chars, valid base58, decodes to 32 bytes).
pub fn validate_address(addr: &str) -> Result<()> {
    if addr.len() < 32 || addr.len() > 44 || addr.chars().any(|c| c.is_whitespace()) {
        return Err(eyre!("Invalid Solana address: {addr}"));
    }
    let bytes = bs58::decode(addr).into_vec()
        .map_err(|e| eyre!("Invalid Solana address (bad base58): {e}"))?;
    if bytes.len() != 32 {
        return Err(eyre!("Invalid Solana address: expected 32 bytes, got {}", bytes.len()));
    }
    Ok(())
}

// ── RPC deserialization types ────────────────────────────────

#[derive(Deserialize)]
struct GetTokenAccountsResult {
    value: Vec<TokenAccountInfo>,
}

#[derive(Deserialize)]
struct TokenAccountInfo {
    pubkey: Option<String>,
    account: AccountData,
}

#[derive(Deserialize)]
struct AccountData {
    lamports: Option<u64>,
    data: ParsedData,
}

#[derive(Deserialize)]
struct ParsedData {
    parsed: ParsedTokenData,
}

#[derive(Deserialize)]
struct ParsedTokenData {
    info: TokenInfo,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenInfo {
    mint: String,
    token_amount: TokenAmount,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenAmount {
    ui_amount: Option<f64>,
    amount: String,
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
    lamports: u64,
    data: serde_json::Value,
}

impl MultiAccountInfo {
    /// Try to extract token amount from parsed SPL token data.
    fn token_ui_amount(&self) -> Option<f64> {
        self.data.get("parsed")
            .and_then(|p| p.get("info"))
            .and_then(|i| i.get("tokenAmount"))
            .and_then(|t| t.get("uiAmount"))
            .and_then(|v| v.as_f64())
    }
}

// ── Balance queries ──────────────────────────────────────────

/// Get SOL balance in SOL (not lamports).
pub async fn get_sol_balance(wallet: &str, rpc_url: Option<&str>) -> Result<f64> {
    let result: GetBalanceResult = rpc_call_simple(
        "getBalance",
        serde_json::json!([wallet, { "commitment": "confirmed" }]),
        rpc_url,
    ).await?;
    Ok(result.value as f64 / 1_000_000_000.0)
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
    ).await?;
    match accounts.value.first() {
        Some(acc) => {
            let ta = &acc.account.data.parsed.info.token_amount;
            Ok((ta.ui_amount.unwrap_or(0.0), ta.amount.clone()))
        }
        None => Ok((0.0, "0".to_string())),
    }
}

/// A Solana SPL token balance.
#[derive(Debug, Clone)]
pub struct SolanaTokenBalance {
    pub symbol: String,
    pub mint: String,
    pub balance: f64,
    /// Raw on-chain amount as string (no float precision loss).
    pub raw_amount: String,
}

/// Build a mint → token entry lookup from a token list.
fn build_mint_map(tokens: &[GmTokenEntry]) -> std::collections::HashMap<&str, &GmTokenEntry> {
    tokens.iter()
        .filter_map(|t| t.solana_address.map(|sol| (sol, t)))
        .collect()
}

/// Parse GM token balances from RPC response, matching against known tokens.
fn parse_gm_balances(accounts: &GetTokenAccountsResult, mint_map: &std::collections::HashMap<&str, &GmTokenEntry>) -> Vec<SolanaTokenBalance> {
    let mut balances = Vec::new();
    for acc in &accounts.value {
        let info = &acc.account.data.parsed.info;
        let amount = info.token_amount.ui_amount.unwrap_or(0.0);
        if amount <= 0.0 {
            continue;
        }
        if let Some(entry) = mint_map.get(info.mint.as_str()) {
            balances.push(SolanaTokenBalance {
                symbol: entry.symbol.to_string(),
                mint: info.mint.clone(),
                balance: amount,
                raw_amount: info.token_amount.amount.clone(),
            });
        }
    }
    balances.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    balances
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
    ).await?;
    let mint_map = build_mint_map(tokens);
    Ok(parse_gm_balances(&accounts, &mint_map))
}

/// Fetch balance for a specific GM token on Solana.
pub async fn get_balance(
    wallet: &str,
    mint: &str,
    rpc_url: Option<&str>,
) -> Result<SolanaTokenBalance> {
    let accounts: GetTokenAccountsResult = rpc_call_simple(
        "getTokenAccountsByOwner",
        serde_json::json!([wallet, { "mint": mint }, { "encoding": "jsonParsed", "commitment": "confirmed" }]),
        rpc_url,
    ).await?;

    let acc = accounts.value.first()
        .ok_or_else(|| eyre!("No token account found for mint {mint}"))?;

    let info = &acc.account.data.parsed.info;
    Ok(SolanaTokenBalance {
        symbol: String::new(),
        mint: info.mint.clone(),
        balance: info.token_amount.ui_amount.unwrap_or(0.0),
        raw_amount: info.token_amount.amount.clone(),
    })
}

/// Portfolio balances — SOL + USDC + all GM tokens.
/// Uses sequential RPC calls to avoid rate limiting on public Solana endpoints.
pub struct PortfolioBalances {
    pub sol: f64,
    pub usdc: f64,
    pub gm_tokens: Vec<SolanaTokenBalance>,
}

pub async fn get_portfolio_balances(
    wallet: &str,
    tokens: &[GmTokenEntry],
    rpc_url: Option<&str>,
) -> Result<PortfolioBalances> {
    let urls = rpc_urls(rpc_url);
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

    let results = rpc_batch_with_retry(client, &urls, &reqs).await?;

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
        .map(|accounts| parse_gm_balances(&accounts, &mint_map))
        .unwrap_or_default();

    Ok(PortfolioBalances { sol, usdc, gm_tokens })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_valid_address() {
        // Real Solana address (System Program)
        assert!(validate_address("11111111111111111111111111111111").is_ok());
    }

    #[test]
    fn validate_real_wallet_address() {
        assert!(validate_address("5CjgV1J2FE8yyxsHKGs2v4GJULBS7AiYtRo7DFYiuZ47").is_ok());
    }

    #[test]
    fn validate_usdc_mint() {
        assert!(validate_address("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").is_ok());
    }

    #[test]
    fn reject_too_short() {
        assert!(validate_address("abc").is_err());
    }

    #[test]
    fn reject_too_long() {
        assert!(validate_address(&"1".repeat(50)).is_err());
    }

    #[test]
    fn reject_whitespace() {
        assert!(validate_address("5CjgV1J2FE8yyxsHKGs2v4GJULBS7AiY tRo7DFYiuZ47").is_err());
    }

    #[test]
    fn reject_invalid_base58() {
        // 'O' and 'I' are not in base58 alphabet
        assert!(validate_address("OOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOO").is_err());
    }

    #[test]
    fn reject_empty() {
        assert!(validate_address("").is_err());
    }
}
