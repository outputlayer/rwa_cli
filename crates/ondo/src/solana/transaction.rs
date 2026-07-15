use eyre::Result;

use crate::wallet::Wallet;
use super::rpc::{rpc_call_simple, RpcMode};

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

impl TransactionErrorKind {
    /// Stable label surfaced as JSON `error_kind` for agents/scripts.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::ConfirmationTimeout => "confirmation_timeout",
            Self::OnChainFailure => "on_chain_failure",
            Self::MissingBlockhash => "missing_blockhash",
            Self::InvalidBlockhash => "invalid_blockhash",
            Self::SimulationFailure => "simulation_failure",
        }
    }
}

impl std::fmt::Display for TransactionErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
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
    level: ConfirmLevel,
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

    // 5. Confirm — poll until the requested commitment level
    let confirmed = match confirm_transaction(&sig, rpc_url, level).await {
        Ok(()) => true,
        Err(e) => {
            eprintln!("Warning: confirmation poll failed ({e}), tx may still land.");
            false
        }
    };

    Ok(TransactionResult { signature: sig, confirmed })
}

/// Commitment level `confirm_transaction` waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmLevel {
    /// Tx is in a block the RPC has replayed — `err` is authoritative, but a
    /// minority fork can still drop it. Use only for low-stakes transactions
    /// (rent reclaim), where the latency win outweighs the rare re-check.
    Processed,
    /// Optimistic confirmation (supermajority vote) — the default for anything
    /// that moves funds.
    Confirmed,
}

/// True when the RPC-reported `confirmationStatus` satisfies `level`.
/// An absent status (`None`) never satisfies any level — keep polling and let
/// the timeout decide; never report a confirmation the RPC didn't assert.
fn status_meets_level(status: Option<&str>, level: ConfirmLevel) -> bool {
    matches!(
        (level, status),
        (ConfirmLevel::Processed, Some("processed" | "confirmed" | "finalized"))
            | (ConfirmLevel::Confirmed, Some("confirmed" | "finalized"))
    )
}

/// Poll for transaction confirmation up to the requested commitment level.
/// Returns Ok(()) when reached, or Err after timeout (30s).
pub async fn confirm_transaction(
    signature: &str,
    rpc_url: Option<&str>,
    level: ConfirmLevel,
) -> Result<()> {
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
            RpcMode::Race,
        ).await
            && let Some(status) = result
                .get("value")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
            && !status.is_null()
        {
            let level_reached = status_meets_level(
                status.get("confirmationStatus").and_then(|s| s.as_str()),
                level,
            );
            if !level_reached {
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
            return Ok(()); // level reached, no error
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// Send an already signed base64 transaction and wait for `confirmed`
/// commitment (this path carries swaps — always full confirmation).
pub async fn send_signed_transaction(tx_base64: &str, rpc_url: Option<&str>) -> Result<TransactionResult> {
    let sig = send_raw_transaction(tx_base64, false, rpc_url).await?;
    let confirmed = match confirm_transaction(&sig, rpc_url, ConfirmLevel::Confirmed).await {
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
        RpcMode::Sequential,
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
        RpcMode::Sequential,
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
        RpcMode::Race,
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
        // Independently specified encodings (Solana short-vec format): the
        // expected bytes are written out by hand, not re-derived via bit math
        // that could share a bug with the encoder.
        let mut buf = Vec::new();
        encode_compact_u16(128, &mut buf);
        assert_eq!(buf, vec![0x80, 0x01]);

        buf.clear();
        encode_compact_u16(255, &mut buf);
        assert_eq!(buf, vec![0xff, 0x01]);
    }

    #[test]
    fn status_meets_level_matrix() {
        use ConfirmLevel::*;
        assert!(status_meets_level(Some("processed"), Processed));
        assert!(status_meets_level(Some("confirmed"), Processed));
        assert!(status_meets_level(Some("finalized"), Processed));
        assert!(status_meets_level(Some("confirmed"), Confirmed));
        assert!(status_meets_level(Some("finalized"), Confirmed));
        assert!(!status_meets_level(Some("processed"), Confirmed));
        // Absent or unknown status never satisfies any level.
        assert!(!status_meets_level(None, Processed));
        assert!(!status_meets_level(None, Confirmed));
        assert!(!status_meets_level(Some("bogus"), Processed));
    }

    #[test]
    fn compact_u16_three_bytes() {
        let mut buf = Vec::new();
        encode_compact_u16(0x4000, &mut buf);
        assert_eq!(buf, vec![0x80, 0x80, 0x01]);

        buf.clear();
        encode_compact_u16(u16::MAX, &mut buf);
        assert_eq!(buf, vec![0xff, 0xff, 0x03]);
    }

    #[test]
    fn compact_u16_roundtrip_against_independent_decoder() {
        // Round-trip through the wallet parser's decoder — a separate
        // implementation of the short-vec spec, so a matching encode/decode
        // bug in this file cannot cancel out.
        for val in [0, 1, 127, 128, 255, 256, 1000, 16383, 16384, 65535u16] {
            let mut buf = Vec::new();
            encode_compact_u16(val, &mut buf);
            let (decoded, consumed) = crate::wallet::parser::decode_compact_u16(&buf)
                .expect("parser decodes our encoding");
            assert_eq!(decoded, val);
            assert_eq!(consumed, buf.len());
        }
    }

    // ── send_legacy_transaction pipeline (mock RPC) ──────────

    /// Deterministic wallet from the repo's reference vector, so the
    /// non-capturing httpmock matcher below can hardcode its pubkey.
    const PIPELINE_TEST_SECRET: &str =
        "37df573b3ac4ad5b522e064e25b63ea16bcbe79d449e81a0268d1047948bb445";
    const PIPELINE_TEST_PUBKEY: &str = "HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpqk";
    /// Blockhash the mock hands out — must appear verbatim in the sent message.
    const PIPELINE_TEST_BLOCKHASH: [u8; 32] = [7u8; 32];

    /// Contract matcher for the submitted transaction. Returns true only when
    /// the wire bytes prove the whole pipeline worked:
    ///   1. a real ed25519 signature by our wallet over the message,
    ///   2. the mocked blockhash embedded in the message,
    ///   3. the CU limit tightened to unitsConsumed × 1.1 (50_000 → 55_000).
    ///
    /// `MockMatcherFunction` is a plain fn pointer, hence the constants above.
    fn sent_tx_satisfies_pipeline_contract(req: &httpmock::prelude::HttpMockRequest) -> bool {
        use base64::Engine;
        use crate::wallet::parser::decode_compact_u16 as dec;

        let Some(body) = req.body.as_ref() else { return false };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else { return false };
        if v["method"] != "sendTransaction" {
            return false;
        }
        let Some(tx_b64) = v["params"][0].as_str() else { return false };
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(tx_b64) else {
            return false;
        };

        // 1. Signature verifies against the wallet pubkey over the message.
        let Ok((num_sigs, prefix)) = dec(&bytes) else { return false };
        if num_sigs != 1 {
            return false;
        }
        let sigs_end = prefix + 64;
        let message = &bytes[sigs_end..];
        let Ok(pk_vec) = bs58::decode(PIPELINE_TEST_PUBKEY).into_vec() else { return false };
        let Ok(pk): std::result::Result<[u8; 32], _> = pk_vec.try_into() else { return false };
        let Ok(vk) = ed25519_dalek::VerifyingKey::from_bytes(&pk) else { return false };
        let Ok(sig_bytes): std::result::Result<[u8; 64], _> =
            bytes[prefix..sigs_end].try_into() else { return false };
        let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
        if vk.verify_strict(message, &sig).is_err() {
            return false;
        }

        // 2 + 3. Walk the message: header, accounts, blockhash, instructions.
        let mut pos = 3; // header bytes
        let Ok((n_accounts, c)) = dec(&message[pos..]) else { return false };
        pos += c + n_accounts as usize * 32;
        if message.len() < pos + 32 || message[pos..pos + 32] != PIPELINE_TEST_BLOCKHASH {
            return false;
        }
        pos += 32;
        let Ok((n_ix, c)) = dec(&message[pos..]) else { return false };
        pos += c;
        let mut cu_limit_tightened = false;
        for _ in 0..n_ix {
            pos += 1; // program_id_index
            let Ok((n_acc, c)) = dec(&message[pos..]) else { return false };
            pos += c + n_acc as usize;
            let Ok((dlen, c)) = dec(&message[pos..]) else { return false };
            pos += c;
            let data = &message[pos..pos + dlen as usize];
            pos += dlen as usize;
            if data.first() == Some(&2) && data.len() == 5 {
                let units = u32::from_le_bytes(data[1..5].try_into().unwrap());
                if units != 55_000 {
                    return false; // CU present but not tightened correctly
                }
                cu_limit_tightened = true;
            }
        }
        cu_limit_tightened
    }

    #[tokio::test]
    async fn send_legacy_pipeline_tightens_cu_signs_and_confirms() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;

        server.mock_async(|when, then| {
            when.method(POST).body_contains("getLatestBlockhash");
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0", "id": 1,
                "result": { "value": {
                    "blockhash": bs58::encode(PIPELINE_TEST_BLOCKHASH).into_string(),
                    "lastValidBlockHeight": 100
                }}
            }));
        }).await;
        server.mock_async(|when, then| {
            when.method(POST).body_contains("simulateTransaction");
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0", "id": 1,
                "result": { "value": { "err": null, "unitsConsumed": 50_000, "logs": [] } }
            }));
        }).await;
        let fake_sig = bs58::encode([9u8; 64]).into_string();
        let sent = server.mock_async(|when, then| {
            when.method(POST)
                .body_contains("sendTransaction")
                .matches(sent_tx_satisfies_pipeline_contract);
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0", "id": 1,
                "result": bs58::encode([9u8; 64]).into_string()
            }));
        }).await;
        server.mock_async(|when, then| {
            when.method(POST).body_contains("getSignatureStatuses");
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0", "id": 1,
                "result": { "value": [ { "confirmationStatus": "confirmed", "err": null } ] }
            }));
        }).await;

        let wallet = Wallet::from_private_key(PIPELINE_TEST_SECRET).unwrap();
        assert_eq!(wallet.pubkey(), PIPELINE_TEST_PUBKEY);
        let tx = super::super::transfer::test_fixture_sol_transfer(&wallet, 1_000_000);

        let result = send_legacy_transaction(
            &wallet, &tx.0, &tx.1, &tx.2,
            Some(&server.base_url()),
            ConfirmLevel::Confirmed,
        )
        .await
        .expect("pipeline must succeed against honest RPC");

        // The strict matcher accepted exactly one submission, and the caller
        // got the network's signature back, confirmed.
        sent.assert_hits_async(1).await;
        assert_eq!(result.signature, fake_sig);
        assert!(result.confirmed);
    }

    #[tokio::test]
    async fn send_legacy_pipeline_fails_closed_when_simulation_fails() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;

        server.mock_async(|when, then| {
            when.method(POST).body_contains("getLatestBlockhash");
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0", "id": 1,
                "result": { "value": {
                    "blockhash": bs58::encode(PIPELINE_TEST_BLOCKHASH).into_string(),
                    "lastValidBlockHeight": 100
                }}
            }));
        }).await;
        server.mock_async(|when, then| {
            when.method(POST).body_contains("simulateTransaction");
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0", "id": 1,
                "result": { "value": {
                    "err": { "InstructionError": [1, "Custom"] },
                    "logs": ["Program log: would fail on-chain"]
                }}
            }));
        }).await;
        let sent = server.mock_async(|when, then| {
            when.method(POST).body_contains("sendTransaction");
            then.status(200).json_body(serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "result": "never"
            }));
        }).await;

        let wallet = Wallet::from_private_key(PIPELINE_TEST_SECRET).unwrap();
        let tx = super::super::transfer::test_fixture_sol_transfer(&wallet, 1_000_000);

        let err = match send_legacy_transaction(
            &wallet, &tx.0, &tx.1, &tx.2,
            Some(&server.base_url()),
            ConfirmLevel::Confirmed,
        )
        .await
        {
            Ok(_) => panic!("failing simulation must abort the pipeline"),
            Err(e) => e,
        };

        // Fail closed: NOTHING was submitted to the network.
        sent.assert_hits_async(0).await;
        let te = err.downcast_ref::<TransactionError>().expect("typed error");
        assert_eq!(te.kind, TransactionErrorKind::SimulationFailure);
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
