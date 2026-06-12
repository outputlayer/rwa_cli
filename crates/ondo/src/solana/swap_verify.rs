//! On-chain simulation guard for Jupiter swaps.
//!
//! Jupiter's current swap/v2 aggregator (jupiterz, dflow, metis, okx, …) settles
//! the swap **inside** the router program via CPI — there is no top-level SPL
//! token transfer debiting the user's ATA. The static byte-parser in
//! [`crate::wallet::verify`] therefore cannot see the input/output legs of these
//! routes. This module is the authoritative economic check: it simulates the
//! exact transaction Jupiter returned (`sigVerify=false`, blockhash replaced) and
//! confirms, from real pre/post balances, that
//!
//!   * the owner's input-mint ATA is debited by **no more than** the expected
//!     amount (no overspend), and
//!   * the owner's output-mint ATA is **credited** (we receive the right mint).
//!
//! A simulation that the network would reject (`err != null`) is itself a
//! refusal to sign — the transaction would fail on-chain. If the RPC is
//! unreachable the check fails closed (we do not sign what we cannot verify).

use base64::Engine;
use eyre::Result;

use super::rpc::{RpcMode, rpc_call_simple};
use super::{TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID, derive_ata_pubkey};

/// Why a pre-sign swap simulation refused. The execute layer maps each to the
/// right action: an unfillable route is retried (with that router excluded), an
/// unsafe balance delta is a hard security refusal, and an unreachable RPC fails
/// closed.
#[derive(Debug)]
pub enum SwapSimError {
    /// The transaction would fail on-chain (e.g. an RFQ market maker that can't
    /// fill). Not a safety problem with *our* intent — a different route may work.
    OnChainWouldFail(String),
    /// The simulated balance deltas violate the swap intent (overspend, or the
    /// expected output mint not credited). A hard refusal — never retried.
    UnsafeDelta(String),
    /// The transaction credits the right mint but materially less than the quote
    /// promised (beyond the slippage tolerance) — a dishonest fill. Retryable
    /// with that router excluded; a different route may quote honestly.
    OutputBelowQuote(String),
    /// Could not reach an RPC to simulate. We do not sign what we cannot verify.
    RpcUnavailable(String),
}

impl std::fmt::Display for SwapSimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OnChainWouldFail(s) => write!(f, "swap would fail on-chain: {s}"),
            Self::UnsafeDelta(s) => write!(f, "swap simulation rejected: {s}"),
            Self::OutputBelowQuote(s) => write!(f, "swap under-delivers vs quote: {s}"),
            Self::RpcUnavailable(s) => write!(f, "could not simulate swap (RPC unavailable): {s}"),
        }
    }
}

impl std::error::Error for SwapSimError {}

/// SPL token `Account` layout: `mint[32] | owner[32] | amount[u64 LE] | …`.
/// The `amount` field sits at byte offset 64 for both classic Token and
/// Token-2022 (extensions, if any, follow the base 165-byte account).
const TOKEN_AMOUNT_OFFSET: usize = 64;

/// Decode the raw token `amount` from a base64 account `data` blob.
/// Returns `0` for `null`/short/non-token data so missing ATAs read as zero.
fn token_amount_from_account(account: Option<&serde_json::Value>) -> u128 {
    let Some(acc) = account else { return 0 };
    if acc.is_null() {
        return 0;
    }
    // data: ["<base64>", "base64"]
    let Some(b64) = acc.get("data").and_then(|d| d.get(0)).and_then(|s| s.as_str()) else {
        return 0;
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64) else {
        return 0;
    };
    if bytes.len() < TOKEN_AMOUNT_OFFSET + 8 {
        return 0;
    }
    let mut amt = [0u8; 8];
    amt.copy_from_slice(&bytes[TOKEN_AMOUNT_OFFSET..TOKEN_AMOUNT_OFFSET + 8]);
    u64::from_le_bytes(amt) as u128
}

/// Pure safety decision over the simulated balance deltas.
///
/// `Ok(())` only when the input debit is positive and within the expected
/// amount, and the output mint was credited at least `min_output` (the quoted
/// amount minus slippage tolerance; `0` disables the floor when the quote had
/// no parseable out-amount). Separated from I/O so it is unit tested directly —
/// this is the money-safety invariant.
fn deltas_are_safe(
    input_debited: u128,
    expected_input: u128,
    output_credited: u128,
    min_output: u128,
) -> std::result::Result<(), SwapSimError> {
    if input_debited == 0 {
        return Err(SwapSimError::UnsafeDelta(
            "debited nothing from the input mint".into(),
        ));
    }
    if input_debited > expected_input {
        return Err(SwapSimError::UnsafeDelta(format!(
            "would debit {input_debited} raw units, more than the expected {expected_input}"
        )));
    }
    if output_credited == 0 {
        return Err(SwapSimError::UnsafeDelta(
            "does not credit this wallet's expected output mint".into(),
        ));
    }
    if output_credited < min_output {
        return Err(SwapSimError::OutputBelowQuote(format!(
            "would credit {output_credited} raw units, below the quote floor {min_output} \
             (quote minus slippage tolerance)"
        )));
    }
    Ok(())
}

/// Minimum acceptable output for a quote: `quoted_out × (1 − tolerance)`.
/// `slippage_bps` comes from the order; when absent we allow up to 300 bps —
/// the same ceiling as the CLI's hard slippage block — rather than trusting
/// the route blindly. Returns 0 (floor disabled) when the quote amount is 0.
pub(crate) fn min_output_floor(quoted_out: u64, slippage_bps: Option<u32>) -> u64 {
    let tolerance = slippage_bps.unwrap_or(300).min(10_000) as u128;
    ((quoted_out as u128) * (10_000 - tolerance) / 10_000) as u64
}

/// Simulate `tx_base64` and verify it credibly performs the intended swap for
/// `owner`: debiting at most `input_amount` of `input_mint`, and crediting
/// `output_mint` by at least `min_output` (0 disables the floor). Returns `Err`
/// (refuse to sign) on any contradiction, an on-chain simulation failure, or an
/// unreachable RPC.
#[allow(clippy::too_many_arguments)]
pub async fn verify_swap_simulation(
    tx_base64: &str,
    input_mint: &[u8; 32],
    input_amount: u64,
    output_mint: &[u8; 32],
    min_output: u64,
    owner: &[u8; 32],
    rpc_url: Option<&str>,
) -> Result<()> {
    // The canonical ATAs we expect to move. Deriving for both token programs
    // means the wrong-program candidate simply doesn't exist (reads as 0); the
    // real ATA is whichever the mint actually uses.
    let input_atas = [
        derive_ata_pubkey(owner, input_mint, &TOKEN_PROGRAM_ID)?,
        derive_ata_pubkey(owner, input_mint, &TOKEN_2022_PROGRAM_ID)?,
    ];
    let output_atas = [
        derive_ata_pubkey(owner, output_mint, &TOKEN_PROGRAM_ID)?,
        derive_ata_pubkey(owner, output_mint, &TOKEN_2022_PROGRAM_ID)?,
    ];
    let watch: Vec<String> = input_atas
        .iter()
        .chain(output_atas.iter())
        .map(|a| bs58::encode(a).into_string())
        .collect();

    // Pre-balances (current on-chain state).
    let pre: serde_json::Value = rpc_call_simple(
        "getMultipleAccounts",
        serde_json::json!([watch, { "encoding": "base64", "commitment": "confirmed" }]),
        rpc_url,
        RpcMode::Race,
    )
    .await
    .map_err(|e| SwapSimError::RpcUnavailable(format!("reading pre-swap balances: {e}")))?;

    // Simulate the exact Jupiter transaction. sigVerify=false because the
    // market-maker fee-payer signature is absent pre-submit; we only want the
    // balance effect. replaceRecentBlockhash sidesteps blockhash staleness.
    let sim: serde_json::Value = rpc_call_simple(
        "simulateTransaction",
        serde_json::json!([
            tx_base64,
            {
                "encoding": "base64",
                "sigVerify": false,
                "replaceRecentBlockhash": true,
                "commitment": "confirmed",
                "accounts": { "encoding": "base64", "addresses": watch }
            }
        ]),
        rpc_url,
        RpcMode::Race,
    )
    .await
    .map_err(|e| SwapSimError::RpcUnavailable(format!("simulateTransaction call: {e}")))?;

    let value = sim.get("value").unwrap_or(&serde_json::Value::Null);
    if let Some(err) = value.get("err")
        && !err.is_null()
    {
        let logs = value
            .get("logs")
            .and_then(|l| l.as_array())
            .map(|logs| {
                logs.iter()
                    .filter_map(|l| l.as_str())
                    .collect::<Vec<_>>()
                    .join(" | ")
            })
            .unwrap_or_default();
        return Err(SwapSimError::OnChainWouldFail(format!("simulation error {err}: {logs}")).into());
    }

    let pre_accounts = pre.get("value").and_then(|v| v.as_array());
    let post_accounts = value.get("accounts").and_then(|v| v.as_array());

    let sum = |accounts: Option<&Vec<serde_json::Value>>, range: std::ops::Range<usize>| -> u128 {
        range
            .map(|i| token_amount_from_account(accounts.and_then(|a| a.get(i))))
            .sum()
    };
    // watch order: [input_token, input_token2022, output_token, output_token2022]
    let input_pre = sum(pre_accounts, 0..2);
    let input_post = sum(post_accounts, 0..2);
    let output_pre = sum(pre_accounts, 2..4);
    let output_post = sum(post_accounts, 2..4);

    let input_debited = input_pre.saturating_sub(input_post);
    let output_credited = output_post.saturating_sub(output_pre);

    deltas_are_safe(
        input_debited,
        input_amount as u128,
        output_credited,
        min_output as u128,
    )
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_when_exact_input_and_output_credited() {
        // -40 USDC, +191203991 NVDA (the empirically observed deltas).
        assert!(deltas_are_safe(40_000_000, 40_000_000, 191_203_991, 0).is_ok());
    }

    #[test]
    fn safe_when_input_slightly_under_expected() {
        assert!(deltas_are_safe(39_900_000, 40_000_000, 1_000, 0).is_ok());
    }

    #[test]
    fn rejects_overspend() {
        // Debiting more than expected is the core attack we must block.
        assert!(deltas_are_safe(40_000_001, 40_000_000, 1_000, 0).is_err());
    }

    #[test]
    fn rejects_no_input_debit() {
        assert!(deltas_are_safe(0, 40_000_000, 1_000, 0).is_err());
    }

    #[test]
    fn rejects_no_output_credit() {
        // Funds leave but nothing of the expected output mint arrives.
        assert!(deltas_are_safe(40_000_000, 40_000_000, 0, 0).is_err());
    }

    #[test]
    fn rejects_output_below_quote_tolerance() {
        // Live incident (2026-06-12): jupiterz quoted 40_273_156 raw AAPLon for
        // 12 USDC but the signed tx credited only 33_272_886 (−17.3%). With a
        // 100 bps tolerance, min acceptable = 39_870_424 — must refuse to sign
        // and be retryable with the router excluded.
        let err = deltas_are_safe(12_000_000, 12_000_000, 33_272_886, 39_870_424)
            .expect_err("under-delivery must be refused");
        assert!(
            matches!(err, SwapSimError::OutputBelowQuote(_)),
            "wrong error kind: {err:?}"
        );
    }

    #[test]
    fn accepts_output_within_tolerance() {
        // Credited slightly above the floor → fine.
        assert!(deltas_are_safe(12_000_000, 12_000_000, 39_900_000, 39_870_424).is_ok());
        // Credited exactly at the floor → fine.
        assert!(deltas_are_safe(12_000_000, 12_000_000, 39_870_424, 39_870_424).is_ok());
    }

    #[test]
    fn zero_min_output_disables_floor_check() {
        // Degraded mode (quote had no parseable out_amount): legacy invariants only.
        assert!(deltas_are_safe(12_000_000, 12_000_000, 1, 0).is_ok());
    }

    #[test]
    fn min_output_floor_from_quote_applies_tolerance() {
        // 40_273_156 quoted, 100 bps tolerance → floor 39_870_424 (rounded down).
        assert_eq!(min_output_floor(40_273_156, Some(100)), 39_870_424);
        // No slippage on the order → conservative 300 bps (the hard-block ceiling).
        assert_eq!(min_output_floor(40_273_156, None), 39_064_961);
        // Nonsense tolerance is clamped, never underflows.
        assert_eq!(min_output_floor(40_273_156, Some(20_000)), 0);
        assert_eq!(min_output_floor(0, Some(100)), 0);
    }

    #[test]
    fn token_amount_decodes_offset_64() {
        // Build a 72-byte token account: mint[32] owner[32] amount[8].
        let mut data = vec![0u8; 72];
        let amount: u64 = 123_456_789;
        data[64..72].copy_from_slice(&amount.to_le_bytes());
        let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
        let acc = serde_json::json!({ "data": [b64, "base64"] });
        assert_eq!(token_amount_from_account(Some(&acc)), 123_456_789);
    }

    #[test]
    fn token_amount_zero_for_null_or_short() {
        assert_eq!(token_amount_from_account(None), 0);
        assert_eq!(token_amount_from_account(Some(&serde_json::Value::Null)), 0);
        let short = serde_json::json!({ "data": ["AAAA", "base64"] });
        assert_eq!(token_amount_from_account(Some(&short)), 0);
    }
}
