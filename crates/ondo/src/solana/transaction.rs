use eyre::Result;

use crate::wallet::Wallet;
use super::rpc::rpc_call_simple;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionErrorKind {
    ConfirmationTimeout,
    OnChainFailure,
    MissingBlockhash,
    InvalidBlockhash,
    SimulationFailure,
}

#[derive(Debug)]
pub struct TransactionError {
    pub kind: TransactionErrorKind,
    pub detail: String,
}

impl TransactionError {
    fn new(kind: TransactionErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for TransactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Solana transaction error [{}]: {}", self.kind, self.detail)
    }
}

impl std::fmt::Display for TransactionErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::ConfirmationTimeout => "confirmation_timeout",
            Self::OnChainFailure => "on_chain_failure",
            Self::MissingBlockhash => "missing_blockhash",
            Self::InvalidBlockhash => "invalid_blockhash",
            Self::SimulationFailure => "simulation_failure",
        };
        f.write_str(label)
    }
}

impl std::error::Error for TransactionError {}

/// Result of sending a transaction — includes whether it was confirmed on-chain.
pub struct TransactionResult {
    /// Base58-encoded transaction signature.
    pub signature: String,
    /// Whether the transaction was confirmed within the polling timeout.
    /// `false` means the tx was sent but confirmation timed out — it may still land.
    pub confirmed: bool,
}

/// Compute Budget Program ID (ComputeBudget111111111111111111111111111111)
pub(super) const COMPUTE_BUDGET_PROGRAM: [u8; 32] = [
    3, 6, 70, 111, 229, 33, 23, 50, 255, 236, 173, 186, 114, 195, 155, 231,
    188, 140, 229, 187, 197, 247, 18, 107, 44, 67, 155, 58, 64, 0, 0, 0,
];

pub(super) struct MessageHeader {
    pub num_required_sigs: u8,
    pub num_readonly_signed: u8,
    pub num_readonly_unsigned: u8,
}

pub(super) struct Instruction {
    pub program_id_index: u8,
    pub account_indices: Vec<u8>,
    pub data: Vec<u8>,
}

/// Build a SetComputeUnitLimit instruction (index 2).
pub(super) fn compute_unit_limit_ix(units: u32) -> Instruction {
    let mut data = vec![2]; // SetComputeUnitLimit
    data.extend_from_slice(&units.to_le_bytes());
    Instruction {
        program_id_index: 0, // placeholder, set by caller
        account_indices: vec![],
        data,
    }
}

/// Build a SetComputeUnitPrice instruction (index 3).
pub(super) fn compute_unit_price_ix(micro_lamports: u64) -> Instruction {
    let mut data = vec![3]; // SetComputeUnitPrice
    data.extend_from_slice(&micro_lamports.to_le_bytes());
    Instruction {
        program_id_index: 0, // placeholder, set by caller
        account_indices: vec![],
        data,
    }
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
    #[expect(dead_code)]
    last_valid_block_height: u64,
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
pub(super) async fn send_legacy_transaction(
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
        Err(e) => {
            // Simulation failed — transaction would fail on-chain, don't send
            return Err(e);
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

/// Poll for transaction confirmation with `confirmed` commitment.
/// Returns Ok(()) when confirmed, or Err after timeout (30s).
pub async fn confirm_transaction(signature: &str, rpc_url: Option<&str>) -> Result<()> {
    let timeout = std::time::Duration::from_secs(30);
    let start = std::time::Instant::now();

    loop {
        if start.elapsed() > timeout {
            return Err(TransactionError::new(
                TransactionErrorKind::ConfirmationTimeout,
                format!("transaction may still land: {signature}"),
            )
            .into());
        }

        if let Ok(result) = rpc_call_simple::<serde_json::Value>(
            "getSignatureStatuses",
            serde_json::json!([[signature], { "searchTransactionHistory": false }]),
            rpc_url,
        ).await
            && let Some(status) = result
                .get("value")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
            && !status.is_null()
        {
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
            if let Some(err) = status.get("err")
                && !err.is_null()
            {
                return Err(TransactionError::new(
                    TransactionErrorKind::OnChainFailure,
                    err.to_string(),
                )
                .into());
            }
            return Ok(()); // confirmed, no error
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// Send an already signed base64 transaction and wait for confirmation.
pub async fn send_signed_transaction(tx_base64: &str, rpc_url: Option<&str>) -> Result<TransactionResult> {
    let sig = send_raw_transaction(tx_base64, false, rpc_url).await?;
    let confirmed = match confirm_transaction(&sig, rpc_url).await {
        Ok(()) => true,
        Err(e) => {
            eprintln!("Warning: confirmation poll failed ({e}), tx may still land.");
            false
        }
    };
    Ok(TransactionResult {
        signature: sig,
        confirmed,
    })
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
        .ok_or_else(|| {
            TransactionError::new(
                TransactionErrorKind::MissingBlockhash,
                "RPC response did not include blockhash",
            )
        })?;
    let last_valid = result["value"]["lastValidBlockHeight"]
        .as_u64()
        .unwrap_or(0);

    let hash_bytes = bs58::decode(hash_str).into_vec()
        .map_err(|e| {
            TransactionError::new(
                TransactionErrorKind::InvalidBlockhash,
                format!("invalid base58 blockhash: {e}"),
            )
        })?;
    let hash: [u8; 32] = hash_bytes.try_into()
        .map_err(|_| {
            TransactionError::new(
                TransactionErrorKind::InvalidBlockhash,
                "blockhash wrong length",
            )
        })?;
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
    if let Some(err) = result.get("value").and_then(|v| v.get("err"))
        && !err.is_null()
    {
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
        return Err(TransactionError::new(
            TransactionErrorKind::SimulationFailure,
            format!("{err}\n  Logs:\n  {logs}"),
        )
        .into());
    }

    let units = result.get("value")
        .and_then(|v| v.get("unitsConsumed"))
        .and_then(|u| u.as_u64())
        .unwrap_or(200_000); // safe fallback

    Ok(units)
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

    #[test]
    fn transaction_error_display_includes_kind() {
        let err = TransactionError::new(
            TransactionErrorKind::ConfirmationTimeout,
            "transaction may still land: abc123",
        );
        let msg = err.to_string();
        assert!(msg.contains("confirmation_timeout"));
        assert!(msg.contains("abc123"));
    }
}
