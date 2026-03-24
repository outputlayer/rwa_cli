use eyre::{Result, eyre};
use serde::{Deserialize, Serialize};

use crate::token_list::GmTokenEntry;

const SOLANA_RPC_URL: &str = "https://api.mainnet-beta.solana.com";
/// Ondo GM tokens use Token-2022 (Token Extensions) on Solana.
const TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
/// USDC on Solana
pub const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

#[derive(Serialize)]
struct RpcRequest<'a> {
    jsonrpc: &'a str,
    id: u64,
    method: &'a str,
    params: serde_json::Value,
}

#[derive(Deserialize)]
struct RpcResponse<T> {
    result: Option<T>,
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    message: String,
}

#[derive(Deserialize)]
struct GetTokenAccountsResult {
    value: Vec<TokenAccountInfo>,
}

#[derive(Deserialize)]
struct TokenAccountInfo {
    account: AccountData,
}

#[derive(Deserialize)]
struct AccountData {
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
}

#[derive(Deserialize)]
struct GetBalanceResult {
    value: u64,
}

/// Get SOL balance in SOL (not lamports).
pub async fn get_sol_balance(wallet: &str, rpc_url: Option<&str>) -> Result<f64> {
    let url = rpc_url.unwrap_or(SOLANA_RPC_URL);

    let req = RpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "getBalance",
        params: serde_json::json!([wallet]),
    };

    let resp: RpcResponse<GetBalanceResult> = reqwest::Client::new()
        .post(url)
        .json(&req)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await?
        .json()
        .await?;

    if let Some(err) = resp.error {
        return Err(eyre!("Solana RPC error: {}", err.message));
    }

    let result = resp.result
        .ok_or_else(|| eyre!("Empty response from Solana RPC"))?;

    Ok(result.value as f64 / 1_000_000_000.0)
}

/// Get USDC balance for a wallet on Solana.
pub async fn get_usdc_balance(wallet: &str, rpc_url: Option<&str>) -> Result<f64> {
    let url = rpc_url.unwrap_or(SOLANA_RPC_URL);

    let req = RpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "getTokenAccountsByOwner",
        params: serde_json::json!([
            wallet,
            { "mint": USDC_MINT },
            { "encoding": "jsonParsed" }
        ]),
    };

    let resp: RpcResponse<GetTokenAccountsResult> = reqwest::Client::new()
        .post(url)
        .json(&req)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await?
        .json()
        .await?;

    if let Some(err) = resp.error {
        return Err(eyre!("Solana RPC error: {}", err.message));
    }

    let accounts = resp.result
        .ok_or_else(|| eyre!("Empty response from Solana RPC"))?;

    match accounts.value.first() {
        Some(acc) => Ok(acc.account.data.parsed.info.token_amount.ui_amount.unwrap_or(0.0)),
        None => Ok(0.0),
    }
}

/// A Solana SPL token balance.
#[derive(Debug, Clone)]
pub struct SolanaTokenBalance {
    pub symbol: String,
    pub mint: String,
    pub balance: f64,
}

/// Fetch all GM token balances for a Solana wallet.
pub async fn get_all_balances(
    wallet: &str,
    tokens: &[GmTokenEntry],
    rpc_url: Option<&str>,
) -> Result<Vec<SolanaTokenBalance>> {
    let url = rpc_url.unwrap_or(SOLANA_RPC_URL);

    // Ondo GM tokens use Token-2022 (Token Extensions) on Solana
    let req = RpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "getTokenAccountsByOwner",
        params: serde_json::json!([
            wallet,
            { "programId": TOKEN_2022_PROGRAM },
            { "encoding": "jsonParsed" }
        ]),
    };

    let resp: RpcResponse<GetTokenAccountsResult> = reqwest::Client::new()
        .post(url)
        .json(&req)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await?
        .json()
        .await?;

    if let Some(err) = resp.error {
        return Err(eyre!("Solana RPC error: {}", err.message));
    }

    let accounts = resp.result
        .ok_or_else(|| eyre!("Empty response from Solana RPC"))?;

    // Build a mint → token entry lookup
    let mint_map: std::collections::HashMap<&str, &GmTokenEntry> = tokens
        .iter()
        .filter_map(|t| t.solana_address.as_deref().map(|sol| (sol, t)))
        .collect();

    let mut balances = Vec::new();
    for acc in &accounts.value {
        let info = &acc.account.data.parsed.info;
        let amount = info.token_amount.ui_amount.unwrap_or(0.0);
        if amount <= 0.0 {
            continue;
        }

        if let Some(entry) = mint_map.get(info.mint.as_str()) {
            balances.push(SolanaTokenBalance {
                symbol: entry.symbol.clone(),
                mint: info.mint.clone(),
                balance: amount,
            });
        }
    }

    balances.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    Ok(balances)
}

/// Fetch balance for a specific GM token on Solana.
pub async fn get_balance(
    wallet: &str,
    mint: &str,
    rpc_url: Option<&str>,
) -> Result<SolanaTokenBalance> {
    let url = rpc_url.unwrap_or(SOLANA_RPC_URL);

    let req = RpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "getTokenAccountsByOwner",
        params: serde_json::json!([
            wallet,
            { "mint": mint },
            { "encoding": "jsonParsed" }
        ]),
    };

    let resp: RpcResponse<GetTokenAccountsResult> = reqwest::Client::new()
        .post(url)
        .json(&req)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await?
        .json()
        .await?;

    if let Some(err) = resp.error {
        return Err(eyre!("Solana RPC error: {}", err.message));
    }

    let accounts = resp.result
        .ok_or_else(|| eyre!("Empty response from Solana RPC"))?;

    let acc = accounts.value.first()
        .ok_or_else(|| eyre!("No token account found for mint {mint}"))?;

    let info = &acc.account.data.parsed.info;
    Ok(SolanaTokenBalance {
        symbol: String::new(),
        mint: info.mint.clone(),
        balance: info.token_amount.ui_amount.unwrap_or(0.0),
    })
}
