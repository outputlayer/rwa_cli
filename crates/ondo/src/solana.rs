use eyre::{Result, eyre};
use serde::{Deserialize, Serialize};

use crate::token_list::GmTokenEntry;

/// Public Solana RPC endpoints — rotated on rate-limit errors.
const RPC_URLS: &[&str] = &[
    "https://api.mainnet-beta.solana.com",
    "https://rpc.ankr.com/solana",
    "https://solana-mainnet.g.alchemy.com/v2/demo",
];

/// Return the list of RPC URLs to try: user-provided first, then public fallbacks.
fn rpc_urls(custom: Option<&str>) -> Vec<&str> {
    match custom {
        Some(url) => {
            let mut urls = vec![url];
            for u in RPC_URLS {
                if *u != url {
                    urls.push(u);
                }
            }
            urls
        }
        None => RPC_URLS.to_vec(),
    }
}
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
    let client = reqwest::Client::new();
    let req = RpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "getBalance",
        params: serde_json::json!([wallet]),
    };

    let resp: RpcResponse<GetBalanceResult> = rpc_call_with_retry(
        &client, &rpc_urls(rpc_url), &req,
    ).await?;

    let result = resp.result
        .ok_or_else(|| eyre!("Empty response from Solana RPC"))?;

    Ok(result.value as f64 / 1_000_000_000.0)
}

/// Get USDC balance for a wallet on Solana.
pub async fn get_usdc_balance(wallet: &str, rpc_url: Option<&str>) -> Result<f64> {
    let client = reqwest::Client::new();
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

    let resp: RpcResponse<GetTokenAccountsResult> = rpc_call_with_retry(
        &client, &rpc_urls(rpc_url), &req,
    ).await?;

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
    let client = reqwest::Client::new();
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

    let resp: RpcResponse<GetTokenAccountsResult> = rpc_call_with_retry(
        &client, &rpc_urls(rpc_url), &req,
    ).await?;

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
    let client = reqwest::Client::new();
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

    let resp: RpcResponse<GetTokenAccountsResult> = rpc_call_with_retry(
        &client, &rpc_urls(rpc_url), &req,
    ).await?;

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
    let client = reqwest::Client::new();

    // Sequential calls to avoid public RPC rate limiting (429 errors).
    // Reuse the same client for HTTP connection pooling.

    // 1. SOL balance
    let sol_resp: RpcResponse<GetBalanceResult> = rpc_call_with_retry(
        &client, &urls,
        &RpcRequest { jsonrpc: "2.0", id: 1, method: "getBalance", params: serde_json::json!([wallet]) },
    ).await?;
    let sol = sol_resp.result
        .map(|r| r.value as f64 / 1_000_000_000.0)
        .unwrap_or(0.0);

    // 2. USDC balance
    let usdc_resp: RpcResponse<GetTokenAccountsResult> = rpc_call_with_retry(
        &client, &urls,
        &RpcRequest {
            jsonrpc: "2.0", id: 2, method: "getTokenAccountsByOwner",
            params: serde_json::json!([wallet, { "mint": USDC_MINT }, { "encoding": "jsonParsed" }]),
        },
    ).await?;
    let usdc = usdc_resp.result
        .and_then(|r| r.value.first().and_then(|a| a.account.data.parsed.info.token_amount.ui_amount))
        .unwrap_or(0.0);

    // 3. GM token balances (Token-2022)
    let gm_resp: RpcResponse<GetTokenAccountsResult> = rpc_call_with_retry(
        &client, &urls,
        &RpcRequest {
            jsonrpc: "2.0", id: 3, method: "getTokenAccountsByOwner",
            params: serde_json::json!([wallet, { "programId": TOKEN_2022_PROGRAM }, { "encoding": "jsonParsed" }]),
        },
    ).await?;

    let mint_map: std::collections::HashMap<&str, &GmTokenEntry> = tokens
        .iter()
        .filter_map(|t| t.solana_address.as_deref().map(|sol| (sol, t)))
        .collect();

    let mut gm_tokens = Vec::new();
    if let Some(accounts) = gm_resp.result {
        for acc in &accounts.value {
            let info = &acc.account.data.parsed.info;
            let amount = info.token_amount.ui_amount.unwrap_or(0.0);
            if amount <= 0.0 {
                continue;
            }
            if let Some(entry) = mint_map.get(info.mint.as_str()) {
                gm_tokens.push(SolanaTokenBalance {
                    symbol: entry.symbol.clone(),
                    mint: info.mint.clone(),
                    balance: amount,
                });
            }
        }
    }

    gm_tokens.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    Ok(PortfolioBalances { sol, usdc, gm_tokens })
}

/// Make an RPC call with retry and RPC URL rotation on rate-limit errors.
/// Tries each URL with backoff before rotating to the next one.
async fn rpc_call_with_retry<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    urls: &[&str],
    req: &RpcRequest<'_>,
) -> Result<RpcResponse<T>> {
    let timeout = std::time::Duration::from_secs(15);
    let mut last_err = String::new();

    for (url_idx, url) in urls.iter().enumerate() {
        for attempt in 0..3u32 {
            if attempt > 0 || url_idx > 0 {
                let delay = 400 * u64::from(attempt + 1);
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            let resp = client
                .post(*url)
                .json(req)
                .timeout(timeout)
                .send().await;

            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    last_err = e.to_string();
                    break; // try next URL on connection error
                }
            };

            let parsed: RpcResponse<T> = match resp.json().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = e.to_string();
                    break;
                }
            };

            if let Some(ref err) = parsed.error {
                if err.message.contains("Too many requests") {
                    last_err = err.message.clone();
                    continue; // retry same URL with backoff
                }
                return Err(eyre!("Solana RPC error: {}", err.message));
            }
            return Ok(parsed);
        }
    }

    Err(eyre!(
        "Solana RPC unavailable (all endpoints rate-limited or down).\n  \
         Last error: {last_err}\n  \
         Hint: set RWA_RPC_URL to a private RPC endpoint, or retry in a few seconds."
    ))
}
