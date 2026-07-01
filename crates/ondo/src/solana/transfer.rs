use eyre::{Result, eyre};

use crate::wallet::Wallet;
use crate::USDC_MINT;
use super::rpc::{rpc_call_simple, RpcMode};
use super::fee::get_priority_fee_for_accounts;
use super::transaction::{
    TransactionResult, MessageHeader, Instruction, ConfirmLevel,
    send_legacy_transaction, compute_unit_limit_ix, compute_unit_price_ix,
    COMPUTE_BUDGET_PROGRAM,
};
use super::balance::GetTokenAccountsResult;

use crate::spl::{derive_ata, ATA_PROGRAM, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID as TOKEN_PROGRAM};

/// System Program ID (for SOL transfers)
const SYSTEM_PROGRAM: [u8; 32] = [0; 32];

/// SPL Token program ID (base58 string, for RPC queries).
const TOKEN_PROGRAM_STR: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// Transfer SOL to a recipient. Returns transaction result with signature and confirmation status.
/// Includes priority fee via Compute Budget instructions for reliable landing.
pub async fn transfer_sol(
    wallet: &Wallet,
    recipient: &str,
    amount_lamports: u64,
    rpc_url: Option<&str>,
) -> Result<TransactionResult> {
    super::validate_address(recipient)?;
    let from = bs58::decode(wallet.pubkey()).into_vec()
        .map_err(|e| eyre!("Invalid sender pubkey: {e}"))?;
    let to = bs58::decode(recipient).into_vec()
        .map_err(|e| eyre!("Invalid recipient address: {e}"))?;

    // Fetch priority fee with writable accounts for accuracy
    let sender_addr = wallet.pubkey();
    let priority_fee = get_priority_fee_for_accounts(&[&sender_addr, recipient], rpc_url).await;

    // System transfer instruction: program_id_index=3, data=lamports(u32_le(2) + u64_le)
    let mut ix_data = vec![2, 0, 0, 0]; // Transfer instruction index
    ix_data.extend_from_slice(&amount_lamports.to_le_bytes());

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

    send_legacy_transaction(wallet, &accounts, &instructions, &header, rpc_url, ConfirmLevel::Confirmed).await
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
    super::validate_address(recipient)?;
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

        send_legacy_transaction(wallet, &accounts, &instructions, &header, rpc_url, ConfirmLevel::Confirmed).await
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

        send_legacy_transaction(wallet, &accounts, &instructions, &header, rpc_url, ConfirmLevel::Confirmed).await
    }
}

/// Check if a Solana account exists (has non-zero lamports).
async fn check_account_exists(address: &[u8], rpc_url: Option<&str>) -> Result<bool> {
    let addr_str = bs58::encode(address).into_string();
    let result: serde_json::Value = rpc_call_simple(
        "getAccountInfo",
        serde_json::json!([addr_str, { "encoding": "base64" }]),
        rpc_url,
        RpcMode::Race,
    ).await?;

    Ok(result.get("value")
        .map(|v| !v.is_null())
        .unwrap_or(false))
}

// ── Reclaim (close empty ATAs) ──────────────────────────────

/// An empty token account eligible for rent reclaim.
#[derive(Debug, Clone)]
pub struct EmptyTokenAccount {
    pub address: String,
    pub mint: String,
    pub lamports: u64,
    pub is_token_2022: bool,
}

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
            serde_json::json!([wallet, { "programId": super::TOKEN_2022_PROGRAM }, { "encoding": "jsonParsed", "commitment": "confirmed" }]),
            rpc_url,
            RpcMode::Race,
        ),
        rpc_call_simple::<GetTokenAccountsResult>(
            "getTokenAccountsByOwner",
            serde_json::json!([wallet, { "programId": TOKEN_PROGRAM_STR }, { "encoding": "jsonParsed", "commitment": "confirmed" }]),
            rpc_url,
            RpcMode::Race,
        ),
    );

    let mut candidates = Vec::new();
    for (result, is_2022) in [(t2022_res, true), (tprog_res, false)] {
        for acc in result?.accounts() {
            let mint = &acc.account.data.parsed.info.mint;
            let amount = &acc.account.data.parsed.info.token_amount.amount;
            // Skip USDC ATA — user needs it for trading
            if mint == USDC_MINT { continue; }
            if amount == "0" {
                let pubkey = acc.pubkey.as_deref().unwrap_or("");
                let lamports = acc.account.lamports.unwrap_or(0);
                if !pubkey.is_empty() && lamports > 0 {
                    candidates.push(EmptyTokenAccount {
                        address: pubkey.to_string(),
                        mint: mint.clone(),
                        lamports,
                        is_token_2022: is_2022,
                    });
                }
            }
        }
    }

    if candidates.is_empty() {
        return Ok(candidates);
    }

    // Verify accounts still belong to a token program (RPC can return stale data
    // for accounts already closed — owner reverts to System Program).
    let t2022_str = super::TOKEN_2022_PROGRAM;
    let tprog_str = TOKEN_PROGRAM_STR;

    let mut verified = Vec::new();
    for chunk in candidates.chunks(100) {
        let addrs: Vec<&str> = chunk.iter().map(|a| a.address.as_str()).collect();
        let result: serde_json::Value = rpc_call_simple(
            "getMultipleAccounts",
            serde_json::json!([addrs, { "encoding": "base64", "commitment": "confirmed" }]),
            rpc_url,
            RpcMode::Race,
        ).await?;

        if let Some(values) = result.get("value").and_then(|v| v.as_array()) {
            for (i, val) in values.iter().enumerate() {
                if i >= chunk.len() { break; }
                // Account must exist AND be owned by the correct token program
                let owner = val.get("owner").and_then(|o| o.as_str()).unwrap_or("");
                let is_token_owned = owner == t2022_str || owner == tprog_str;
                if !val.is_null() && is_token_owned {
                    verified.push(chunk[i].clone());
                }
            }
        }
    }

    Ok(verified)
}

/// Close empty token accounts and reclaim rent. Returns (signatures, total_lamports).
/// Skips batches that fail (accounts already closed, etc.) and continues with the rest.
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

    // Token-2022 CloseAccount with pausable extension needs mint per account →
    // max ~8 accounts per tx to stay within the 1232-byte Solana limit.
    // Regular SPL Token CloseAccount doesn't need mints → can fit 15.
    let t2022_batch_size = 8;
    let tprog_batch_size = 15;

    let all_batches: Vec<(&[&EmptyTokenAccount], &[u8; 32], bool)> = token_2022.chunks(t2022_batch_size)
        .map(|b| (b, &TOKEN_2022_PROGRAM_ID, true))
        .chain(token_prog.chunks(tprog_batch_size).map(|b| (b, &TOKEN_PROGRAM, false)))
        .collect();

    for (batch, program, is_2022) in &all_batches {
        if !signatures.is_empty() {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        let batch_result = close_account_batch(wallet, &owner, batch, program, *is_2022, rpc_url).await;
        match batch_result {
            Ok(result) if result.confirmed => {
                total_lamports += batch.iter().map(|a| a.lamports).sum::<u64>();
                signatures.push(result.signature);
            }
            Ok(result) => {
                // Sent but not confirmed — don't count lamports
                eprintln!("Warning: tx {} sent but not confirmed", result.signature);
            }
            Err(e) => {
                // Batch failed (e.g. accounts already closed) — skip
                eprintln!("Warning: batch of {} accounts skipped: {e}", batch.len());
            }
        }
    }

    Ok((signatures, total_lamports))
}

/// Close a batch of empty token accounts in a single transaction.
/// For Token-2022 accounts with pausableAccount extension, the mint must be
/// included as an additional readonly account in the CloseAccount instruction.
async fn close_account_batch(
    wallet: &Wallet,
    owner: &[u8],
    batch: &[&EmptyTokenAccount],
    token_program: &[u8; 32],
    is_token_2022: bool,
    rpc_url: Option<&str>,
) -> Result<TransactionResult> {
    // Account layout:
    //   [0]        = owner/destination (signer, writable)
    //   [1..N]     = ATAs to close (writable)
    //   [N+1..M]   = mint addresses (readonly, Token-2022 only — needed for pausable extension)
    //   [last]     = token program (readonly)
    let mut accounts: Vec<Vec<u8>> = vec![owner.to_vec()];
    for acc in batch {
        let bytes = bs58::decode(&acc.address).into_vec()
            .map_err(|e| eyre!("Bad ATA address: {e}"))?;
        accounts.push(bytes);
    }

    // For Token-2022: add unique mint addresses (needed for pausableAccount extension check)
    let mut mint_indices: Vec<u8> = Vec::new();
    if is_token_2022 {
        let mut seen_mints = std::collections::HashMap::new();
        for acc in batch {
            let mint_idx = if let Some(&idx) = seen_mints.get(&acc.mint) {
                idx
            } else {
                let mint_bytes = bs58::decode(&acc.mint).into_vec()
                    .map_err(|e| eyre!("Bad mint address: {e}"))?;
                let idx = accounts.len() as u8;
                accounts.push(mint_bytes);
                seen_mints.insert(&acc.mint, idx);
                idx
            };
            mint_indices.push(mint_idx);
        }
    }

    accounts.push(token_program.to_vec());
    let program_idx = accounts.len() as u8 - 1;

    // readonly accounts = mints + token program
    let num_readonly_unsigned = if is_token_2022 {
        let unique_mints = mint_indices.iter().collect::<std::collections::HashSet<_>>().len();
        (unique_mints + 1) as u8 // mints + token program
    } else {
        1u8 // just token program
    };

    let mut instructions = Vec::new();
    for (i, _) in batch.iter().enumerate() {
        if is_token_2022 {
            // Token-2022 CloseAccount: [account, destination, owner, mint]
            instructions.push(Instruction {
                program_id_index: program_idx,
                account_indices: vec![(i + 1) as u8, 0, 0, mint_indices[i]],
                data: vec![9], // CloseAccount discriminator
            });
        } else {
            // SPL Token CloseAccount: [account, destination, owner]
            instructions.push(Instruction {
                program_id_index: program_idx,
                account_indices: vec![(i + 1) as u8, 0, 0],
                data: vec![9],
            });
        }
    }

    let header = MessageHeader {
        num_required_sigs: 1,
        num_readonly_signed: 0,
        num_readonly_unsigned,
    };

    send_legacy_transaction(wallet, &accounts, &instructions, &header, rpc_url, ConfirmLevel::Processed).await
}

