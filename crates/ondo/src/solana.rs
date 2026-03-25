use eyre::{Result, eyre};
use serde::{Deserialize, Serialize};

use crate::token_list::GmTokenEntry;
use crate::wallet::Wallet;

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
        params: serde_json::json!([wallet, { "commitment": "confirmed" }]),
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
            { "encoding": "jsonParsed", "commitment": "confirmed" }
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
            { "encoding": "jsonParsed", "commitment": "confirmed" }
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
            { "encoding": "jsonParsed", "commitment": "confirmed" }
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
        &RpcRequest { jsonrpc: "2.0", id: 1, method: "getBalance", params: serde_json::json!([wallet, { "commitment": "confirmed" }]) },
    ).await?;
    let sol = sol_resp.result
        .map(|r| r.value as f64 / 1_000_000_000.0)
        .unwrap_or(0.0);

    // 2. USDC balance
    let usdc_resp: RpcResponse<GetTokenAccountsResult> = rpc_call_with_retry(
        &client, &urls,
        &RpcRequest {
            jsonrpc: "2.0", id: 2, method: "getTokenAccountsByOwner",
            params: serde_json::json!([wallet, { "mint": USDC_MINT }, { "encoding": "jsonParsed", "commitment": "confirmed" }]),
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
            params: serde_json::json!([wallet, { "programId": TOKEN_2022_PROGRAM }, { "encoding": "jsonParsed", "commitment": "confirmed" }]),
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
/// Solana Rent Sysvar
const RENT_SYSVAR: [u8; 32] = [
    6, 167, 213, 23, 25, 44, 86, 142, 224, 138, 132, 95, 115, 210, 151, 136,
    207, 3, 92, 49, 69, 178, 26, 179, 68, 216, 6, 46, 169, 64, 0, 0,
];

/// Transfer SOL to a recipient. Returns transaction signature.
pub async fn transfer_sol(
    wallet: &Wallet,
    recipient: &str,
    amount_sol: f64,
    rpc_url: Option<&str>,
) -> Result<String> {
    let lamports = (amount_sol * 1_000_000_000.0) as u64;
    let from = bs58::decode(wallet.pubkey()).into_vec()
        .map_err(|e| eyre!("Invalid sender pubkey: {e}"))?;
    let to = bs58::decode(recipient).into_vec()
        .map_err(|e| eyre!("Invalid recipient address: {e}"))?;
    if to.len() != 32 {
        return Err(eyre!("Invalid recipient address length"));
    }

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
    let from_pubkey = bs58::decode(wallet.pubkey()).into_vec()
        .map_err(|e| eyre!("Invalid sender pubkey: {e}"))?;
    let to_pubkey = bs58::decode(recipient).into_vec()
        .map_err(|e| eyre!("Invalid recipient address: {e}"))?;
    let mint_pubkey = bs58::decode(mint).into_vec()
        .map_err(|e| eyre!("Invalid mint address: {e}"))?;
    if to_pubkey.len() != 32 || mint_pubkey.len() != 32 {
        return Err(eyre!("Invalid address length"));
    }

    let token_program = if is_token_2022 { TOKEN_2022_PROGRAM_ID } else { TOKEN_PROGRAM };

    let from_ata = derive_ata(&from_pubkey, &mint_pubkey, &token_program);
    let to_ata = derive_ata(&to_pubkey, &mint_pubkey, &token_program);

    // Check if recipient ATA exists
    let ata_exists = check_account_exists(&to_ata, rpc_url).await?;

    // Build transfer instruction (TransferChecked = index 12)
    let mut ix_data = vec![12]; // TransferChecked
    ix_data.extend_from_slice(&amount_raw.to_le_bytes());
    ix_data.push(decimals);

    let accounts: Vec<Vec<u8>>;
    let mut instructions: Vec<Instruction> = Vec::new();

    if ata_exists {
        // Simple transfer: [from, mint, to_ata, to_pubkey, system, rent, token_prog]
        accounts = vec![
            from_pubkey.clone(),        // 0: sender (signer, writable)
            from_ata.clone(),           // 1: source ATA (writable)
            mint_pubkey.clone(),        // 2: mint (readonly)
            to_ata.clone(),             // 3: dest ATA (writable)
            token_program.to_vec(),     // 4: token program
        ];

        instructions.push(Instruction {
            program_id_index: 4,
            account_indices: vec![1, 2, 3, 0],
            data: ix_data,
        });

        let header = MessageHeader {
            num_required_sigs: 1,
            num_readonly_signed: 0,
            num_readonly_unsigned: 2, // mint, token_program
        };

        send_legacy_transaction(wallet, &accounts, &instructions, &header, rpc_url).await
    } else {
        // Need to create ATA first, then transfer
        accounts = vec![
            from_pubkey.clone(),        // 0: payer/sender (signer, writable)
            to_ata.clone(),             // 1: new ATA (writable)
            to_pubkey.clone(),          // 2: owner of new ATA
            mint_pubkey.clone(),        // 3: mint
            SYSTEM_PROGRAM.to_vec(),    // 4: system program
            token_program.to_vec(),     // 5: token program
            ATA_PROGRAM.to_vec(),       // 6: ATA program
            from_ata.clone(),           // 7: source ATA (writable)
            RENT_SYSVAR.to_vec(),       // 8: rent sysvar
        ];

        // Create ATA instruction (ATA program, no data)
        instructions.push(Instruction {
            program_id_index: 6,
            account_indices: vec![0, 1, 2, 3, 4, 5],
            data: vec![],
        });

        // TransferChecked: source(7), mint(3), dest(1), authority(0)
        instructions.push(Instruction {
            program_id_index: 5,
            account_indices: vec![7, 3, 1, 0],
            data: ix_data,
        });

        let header = MessageHeader {
            num_required_sigs: 1,
            num_readonly_signed: 0,
            num_readonly_unsigned: 5, // to_pubkey, mint, system, token_prog, ata_prog, rent
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
    let client = reqwest::Client::new();
    let req = RpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "getLatestBlockhash",
        params: serde_json::json!([{ "commitment": "finalized" }]),
    };

    let resp: RpcResponse<serde_json::Value> = rpc_call_with_retry(
        &client, &rpc_urls(rpc_url), &req,
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
    let client = reqwest::Client::new();
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
        &client, &rpc_urls(rpc_url), &req,
    ).await?;

    resp.result.ok_or_else(|| eyre!("Transaction failed — no signature returned"))
}

/// Check if a Solana account exists (has non-zero lamports).
async fn check_account_exists(address: &[u8], rpc_url: Option<&str>) -> Result<bool> {
    let addr_str = bs58::encode(address).into_string();
    let client = reqwest::Client::new();
    let req = RpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method: "getAccountInfo",
        params: serde_json::json!([addr_str, { "encoding": "base64" }]),
    };

    let resp: RpcResponse<serde_json::Value> = rpc_call_with_retry(
        &client, &rpc_urls(rpc_url), &req,
    ).await?;

    Ok(resp.result
        .and_then(|r| r.get("value").cloned())
        .map(|v| !v.is_null())
        .unwrap_or(false))
}

/// Derive Associated Token Account address.
fn derive_ata(owner: &[u8], mint: &[u8], token_program: &[u8; 32]) -> Vec<u8> {
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
        // Simple check: try to decompress as Ed25519 point
        if !is_on_curve(&hash) {
            return hash.to_vec();
        }
    }

    // Fallback (should never happen)
    Vec::new()
}

/// Check if a 32-byte key is on the Ed25519 curve.
fn is_on_curve(bytes: &[u8]) -> bool {
    if bytes.len() != 32 { return false; }
    let arr: [u8; 32] = bytes.try_into().unwrap();
    ed25519_dalek::VerifyingKey::from_bytes(&arr).is_ok()
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
