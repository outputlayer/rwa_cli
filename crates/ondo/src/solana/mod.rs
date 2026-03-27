mod rpc;

use eyre::{Result, eyre};
use serde::Deserialize;

use crate::token_list::GmTokenEntry;
use crate::wallet::Wallet;
use crate::HTTP;
use rpc::{rpc_call_simple, rpc_batch_with_retry, rpc_urls, RpcRequest, RpcResponse};

/// Result of sending a transaction — includes whether it was confirmed on-chain.
pub struct TransactionResult {
    /// Base58-encoded transaction signature.
    pub signature: String,
    /// Whether the transaction was confirmed within the polling timeout.
    /// `false` means the tx was sent but confirmation timed out — it may still land.
    pub confirmed: bool,
}

/// Ondo GM tokens use Token-2022 (Token Extensions) on Solana.
const TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
pub use crate::USDC_MINT;

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

// ── getMultipleAccounts response (flexible: wallet + token accounts) ───

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

/// Get SOL balance in SOL (not lamports).
pub async fn get_sol_balance(wallet: &str, rpc_url: Option<&str>) -> Result<f64> {
    let result: GetBalanceResult = rpc_call_simple(
        "getBalance",
        serde_json::json!([wallet, { "commitment": "confirmed" }]),
        rpc_url,
    ).await?;
    Ok(result.value as f64 / 1_000_000_000.0)
}

/// Base fee per signature (5000 lamports, protocol constant).
const BASE_FEE_LAMPORTS: u64 = 5_000;

/// Fetch median recent priority fee from RPC.
/// Passes writable account addresses for more accurate fee estimates (per Solana docs).
async fn fetch_priority_fee(writable_accounts: &[&str], rpc_url: Option<&str>) -> Result<u64> {
    let params = if writable_accounts.is_empty() {
        serde_json::json!([])
    } else {
        serde_json::json!([writable_accounts])
    };
    let entries: Vec<PriorityFeeEntry> = rpc_call_simple(
        "getRecentPrioritizationFees",
        params,
        rpc_url,
    ).await.unwrap_or_default();
    if entries.is_empty() {
        return Ok(0);
    }
    let mut fees: Vec<u64> = entries.iter().map(|e| e.prioritization_fee).collect();
    fees.sort_unstable();
    Ok(fees[fees.len() / 2]) // median
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PriorityFeeEntry {
    prioritization_fee: u64,
    #[allow(dead_code)]
    slot: Option<u64>,
}

/// Get USDC balance for a wallet on Solana.
pub async fn get_usdc_balance(wallet: &str, rpc_url: Option<&str>) -> Result<f64> {
    let accounts: GetTokenAccountsResult = rpc_call_simple(
        "getTokenAccountsByOwner",
        serde_json::json!([wallet, { "mint": USDC_MINT }, { "encoding": "jsonParsed", "commitment": "confirmed" }]),
        rpc_url,
    ).await?;
    match accounts.value.first() {
        Some(acc) => Ok(acc.account.data.parsed.info.token_amount.ui_amount.unwrap_or(0.0)),
        None => Ok(0.0),
    }
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

    // Build a mint → token entry lookup
    let mint_map: std::collections::HashMap<&str, &GmTokenEntry> = tokens
        .iter()
        .filter_map(|t| t.solana_address.map(|sol| (sol, t)))
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
                symbol: entry.symbol.to_string(),
                mint: info.mint.clone(),
                balance: amount,
                raw_amount: info.token_amount.amount.clone(),
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
    let client = &*HTTP;

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

    let mint_map: std::collections::HashMap<&str, &GmTokenEntry> = tokens
        .iter()
        .filter_map(|t| t.solana_address.map(|sol| (sol, t)))
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
                    symbol: entry.symbol.to_string(),
                    mint: info.mint.clone(),
                    balance: amount,
                    raw_amount: info.token_amount.amount.clone(),
                });
            }
        }
    }

    gm_tokens.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    Ok(PortfolioBalances { sol, usdc, gm_tokens })
}

// ── Fee cache ─────────────────────────────────────────────

use std::sync::Mutex;

/// Cached priority fee: (fee_lamports, timestamp).
static FEE_CACHE: std::sync::LazyLock<Mutex<(u64, std::time::Instant)>> =
    std::sync::LazyLock::new(|| Mutex::new((BASE_FEE_LAMPORTS, std::time::Instant::now())));

/// Fee cache TTL — reuse cached fee for this duration.
const FEE_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(10);

/// Estimate the transaction fee in SOL for a simple transfer (1 signature).
/// Fetches recent priority fees from RPC with caching (10s TTL), adds 30% buffer.
pub async fn estimate_tx_fee(rpc_url: Option<&str>) -> f64 {
    let priority = get_priority_fee_cached(rpc_url).await;
    let total = BASE_FEE_LAMPORTS + priority;
    let with_buffer = (total as f64 * 1.3) as u64;
    with_buffer as f64 / 1_000_000_000.0
}

/// Rent-exempt cache: (lamports_per_byte, timestamp).
/// Cached because rent rate changes only via Solana governance vote (extremely rare).
static RENT_CACHE: std::sync::LazyLock<Mutex<(Option<u64>, std::time::Instant)>> =
    std::sync::LazyLock::new(|| Mutex::new((None, std::time::Instant::now())));

/// Cache TTL for rent-exempt values — 5 minutes (changes only via governance).
const RENT_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// SPL Token account data size (bytes).
const SPL_TOKEN_ACCOUNT_SIZE: u64 = 165;
/// Token-2022 account data size (bytes) — base size without extensions.
const TOKEN_2022_ACCOUNT_SIZE: u64 = 182;

/// Fallback rent values if RPC is unreachable.
const ATA_RENT_LAMPORTS_FALLBACK: u64 = 2_039_280;
const ATA_RENT_LAMPORTS_2022_FALLBACK: u64 = 2_165_280;

/// Fetch rent-exempt minimum from Solana RPC via `getMinimumBalanceForRentExemption`.
/// Caches the result for 5 minutes. Falls back to known values if RPC fails.
async fn get_rent_exempt_cached(data_size: u64, rpc_url: Option<&str>) -> u64 {
    // Check cache first
    if let Ok(cache) = RENT_CACHE.lock() {
        if let (Some(lamports), ts) = &*cache {
            if ts.elapsed() < RENT_CACHE_TTL && data_size == SPL_TOKEN_ACCOUNT_SIZE {
                return *lamports;
            }
        }
    }

    // Fetch from RPC
    if let Ok(lamports) = rpc_call_simple::<u64>(
        "getMinimumBalanceForRentExemption",
        serde_json::json!([data_size]),
        rpc_url,
    ).await {
        if data_size == SPL_TOKEN_ACCOUNT_SIZE {
            if let Ok(mut cache) = RENT_CACHE.lock() {
                *cache = (Some(lamports), std::time::Instant::now());
            }
        }
        return lamports;
    }

    // Fallback to known values
    match data_size {
        TOKEN_2022_ACCOUNT_SIZE => ATA_RENT_LAMPORTS_2022_FALLBACK,
        _ => ATA_RENT_LAMPORTS_FALLBACK,
    }
}

/// Estimate SOL needed for gas, optionally including ATA creation rent.
/// Uses `getMinimumBalanceForRentExemption` RPC call (cached 5 min) as Solana recommends.
///
/// - `needs_ata`: if true, includes rent-exempt minimum for an SPL token ATA.
/// - `is_token_2022`: if true, uses Token-2022 ATA size (182 bytes) for rent.
/// - Returns SOL amount with 30% safety buffer.
pub async fn estimate_gas_needed(needs_ata: bool, is_token_2022: bool, rpc_url: Option<&str>) -> f64 {
    let priority = get_priority_fee_cached(rpc_url).await;
    let mut total_lamports = BASE_FEE_LAMPORTS + priority;
    if needs_ata {
        let size = if is_token_2022 { TOKEN_2022_ACCOUNT_SIZE } else { SPL_TOKEN_ACCOUNT_SIZE };
        total_lamports += get_rent_exempt_cached(size, rpc_url).await;
    }
    // 30% buffer on total
    let with_buffer = (total_lamports as f64 * 1.3) as u64;
    with_buffer as f64 / 1_000_000_000.0
}

/// Get cached priority fee, refreshing from RPC if stale.
/// Pass writable account addresses for account-specific fee estimates.
async fn get_priority_fee_cached(rpc_url: Option<&str>) -> u64 {
    if let Ok(cache) = FEE_CACHE.lock() {
        if cache.1.elapsed() < FEE_CACHE_TTL {
            return cache.0;
        }
    }
    let fee = fetch_priority_fee(&[], rpc_url).await.unwrap_or(0);
    if let Ok(mut cache) = FEE_CACHE.lock() {
        *cache = (fee, std::time::Instant::now());
    }
    fee
}

/// Get priority fee with specific writable accounts (bypasses cache for accuracy).
async fn get_priority_fee_for_accounts(accounts: &[&str], rpc_url: Option<&str>) -> u64 {
    fetch_priority_fee(accounts, rpc_url).await.unwrap_or(0)
}

// ── Transaction confirmation ──────────────────────────────

/// Poll for transaction confirmation with `confirmed` commitment.
/// Returns Ok(()) when confirmed, or Err after timeout (30s).
pub async fn confirm_transaction(signature: &str, rpc_url: Option<&str>) -> Result<()> {
    let timeout = std::time::Duration::from_secs(30);
    let start = std::time::Instant::now();

    loop {
        if start.elapsed() > timeout {
            return Err(eyre!("Transaction confirmation timeout (30s) — tx may still land: {signature}"));
        }

        if let Ok(result) = rpc_call_simple::<serde_json::Value>(
            "getSignatureStatuses",
            serde_json::json!([[signature], { "searchTransactionHistory": false }]),
            rpc_url,
        ).await {
            if let Some(status) = result.get("value")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
            {
                if !status.is_null() {
                    // Check confirmationStatus is at least "confirmed"
                    let is_confirmed = status.get("confirmationStatus")
                        .and_then(|s| s.as_str())
                        .map(|s| s == "confirmed" || s == "finalized")
                        .unwrap_or(true); // if no status field, assume confirmed
                    if !is_confirmed {
                        // Still "processed" — wait for confirmed
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        continue;
                    }
                    if let Some(err) = status.get("err") {
                        if !err.is_null() {
                            return Err(eyre!("Transaction failed on-chain: {err}"));
                        }
                    }
                    return Ok(()); // confirmed, no error
                }
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

// ── Compute Budget ────────────────────────────────────────

/// Compute Budget Program ID (ComputeBudget111111111111111111111111111111)
const COMPUTE_BUDGET_PROGRAM: [u8; 32] = [
    3, 6, 70, 111, 229, 33, 23, 50, 255, 236, 173, 186, 114, 195, 155, 231,
    188, 140, 229, 187, 197, 247, 18, 107, 44, 67, 155, 58, 64, 0, 0, 0,
];

/// Build a SetComputeUnitLimit instruction (index 2).
fn compute_unit_limit_ix(units: u32) -> Instruction {
    let mut data = vec![2]; // SetComputeUnitLimit
    data.extend_from_slice(&units.to_le_bytes());
    Instruction {
        program_id_index: 0, // placeholder, set by caller
        account_indices: vec![],
        data,
    }
}

/// Build a SetComputeUnitPrice instruction (index 3).
fn compute_unit_price_ix(micro_lamports: u64) -> Instruction {
    let mut data = vec![3]; // SetComputeUnitPrice
    data.extend_from_slice(&micro_lamports.to_le_bytes());
    Instruction {
        program_id_index: 0, // placeholder, set by caller
        account_indices: vec![],
        data,
    }
}

// ── Transfer functions ─────────────────────────────────────

/// System Program ID (for SOL transfers)
const SYSTEM_PROGRAM: [u8; 32] = [0; 32];
/// SPL Token Program ID (for USDC)
const TOKEN_PROGRAM: [u8; 32] = [
    6, 221, 246, 225, 215, 101, 161, 147, 217, 203, 225, 70, 206, 235, 121, 172,
    28, 180, 133, 237, 95, 91, 55, 145, 58, 140, 245, 133, 126, 255, 0, 169,
];
/// SPL Token-2022 Program ID (for GM tokens)
const TOKEN_2022_PROGRAM_ID: [u8; 32] = [
    6, 221, 246, 225, 238, 117, 143, 222, 170, 87, 98, 201, 149, 175, 67, 79,
    76, 58, 63, 231, 108, 86, 185, 252, 92, 143, 207, 172, 247, 90, 75, 117,
];
/// Associated Token Account Program ID
const ATA_PROGRAM: [u8; 32] = [
    140, 151, 37, 143, 78, 36, 137, 241, 187, 61, 16, 41, 20, 142, 13, 131,
    11, 90, 19, 153, 218, 255, 16, 132, 4, 142, 123, 216, 219, 233, 248, 89,
];

/// Transfer SOL to a recipient. Returns transaction result with signature and confirmation status.
/// Includes priority fee via Compute Budget instructions for reliable landing.
pub async fn transfer_sol(
    wallet: &Wallet,
    recipient: &str,
    amount_sol: f64,
    rpc_url: Option<&str>,
) -> Result<TransactionResult> {
    // String-based conversion to avoid float→integer precision loss.
    let amount_str = format!("{amount_sol:.9}");
    let lamports: u64 = crate::jupiter::token_to_raw(&amount_str, 9)?
        .parse()
        .map_err(|e| eyre!("Invalid lamports value: {e}"))?;
    validate_address(recipient)?;
    let from = bs58::decode(wallet.pubkey()).into_vec()
        .map_err(|e| eyre!("Invalid sender pubkey: {e}"))?;
    let to = bs58::decode(recipient).into_vec()
        .map_err(|e| eyre!("Invalid recipient address: {e}"))?;

    // Fetch priority fee with writable accounts for accuracy
    let sender_addr = wallet.pubkey();
    let priority_fee = get_priority_fee_for_accounts(&[&sender_addr, recipient], rpc_url).await;

    // System transfer instruction: program_id_index=3, data=lamports(u32_le(2) + u64_le)
    let mut ix_data = vec![2, 0, 0, 0]; // Transfer instruction index
    ix_data.extend_from_slice(&lamports.to_le_bytes());

    let accounts = vec![
        from.clone(),                   // 0: sender (signer, writable)
        to.clone(),                     // 1: recipient (writable)
        COMPUTE_BUDGET_PROGRAM.to_vec(), // 2: compute budget program
        SYSTEM_PROGRAM.to_vec(),        // 3: system program
    ];

    let mut instructions = Vec::new();

    // Compute budget: limit + price (program_id_index = 2)
    let mut cu_limit = compute_unit_limit_ix(200_000); // will be tightened by simulation
    cu_limit.program_id_index = 2;
    instructions.push(cu_limit);

    if priority_fee > 0 {
        let mut cu_price = compute_unit_price_ix(priority_fee);
        cu_price.program_id_index = 2;
        instructions.push(cu_price);
    }

    // SOL transfer (program_id_index = 3)
    instructions.push(Instruction {
        program_id_index: 3,
        account_indices: vec![0, 1],
        data: ix_data,
    });

    let header = MessageHeader {
        num_required_sigs: 1,
        num_readonly_signed: 0,
        num_readonly_unsigned: 2, // compute budget + system program
    };

    send_legacy_transaction(wallet, &accounts, &instructions, &header, rpc_url).await
}

/// Transfer SPL token (USDC or GM token) to a recipient. Returns transaction result.
/// Automatically creates recipient's ATA if it doesn't exist.
/// Includes priority fee via Compute Budget instructions for reliable landing.
pub async fn transfer_spl(
    wallet: &Wallet,
    recipient: &str,
    mint: &str,
    amount_raw: u64,
    decimals: u8,
    is_token_2022: bool,
    rpc_url: Option<&str>,
) -> Result<TransactionResult> {
    validate_address(recipient)?;
    let from_pubkey = bs58::decode(wallet.pubkey()).into_vec()
        .map_err(|e| eyre!("Invalid sender pubkey: {e}"))?;
    let to_pubkey = bs58::decode(recipient).into_vec()
        .map_err(|e| eyre!("Invalid recipient address: {e}"))?;
    let mint_pubkey = bs58::decode(mint).into_vec()
        .map_err(|e| eyre!("Invalid mint address: {e}"))?;

    let token_program = if is_token_2022 { TOKEN_2022_PROGRAM_ID } else { TOKEN_PROGRAM };

    let from_ata = derive_ata(&from_pubkey, &mint_pubkey, &token_program)?;
    let to_ata = derive_ata(&to_pubkey, &mint_pubkey, &token_program)?;

    // Check if recipient ATA exists + fetch priority fee with writable accounts
    let from_ata_str = bs58::encode(&from_ata).into_string();
    let to_ata_str = bs58::encode(&to_ata).into_string();
    let fee_accounts: Vec<&str> = vec![&from_ata_str, &to_ata_str];
    let (ata_exists, priority_fee) = tokio::join!(
        check_account_exists(&to_ata, rpc_url),
        get_priority_fee_for_accounts(&fee_accounts, rpc_url),
    );
    let ata_exists = ata_exists?;

    // Build transfer instruction (TransferChecked = index 12)
    let mut ix_data = vec![12]; // TransferChecked
    ix_data.extend_from_slice(&amount_raw.to_le_bytes());
    ix_data.push(decimals);

    let accounts: Vec<Vec<u8>>;
    let mut instructions: Vec<Instruction> = Vec::new();

    if ata_exists {
        // Accounts: sender, source ATA, dest ATA, compute_budget, mint, token_program
        accounts = vec![
            from_pubkey.clone(),                // 0: sender (signer, writable)
            from_ata.clone(),                   // 1: source ATA (writable)
            to_ata.clone(),                     // 2: dest ATA (writable)
            COMPUTE_BUDGET_PROGRAM.to_vec(),    // 3: compute budget (readonly)
            mint_pubkey.clone(),                // 4: mint (readonly)
            token_program.to_vec(),             // 5: token program (readonly)
        ];

        // Compute budget instructions (program_id_index = 3)
        let mut cu_limit = compute_unit_limit_ix(200_000);
        cu_limit.program_id_index = 3;
        instructions.push(cu_limit);
        if priority_fee > 0 {
            let mut cu_price = compute_unit_price_ix(priority_fee);
            cu_price.program_id_index = 3;
            instructions.push(cu_price);
        }

        // TransferChecked: [source(1), mint(4), dest(2), authority(0)]
        instructions.push(Instruction {
            program_id_index: 5,
            account_indices: vec![1, 4, 2, 0],
            data: ix_data,
        });

        let header = MessageHeader {
            num_required_sigs: 1,
            num_readonly_signed: 0,
            num_readonly_unsigned: 3, // compute_budget, mint, token_program
        };

        send_legacy_transaction(wallet, &accounts, &instructions, &header, rpc_url).await
    } else {
        // Create recipient ATA, then transfer
        accounts = vec![
            from_pubkey.clone(),                // 0: payer/sender (signer, writable)
            to_ata.clone(),                     // 1: new ATA (writable)
            from_ata.clone(),                   // 2: source ATA (writable)
            COMPUTE_BUDGET_PROGRAM.to_vec(),    // 3: compute budget (readonly)
            to_pubkey.clone(),                  // 4: owner of new ATA (readonly)
            mint_pubkey.clone(),                // 5: mint (readonly)
            SYSTEM_PROGRAM.to_vec(),            // 6: system program (readonly)
            token_program.to_vec(),             // 7: token program (readonly)
            ATA_PROGRAM.to_vec(),               // 8: ATA program (readonly)
        ];

        // Compute budget instructions (program_id_index = 3)
        let mut cu_limit = compute_unit_limit_ix(400_000); // higher limit for create ATA + transfer
        cu_limit.program_id_index = 3;
        instructions.push(cu_limit);
        if priority_fee > 0 {
            let mut cu_price = compute_unit_price_ix(priority_fee);
            cu_price.program_id_index = 3;
            instructions.push(cu_price);
        }

        // Create ATA: [payer(0), ata(1), owner(4), mint(5), system(6), token_prog(7)]
        instructions.push(Instruction {
            program_id_index: 8,
            account_indices: vec![0, 1, 4, 5, 6, 7],
            data: vec![],
        });

        // TransferChecked: [source(2), mint(5), dest(1), authority(0)]
        instructions.push(Instruction {
            program_id_index: 7,
            account_indices: vec![2, 5, 1, 0],
            data: ix_data,
        });

        let header = MessageHeader {
            num_required_sigs: 1,
            num_readonly_signed: 0,
            num_readonly_unsigned: 6, // compute_budget, to_pubkey, mint, system, token_prog, ata_prog
        };

        send_legacy_transaction(wallet, &accounts, &instructions, &header, rpc_url).await
    }
}

// ── Transaction builder ────────────────────────────────────

struct MessageHeader {
    num_required_sigs: u8,
    num_readonly_signed: u8,
    num_readonly_unsigned: u8,
}

struct Instruction {
    program_id_index: u8,
    account_indices: Vec<u8>,
    data: Vec<u8>,
}

/// Serialize a legacy message from components.
fn build_legacy_message(
    header: &MessageHeader,
    accounts: &[Vec<u8>],
    blockhash: &[u8; 32],
    instructions: &[Instruction],
) -> Vec<u8> {
    let mut message = Vec::new();
    message.push(header.num_required_sigs);
    message.push(header.num_readonly_signed);
    message.push(header.num_readonly_unsigned);
    encode_compact_u16(accounts.len() as u16, &mut message);
    for acc in accounts {
        message.extend_from_slice(acc);
    }
    message.extend_from_slice(blockhash);
    encode_compact_u16(instructions.len() as u16, &mut message);
    for ix in instructions {
        message.push(ix.program_id_index);
        encode_compact_u16(ix.account_indices.len() as u16, &mut message);
        message.extend_from_slice(&ix.account_indices);
        encode_compact_u16(ix.data.len() as u16, &mut message);
        message.extend_from_slice(&ix.data);
    }
    message
}

/// Sign a message and assemble into a base64-encoded legacy transaction.
fn sign_and_encode(wallet: &Wallet, message: &[u8]) -> String {
    use ed25519_dalek::Signer;
    let signature = wallet.signing_key().sign(message);
    let mut tx = Vec::new();
    encode_compact_u16(1, &mut tx); // 1 signature
    tx.extend_from_slice(&signature.to_bytes());
    tx.extend_from_slice(message);
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(&tx)
}

/// Build, simulate, adjust CU, sign, and send a legacy Solana transaction.
/// Flow per Solana best practices:
///   1. Build tx with high CU limit → simulate → get unitsConsumed
///   2. Rebuild tx with tight CU limit (consumed × 1.1)
///   3. Send with skipPreflight=true (already validated)
///   4. Poll for confirmation
async fn send_legacy_transaction(
    wallet: &Wallet,
    accounts: &[Vec<u8>],
    instructions: &[Instruction],
    header: &MessageHeader,
    rpc_url: Option<&str>,
) -> Result<TransactionResult> {
    // 1. Get recent blockhash (confirmed commitment for more validity)
    let bh = get_recent_blockhash(rpc_url).await?;

    // 2. Build tx with original CU limit → simulate
    let message = build_legacy_message(header, accounts, &bh.hash, instructions);
    let sim_tx = sign_and_encode(wallet, &message);

    let tight_cu = match simulate_transaction(&sim_tx, rpc_url).await {
        Ok(units_consumed) => {
            // Add 10% buffer per Solana recommendation
            let tight = ((units_consumed as f64) * 1.1) as u32;
            tight.max(1_000) // minimum 1K CU
        }
        Err(_) => {
            // Simulation failed — send with skipPreflight (some RPCs don't simulate well)
            let sig = send_raw_transaction(&sim_tx, true, rpc_url).await?;
            let confirmed = confirm_transaction(&sig, rpc_url).await.is_ok();
            return Ok(TransactionResult { signature: sig, confirmed });
        }
    };

    // 3. Rebuild instructions with tight CU limit
    let mut tight_instructions = Vec::new();
    for ix in instructions {
        if ix.data.first() == Some(&2) && ix.data.len() == 5 {
            // Replace SetComputeUnitLimit with tight value
            tight_instructions.push(compute_unit_limit_ix_with_index(tight_cu, ix.program_id_index));
        } else {
            tight_instructions.push(Instruction {
                program_id_index: ix.program_id_index,
                account_indices: ix.account_indices.clone(),
                data: ix.data.clone(),
            });
        }
    }

    // 4. Sign and send with skipPreflight=true (already simulated)
    let final_message = build_legacy_message(header, accounts, &bh.hash, &tight_instructions);
    let final_tx = sign_and_encode(wallet, &final_message);
    let sig = send_raw_transaction(&final_tx, true, rpc_url).await?;

    // 5. Confirm — poll until confirmed
    let confirmed = match confirm_transaction(&sig, rpc_url).await {
        Ok(()) => true,
        Err(e) => {
            eprintln!("Warning: confirmation poll failed ({e}), tx may still land.");
            false
        }
    };

    Ok(TransactionResult { signature: sig, confirmed })
}

/// Build a SetComputeUnitLimit instruction with a specific program_id_index.
fn compute_unit_limit_ix_with_index(units: u32, program_id_index: u8) -> Instruction {
    let mut ix = compute_unit_limit_ix(units);
    ix.program_id_index = program_id_index;
    ix
}

/// Blockhash with expiration info for retry logic.
struct BlockhashInfo {
    hash: [u8; 32],
    /// Stored for future retry/expiration detection (Solana best practice).
    #[allow(dead_code)]
    last_valid_block_height: u64,
}

/// Get a recent blockhash from Solana RPC.
/// Uses `confirmed` commitment for ~13s more validity vs `finalized` (per Solana docs).
/// Returns blockhash + lastValidBlockHeight for expiration tracking.
async fn get_recent_blockhash(rpc_url: Option<&str>) -> Result<BlockhashInfo> {
    let result: serde_json::Value = rpc_call_simple(
        "getLatestBlockhash",
        serde_json::json!([{ "commitment": "confirmed" }]),
        rpc_url,
    ).await?;

    let hash_str = result["value"]["blockhash"]
        .as_str()
        .ok_or_else(|| eyre!("Missing blockhash in response"))?;
    let last_valid = result["value"]["lastValidBlockHeight"]
        .as_u64()
        .unwrap_or(0);

    let hash_bytes = bs58::decode(hash_str).into_vec()
        .map_err(|e| eyre!("Invalid blockhash: {e}"))?;
    let hash: [u8; 32] = hash_bytes.try_into()
        .map_err(|_| eyre!("Blockhash wrong length"))?;
    Ok(BlockhashInfo { hash, last_valid_block_height: last_valid })
}

/// Send a signed transaction to Solana RPC. Returns tx signature.
/// `skip_preflight`: set true if already simulated (avoids double-check, faster).
async fn send_raw_transaction(tx_base64: &str, skip_preflight: bool, rpc_url: Option<&str>) -> Result<String> {
    rpc_call_simple(
        "sendTransaction",
        serde_json::json!([
            tx_base64,
            { "encoding": "base64", "skipPreflight": skip_preflight, "preflightCommitment": "confirmed" }
        ]),
        rpc_url,
    ).await
}

/// Simulate a transaction to get compute units consumed.
/// Returns (units_consumed, error_if_any). Uses `replaceRecentBlockhash` for convenience.
async fn simulate_transaction(tx_base64: &str, rpc_url: Option<&str>) -> Result<u64> {
    let result: serde_json::Value = rpc_call_simple(
        "simulateTransaction",
        serde_json::json!([
            tx_base64,
            { "encoding": "base64", "commitment": "confirmed", "replaceRecentBlockhash": true }
        ]),
        rpc_url,
    ).await?;

    // Check for simulation error
    if let Some(err) = result.get("value").and_then(|v| v.get("err")) {
        if !err.is_null() {
            let logs = result.get("value")
                .and_then(|v| v.get("logs"))
                .and_then(|l| l.as_array())
                .map(|logs| {
                    logs.iter()
                        .filter_map(|l| l.as_str())
                        .collect::<Vec<_>>()
                        .join("\n  ")
                })
                .unwrap_or_default();
            return Err(eyre!("Transaction simulation failed: {err}\n  {logs}"));
        }
    }

    let units = result.get("value")
        .and_then(|v| v.get("unitsConsumed"))
        .and_then(|u| u.as_u64())
        .unwrap_or(200_000); // safe fallback

    Ok(units)
}

/// Check if a Solana account exists (has non-zero lamports).
async fn check_account_exists(address: &[u8], rpc_url: Option<&str>) -> Result<bool> {
    let addr_str = bs58::encode(address).into_string();
    let result: serde_json::Value = rpc_call_simple(
        "getAccountInfo",
        serde_json::json!([addr_str, { "encoding": "base64" }]),
        rpc_url,
    ).await?;

    Ok(result.get("value")
        .map(|v| !v.is_null())
        .unwrap_or(false))
}

/// Derive Associated Token Account address.
fn derive_ata(owner: &[u8], mint: &[u8], token_program: &[u8; 32]) -> Result<Vec<u8>> {
    use sha2::{Sha256, Digest};

    // PDA seeds: [owner, token_program, mint]
    // Program: ATA program
    let seeds: &[&[u8]] = &[owner, token_program.as_slice(), mint];

    // Find PDA: try nonce from 255 down to 0
    for nonce in (0..=255u8).rev() {
        let mut hasher = Sha256::new();
        for seed in seeds {
            hasher.update(seed);
        }
        hasher.update([nonce]);
        hasher.update(ATA_PROGRAM);
        hasher.update(b"ProgramDerivedAddress");
        let hash = hasher.finalize();

        // Valid PDA must NOT be on the Ed25519 curve
        if !is_on_curve(&hash) {
            return Ok(hash.to_vec());
        }
    }

    Err(eyre!("Failed to derive ATA — no valid PDA found"))
}

/// Check if a 32-byte key is on the Ed25519 curve.
fn is_on_curve(bytes: &[u8]) -> bool {
    if bytes.len() != 32 { return false; }
    let arr: [u8; 32] = bytes.try_into().unwrap();
    ed25519_dalek::VerifyingKey::from_bytes(&arr).is_ok()
}

// ── Reclaim (close empty ATAs) ──────────────────────────────

/// An empty token account eligible for rent reclaim.
#[derive(Debug)]
pub struct EmptyTokenAccount {
    pub address: String,
    pub mint: String,
    pub lamports: u64,
    pub is_token_2022: bool,
}

/// SPL Token program ID (base58 string, for RPC queries).
const TOKEN_PROGRAM_STR: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// Find all empty token accounts (Token and Token-2022) for a wallet.
/// Skips the USDC ATA since it's needed for trading.
pub async fn get_empty_token_accounts(
    wallet: &str,
    rpc_url: Option<&str>,
) -> Result<Vec<EmptyTokenAccount>> {
    // Parallel: fetch Token-2022 + Token Program accounts simultaneously
    let (t2022_res, tprog_res) = tokio::join!(
        rpc_call_simple::<GetTokenAccountsResult>(
            "getTokenAccountsByOwner",
            serde_json::json!([wallet, { "programId": TOKEN_2022_PROGRAM }, { "encoding": "jsonParsed", "commitment": "confirmed" }]),
            rpc_url,
        ),
        rpc_call_simple::<GetTokenAccountsResult>(
            "getTokenAccountsByOwner",
            serde_json::json!([wallet, { "programId": TOKEN_PROGRAM_STR }, { "encoding": "jsonParsed", "commitment": "confirmed" }]),
            rpc_url,
        ),
    );

    let mut empty = Vec::new();
    for (result, is_2022) in [(t2022_res, true), (tprog_res, false)] {
        for acc in result?.value {
            let mint = &acc.account.data.parsed.info.mint;
            let amount = &acc.account.data.parsed.info.token_amount.amount;
            // Skip USDC ATA — user needs it for trading
            if mint == USDC_MINT { continue; }
            if amount == "0" {
                let pubkey = acc.pubkey.as_deref().unwrap_or("");
                let lamports = acc.account.lamports.unwrap_or(0);
                if !pubkey.is_empty() && lamports > 0 {
                    empty.push(EmptyTokenAccount {
                        address: pubkey.to_string(),
                        mint: mint.clone(),
                        lamports,
                        is_token_2022: is_2022,
                    });
                }
            }
        }
    }

    Ok(empty)
}

/// Close empty token accounts and reclaim rent. Returns (signatures, total_lamports).
pub async fn close_empty_accounts(
    wallet: &Wallet,
    accounts: &[EmptyTokenAccount],
    rpc_url: Option<&str>,
) -> Result<(Vec<String>, u64)> {
    if accounts.is_empty() {
        return Ok((vec![], 0));
    }

    let owner = bs58::decode(wallet.pubkey()).into_vec()
        .map_err(|e| eyre!("Invalid wallet pubkey: {e}"))?;

    let token_2022: Vec<_> = accounts.iter().filter(|a| a.is_token_2022).collect();
    let token_prog: Vec<_> = accounts.iter().filter(|a| !a.is_token_2022).collect();

    let mut signatures = Vec::new();
    let mut total_lamports = 0u64;

    for batch in token_2022.chunks(15) {
        if !signatures.is_empty() {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        let result = close_account_batch(wallet, &owner, batch, &TOKEN_2022_PROGRAM_ID, rpc_url).await?;
        total_lamports += batch.iter().map(|a| a.lamports).sum::<u64>();
        signatures.push(result.signature);
    }

    for batch in token_prog.chunks(15) {
        if !signatures.is_empty() {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        let result = close_account_batch(wallet, &owner, batch, &TOKEN_PROGRAM, rpc_url).await?;
        total_lamports += batch.iter().map(|a| a.lamports).sum::<u64>();
        signatures.push(result.signature);
    }

    Ok((signatures, total_lamports))
}

/// Close a batch of empty token accounts in a single transaction.
async fn close_account_batch(
    wallet: &Wallet,
    owner: &[u8],
    batch: &[&EmptyTokenAccount],
    token_program: &[u8; 32],
    rpc_url: Option<&str>,
) -> Result<TransactionResult> {
    let mut accounts: Vec<Vec<u8>> = vec![owner.to_vec()];
    for acc in batch {
        let bytes = bs58::decode(&acc.address).into_vec()
            .map_err(|e| eyre!("Bad ATA address: {e}"))?;
        accounts.push(bytes);
    }
    accounts.push(token_program.to_vec());

    let program_idx = accounts.len() as u8 - 1;

    let mut instructions = Vec::new();
    for (i, _) in batch.iter().enumerate() {
        // CloseAccount: [account, destination, owner]
        instructions.push(Instruction {
            program_id_index: program_idx,
            account_indices: vec![(i + 1) as u8, 0, 0],
            data: vec![9], // CloseAccount discriminator
        });
    }

    let header = MessageHeader {
        num_required_sigs: 1,
        num_readonly_signed: 0,
        num_readonly_unsigned: 1,
    };

    send_legacy_transaction(wallet, &accounts, &instructions, &header, rpc_url).await
}

/// Encode a u16 as Solana compact-u16.
fn encode_compact_u16(val: u16, buf: &mut Vec<u8>) {
    if val < 0x80 {
        buf.push(val as u8);
    } else if val < 0x4000 {
        buf.push((val & 0x7f) as u8 | 0x80);
        buf.push((val >> 7) as u8);
    } else {
        buf.push((val & 0x7f) as u8 | 0x80);
        buf.push(((val >> 7) & 0x7f) as u8 | 0x80);
        buf.push((val >> 14) as u8);
    }
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

    // ── encode_compact_u16 ───────────────────────────────────

    #[test]
    fn compact_u16_single_byte() {
        let mut buf = Vec::new();
        encode_compact_u16(0, &mut buf);
        assert_eq!(buf, vec![0]);

        buf.clear();
        encode_compact_u16(127, &mut buf);
        assert_eq!(buf, vec![127]);
    }

    #[test]
    fn compact_u16_two_bytes() {
        let mut buf = Vec::new();
        encode_compact_u16(128, &mut buf);
        assert_eq!(buf.len(), 2);

        buf.clear();
        encode_compact_u16(255, &mut buf);
        assert_eq!(buf.len(), 2);
        // Decode back: (buf[0] & 0x7f) | (buf[1] << 7)
        let val = (buf[0] as u16 & 0x7f) | ((buf[1] as u16) << 7);
        assert_eq!(val, 255);
    }

    #[test]
    fn compact_u16_three_bytes() {
        let mut buf = Vec::new();
        encode_compact_u16(0x4000, &mut buf);
        assert_eq!(buf.len(), 3);

        buf.clear();
        encode_compact_u16(u16::MAX, &mut buf);
        assert_eq!(buf.len(), 3);
        let val = (buf[0] as u16 & 0x7f) | (((buf[1] as u16) & 0x7f) << 7) | ((buf[2] as u16) << 14);
        assert_eq!(val, u16::MAX);
    }

    #[test]
    fn compact_u16_roundtrip() {
        for val in [0, 1, 127, 128, 255, 256, 1000, 16383, 16384, 65535u16] {
            let mut buf = Vec::new();
            encode_compact_u16(val, &mut buf);
            // Verify it encodes to 1-3 bytes
            assert!(buf.len() >= 1 && buf.len() <= 3, "val={val} encoded to {} bytes", buf.len());
        }
    }

    // ── fee estimation ────────────────────────────────────────

    #[test]
    fn base_fee_constant() {
        assert_eq!(BASE_FEE_LAMPORTS, 5_000);
    }

    #[test]
    fn priority_fee_entry_deserialize() {
        let json = r#"{"slot":123,"prioritizationFee":1000}"#;
        let entry: PriorityFeeEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.prioritization_fee, 1000);
    }

    // ── compute budget instructions ──────────────────────────

    #[test]
    fn compute_unit_limit_ix_encodes_correctly() {
        let ix = compute_unit_limit_ix(200_000);
        assert_eq!(ix.data[0], 2); // SetComputeUnitLimit discriminator
        let units = u32::from_le_bytes(ix.data[1..5].try_into().unwrap());
        assert_eq!(units, 200_000);
        assert!(ix.account_indices.is_empty());
    }

    #[test]
    fn compute_unit_price_ix_encodes_correctly() {
        let ix = compute_unit_price_ix(50_000);
        assert_eq!(ix.data[0], 3); // SetComputeUnitPrice discriminator
        let price = u64::from_le_bytes(ix.data[1..9].try_into().unwrap());
        assert_eq!(price, 50_000);
        assert!(ix.account_indices.is_empty());
    }

    #[test]
    fn compute_budget_program_id_matches() {
        // Compute Budget Program: ComputeBudget111111111111111111111111111111
        let expected = bs58::decode("ComputeBudget111111111111111111111111111111")
            .into_vec()
            .unwrap();
        assert_eq!(COMPUTE_BUDGET_PROGRAM.as_slice(), expected.as_slice());
    }

    // ── fee cache ────────────────────────────────────────────

    #[test]
    fn fee_cache_ttl_is_10s() {
        assert_eq!(FEE_CACHE_TTL, std::time::Duration::from_secs(10));
    }

    // ── derive_ata ───────────────────────────────────────────

    #[test]
    fn derive_ata_produces_32_bytes() {
        let owner = bs58::decode("11111111111111111111111111111111").into_vec().unwrap();
        let mint = bs58::decode("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").into_vec().unwrap();
        let ata = derive_ata(&owner, &mint, &TOKEN_PROGRAM).unwrap();
        assert_eq!(ata.len(), 32);
    }

    #[test]
    fn derive_ata_deterministic() {
        let owner = bs58::decode("5CjgV1J2FE8yyxsHKGs2v4GJULBS7AiYtRo7DFYiuZ47").into_vec().unwrap();
        let mint = bs58::decode("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").into_vec().unwrap();
        let ata1 = derive_ata(&owner, &mint, &TOKEN_PROGRAM).unwrap();
        let ata2 = derive_ata(&owner, &mint, &TOKEN_PROGRAM).unwrap();
        assert_eq!(ata1, ata2);
    }

    // ── TransactionResult ────────────────────────────────────

    #[test]
    fn tx_result_confirmed() {
        let r = TransactionResult {
            signature: "abc123".to_string(),
            confirmed: true,
        };
        assert_eq!(r.signature, "abc123");
        assert!(r.confirmed);
    }

    #[test]
    fn tx_result_unconfirmed() {
        let r = TransactionResult {
            signature: "xyz789".to_string(),
            confirmed: false,
        };
        assert!(!r.confirmed);
    }

    // ── rent constants & fallback ────────────────────────────

    #[test]
    fn spl_token_account_size_is_165() {
        assert_eq!(SPL_TOKEN_ACCOUNT_SIZE, 165);
    }

    #[test]
    fn token_2022_account_size_is_182() {
        assert_eq!(TOKEN_2022_ACCOUNT_SIZE, 182);
    }

    #[test]
    fn rent_fallback_spl_is_known_value() {
        // 2_039_280 lamports = rent-exempt min for 165-byte SPL Token account (epoch 0 rate)
        assert_eq!(ATA_RENT_LAMPORTS_FALLBACK, 2_039_280);
    }

    #[test]
    fn rent_fallback_2022_is_known_value() {
        assert_eq!(ATA_RENT_LAMPORTS_2022_FALLBACK, 2_165_280);
    }

    #[test]
    fn rent_cache_ttl_is_5_minutes() {
        assert_eq!(RENT_CACHE_TTL, std::time::Duration::from_secs(300));
    }

    #[tokio::test]
    async fn estimate_gas_no_ata_returns_small_value() {
        // Without ATA creation, should only be base fee + priority + buffer
        let est = estimate_gas_needed(false, false, None).await;
        // Must be less than 0.001 SOL (just fees, no rent)
        assert!(est > 0.0 && est < 0.001, "estimate without ATA: {est}");
    }

    #[tokio::test]
    async fn estimate_gas_with_ata_includes_rent() {
        let without = estimate_gas_needed(false, false, None).await;
        let with_spl = estimate_gas_needed(true, false, None).await;
        let with_2022 = estimate_gas_needed(true, true, None).await;
        // With ATA must be significantly more than without
        assert!(with_spl > without + 0.001, "SPL ATA estimate should include rent");
        assert!(with_2022 > without + 0.001, "Token-2022 ATA estimate should include rent");
        // Token-2022 should be >= SPL (more bytes)
        assert!(with_2022 >= with_spl, "Token-2022 rent >= SPL rent");
    }
}
