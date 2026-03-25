use eyre::{Result, eyre};
use serde::{Deserialize, Serialize};

use crate::token_list::GmTokenEntry;
use crate::wallet::Wallet;
use crate::HTTP;

/// Public Solana RPC endpoints — rotated on rate-limit errors.
/// Order: most stable first. User can override with --rpc-url or RWA_RPC_URL.
const RPC_URLS: &[&str] = &[
    "https://solana-rpc.publicnode.com",           // PublicNode — 10 nodes, stable
    "https://solana-mainnet.rpc.extrnode.com",     // ExtrNode — used by wallets
    "https://api.mainnet-beta.solana.com",         // Solana Foundation — 10 req/s
    "https://rpc.ankr.com/solana",                 // Ankr — ~30 req/min free
    "https://solana.drpc.org",                     // dRPC — decentralized, free tier
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
    let client = &*HTTP;
    let req = RpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "getBalance",
        params: serde_json::json!([wallet, { "commitment": "confirmed" }]),
    };

    let resp: RpcResponse<GetBalanceResult> = rpc_call_with_retry(
        client, &rpc_urls(rpc_url), &req,
    ).await?;

    let result = resp.result
        .ok_or_else(|| eyre!("Empty response from Solana RPC"))?;

    Ok(result.value as f64 / 1_000_000_000.0)
}

/// Get USDC balance for a wallet on Solana.
pub async fn get_usdc_balance(wallet: &str, rpc_url: Option<&str>) -> Result<f64> {
    let client = &*HTTP;
    let req = RpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "getTokenAccountsByOwner",
        params: serde_json::json!([
            wallet,
            { "mint": USDC_MINT },
            { "encoding": "jsonParsed", "commitment": "confirmed" }
        ]),
    };

    let resp: RpcResponse<GetTokenAccountsResult> = rpc_call_with_retry(
        client, &rpc_urls(rpc_url), &req,
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
    /// Raw on-chain amount as string (no float precision loss).
    pub raw_amount: String,
}

/// Fetch all GM token balances for a Solana wallet.
pub async fn get_all_balances(
    wallet: &str,
    tokens: &[GmTokenEntry],
    rpc_url: Option<&str>,
) -> Result<Vec<SolanaTokenBalance>> {
    let client = &*HTTP;
    let req = RpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "getTokenAccountsByOwner",
        params: serde_json::json!([
            wallet,
            { "programId": TOKEN_2022_PROGRAM },
            { "encoding": "jsonParsed", "commitment": "confirmed" }
        ]),
    };

    let resp: RpcResponse<GetTokenAccountsResult> = rpc_call_with_retry(
        client, &rpc_urls(rpc_url), &req,
    ).await?;

    let accounts = resp.result
        .ok_or_else(|| eyre!("Empty response from Solana RPC"))?;

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
    let client = &*HTTP;
    let req = RpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "getTokenAccountsByOwner",
        params: serde_json::json!([
            wallet,
            { "mint": mint },
            { "encoding": "jsonParsed", "commitment": "confirmed" }
        ]),
    };

    let resp: RpcResponse<GetTokenAccountsResult> = rpc_call_with_retry(
        client, &rpc_urls(rpc_url), &req,
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
                let delay = 1000 * u64::from(attempt + 1);
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            let resp = match client.post(*url).json(req).timeout(timeout).send().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = e.to_string();
                    break; // connection error → try next URL
                }
            };

            let status = resp.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                last_err = format!("HTTP {status}");
                continue; // rate limited or 5xx → retry same URL with backoff
            }

            let parsed: RpcResponse<T> = match resp.json().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = format!("error decoding response body: {e}");
                    continue; // HTML / bad response → retry with backoff
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

/// Make a batch RPC call (multiple requests in one HTTP request) with retry.
async fn rpc_batch_with_retry(
    client: &reqwest::Client,
    urls: &[&str],
    reqs: &[RpcRequest<'_>],
) -> Result<Vec<serde_json::Value>> {
    let timeout = std::time::Duration::from_secs(20);
    let mut last_err = String::new();

    for (url_idx, url) in urls.iter().enumerate() {
        for attempt in 0..3u32 {
            if attempt > 0 || url_idx > 0 {
                let delay = 1000 * u64::from(attempt + 1);
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            let resp = match client.post(*url).json(&reqs).timeout(timeout).send().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = e.to_string();
                    break; // connection error → try next URL
                }
            };

            let status = resp.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                last_err = format!("HTTP {status}");
                continue;
            }

            let results: Vec<serde_json::Value> = match resp.json().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = format!("error decoding response body: {e}");
                    continue; // HTML / bad response → retry with backoff
                }
            };

            if results.len() != reqs.len() {
                last_err = format!("batch: got {} responses, expected {}", results.len(), reqs.len());
                continue;
            }

            return Ok(results);
        }
    }

    Err(eyre!(
        "Solana RPC unavailable (all endpoints rate-limited or down).\n  \
         Last error: {last_err}\n  \
         Hint: set RWA_RPC_URL to a private RPC endpoint, or retry in a few seconds."
    ))
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

/// Transfer SOL to a recipient. Returns transaction signature.
pub async fn transfer_sol(
    wallet: &Wallet,
    recipient: &str,
    amount_sol: f64,
    rpc_url: Option<&str>,
) -> Result<String> {
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

    // System transfer instruction: program_id_index=2, data=lamports(u32_le(2) + u64_le)
    let mut ix_data = vec![2, 0, 0, 0]; // Transfer instruction index
    ix_data.extend_from_slice(&lamports.to_le_bytes());

    let accounts = vec![
        from.clone(),      // 0: sender (signer, writable)
        to.clone(),        // 1: recipient (writable)
        SYSTEM_PROGRAM.to_vec(), // 2: system program
    ];

    let ix = Instruction {
        program_id_index: 2,
        account_indices: vec![0, 1],
        data: ix_data,
    };

    let header = MessageHeader {
        num_required_sigs: 1,
        num_readonly_signed: 0,
        num_readonly_unsigned: 1,
    };

    send_legacy_transaction(wallet, &accounts, &[ix], &header, rpc_url).await
}

/// Transfer SPL token (USDC or GM token) to a recipient. Returns tx signature.
/// Automatically creates recipient's ATA if it doesn't exist.
pub async fn transfer_spl(
    wallet: &Wallet,
    recipient: &str,
    mint: &str,
    amount_raw: u64,
    decimals: u8,
    is_token_2022: bool,
    rpc_url: Option<&str>,
) -> Result<String> {
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

    // Check if recipient ATA exists
    let ata_exists = check_account_exists(&to_ata, rpc_url).await?;

    // Build transfer instruction (TransferChecked = index 12)
    let mut ix_data = vec![12]; // TransferChecked
    ix_data.extend_from_slice(&amount_raw.to_le_bytes());
    ix_data.push(decimals);

    let accounts: Vec<Vec<u8>>;
    let mut instructions: Vec<Instruction> = Vec::new();

    if ata_exists {
        // TransferChecked: source ATA → dest ATA
        // Solana requires: writable accounts first, then readonly last
        accounts = vec![
            from_pubkey.clone(),        // 0: sender (signer, writable)
            from_ata.clone(),           // 1: source ATA (writable)
            to_ata.clone(),             // 2: dest ATA (writable)
            mint_pubkey.clone(),        // 3: mint (readonly)
            token_program.to_vec(),     // 4: token program (readonly)
        ];

        // TransferChecked: [source(1), mint(3), dest(2), authority(0)]
        instructions.push(Instruction {
            program_id_index: 4,
            account_indices: vec![1, 3, 2, 0],
            data: ix_data,
        });

        let header = MessageHeader {
            num_required_sigs: 1,
            num_readonly_signed: 0,
            num_readonly_unsigned: 2, // mint, token_program
        };

        send_legacy_transaction(wallet, &accounts, &instructions, &header, rpc_url).await
    } else {
        // Create recipient ATA, then transfer
        // Writable non-signers first, then readonly non-signers
        accounts = vec![
            from_pubkey.clone(),        // 0: payer/sender (signer, writable)
            to_ata.clone(),             // 1: new ATA (writable)
            from_ata.clone(),           // 2: source ATA (writable)
            to_pubkey.clone(),          // 3: owner of new ATA (readonly)
            mint_pubkey.clone(),        // 4: mint (readonly)
            SYSTEM_PROGRAM.to_vec(),    // 5: system program (readonly)
            token_program.to_vec(),     // 6: token program (readonly)
            ATA_PROGRAM.to_vec(),       // 7: ATA program (readonly)
        ];

        // Create ATA: [payer(0), ata(1), owner(3), mint(4), system(5), token_prog(6)]
        instructions.push(Instruction {
            program_id_index: 7,
            account_indices: vec![0, 1, 3, 4, 5, 6],
            data: vec![],
        });

        // TransferChecked: [source(2), mint(4), dest(1), authority(0)]
        instructions.push(Instruction {
            program_id_index: 6,
            account_indices: vec![2, 4, 1, 0],
            data: ix_data,
        });

        let header = MessageHeader {
            num_required_sigs: 1,
            num_readonly_signed: 0,
            num_readonly_unsigned: 5, // to_pubkey, mint, system, token_prog, ata_prog
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

/// Build, sign, and send a legacy Solana transaction.
async fn send_legacy_transaction(
    wallet: &Wallet,
    accounts: &[Vec<u8>],
    instructions: &[Instruction],
    header: &MessageHeader,
    rpc_url: Option<&str>,
) -> Result<String> {
    // 1. Get recent blockhash
    let blockhash = get_recent_blockhash(rpc_url).await?;

    // 2. Build message
    let mut message = Vec::new();
    message.push(header.num_required_sigs);
    message.push(header.num_readonly_signed);
    message.push(header.num_readonly_unsigned);

    // Account keys
    encode_compact_u16(accounts.len() as u16, &mut message);
    for acc in accounts {
        message.extend_from_slice(acc);
    }

    // Recent blockhash
    message.extend_from_slice(&blockhash);

    // Instructions
    encode_compact_u16(instructions.len() as u16, &mut message);
    for ix in instructions {
        message.push(ix.program_id_index);
        encode_compact_u16(ix.account_indices.len() as u16, &mut message);
        message.extend_from_slice(&ix.account_indices);
        encode_compact_u16(ix.data.len() as u16, &mut message);
        message.extend_from_slice(&ix.data);
    }

    // 3. Sign
    use ed25519_dalek::Signer;
    let signature = wallet.signing_key().sign(&message);

    // 4. Build transaction: [sig_count] [signatures] [message]
    let mut tx = Vec::new();
    encode_compact_u16(1, &mut tx); // 1 signature
    tx.extend_from_slice(&signature.to_bytes());
    tx.extend_from_slice(&message);

    // 5. Send
    let tx_base64 = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(&tx)
    };

    send_raw_transaction(&tx_base64, rpc_url).await
}

/// Get a recent blockhash from Solana RPC.
async fn get_recent_blockhash(rpc_url: Option<&str>) -> Result<[u8; 32]> {
    let client = &*HTTP;
    let req = RpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "getLatestBlockhash",
        params: serde_json::json!([{ "commitment": "finalized" }]),
    };

    let resp: RpcResponse<serde_json::Value> = rpc_call_with_retry(
        client, &rpc_urls(rpc_url), &req,
    ).await?;

    let result = resp.result
        .ok_or_else(|| eyre!("Failed to get blockhash"))?;
    let hash_str = result["value"]["blockhash"]
        .as_str()
        .ok_or_else(|| eyre!("Missing blockhash in response"))?;

    let hash_bytes = bs58::decode(hash_str).into_vec()
        .map_err(|e| eyre!("Invalid blockhash: {e}"))?;
    let hash: [u8; 32] = hash_bytes.try_into()
        .map_err(|_| eyre!("Blockhash wrong length"))?;
    Ok(hash)
}

/// Send a signed transaction to Solana RPC. Returns tx signature.
async fn send_raw_transaction(tx_base64: &str, rpc_url: Option<&str>) -> Result<String> {
    let client = &*HTTP;
    let req = RpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "sendTransaction",
        params: serde_json::json!([
            tx_base64,
            { "encoding": "base64", "skipPreflight": false, "preflightCommitment": "confirmed" }
        ]),
    };

    let resp: RpcResponse<String> = rpc_call_with_retry(
        client, &rpc_urls(rpc_url), &req,
    ).await?;

    resp.result.ok_or_else(|| eyre!("Transaction failed — no signature returned"))
}

/// Check if a Solana account exists (has non-zero lamports).
async fn check_account_exists(address: &[u8], rpc_url: Option<&str>) -> Result<bool> {
    let addr_str = bs58::encode(address).into_string();
    let client = &*HTTP;
    let req = RpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "getAccountInfo",
        params: serde_json::json!([addr_str, { "encoding": "base64" }]),
    };

    let resp: RpcResponse<serde_json::Value> = rpc_call_with_retry(
        client, &rpc_urls(rpc_url), &req,
    ).await?;

    Ok(resp.result
        .and_then(|r| r.get("value").cloned())
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
    let urls = rpc_urls(rpc_url);
    let client = &*HTTP;

    let reqs = vec![
        RpcRequest {
            jsonrpc: "2.0", id: 1,
            method: "getTokenAccountsByOwner",
            params: serde_json::json!([
                wallet,
                { "programId": TOKEN_2022_PROGRAM },
                { "encoding": "jsonParsed", "commitment": "confirmed" }
            ]),
        },
        RpcRequest {
            jsonrpc: "2.0", id: 2,
            method: "getTokenAccountsByOwner",
            params: serde_json::json!([
                wallet,
                { "programId": TOKEN_PROGRAM_STR },
                { "encoding": "jsonParsed", "commitment": "confirmed" }
            ]),
        },
    ];

    let results = rpc_batch_with_retry(client, &urls, &reqs).await?;
    let mut empty = Vec::new();

    for (i, result) in results.iter().enumerate() {
        let is_2022 = i == 0;
        let accounts = result.get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_array());

        if let Some(accounts) = accounts {
            for acc in accounts {
                let pubkey = acc.get("pubkey").and_then(|v| v.as_str()).unwrap_or("");
                let lamports = acc.get("account")
                    .and_then(|a| a.get("lamports"))
                    .and_then(|l| l.as_u64())
                    .unwrap_or(0);
                let info = acc.get("account")
                    .and_then(|a| a.get("data"))
                    .and_then(|d| d.get("parsed"))
                    .and_then(|p| p.get("info"));
                let mint = info
                    .and_then(|i| i.get("mint"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("");
                let amount = info
                    .and_then(|i| i.get("tokenAmount"))
                    .and_then(|t| t.get("amount"))
                    .and_then(|a| a.as_str())
                    .unwrap_or("0");

                // Skip USDC ATA — user needs it for trading
                if mint == USDC_MINT {
                    continue;
                }

                if amount == "0" && !pubkey.is_empty() && lamports > 0 {
                    empty.push(EmptyTokenAccount {
                        address: pubkey.to_string(),
                        mint: mint.to_string(),
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
        let sig = close_account_batch(wallet, &owner, batch, &TOKEN_2022_PROGRAM_ID, rpc_url).await?;
        total_lamports += batch.iter().map(|a| a.lamports).sum::<u64>();
        signatures.push(sig);
    }

    for batch in token_prog.chunks(15) {
        if !signatures.is_empty() {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        let sig = close_account_batch(wallet, &owner, batch, &TOKEN_PROGRAM, rpc_url).await?;
        total_lamports += batch.iter().map(|a| a.lamports).sum::<u64>();
        signatures.push(sig);
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
) -> Result<String> {
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
}
