use eyre::{Result, eyre};
use serde::{Deserialize, Serialize};

use crate::wallet::Wallet;
use crate::HTTP;

const SWAP_API_BASE: &str = "https://lite-api.jup.ag/swap/v2";
pub use crate::USDC_MINT;
/// Wrapped SOL on Solana
pub const SOL_MINT: &str = "So11111111111111111111111111111111111111112";
pub const USDC_DECIMALS: u8 = 6;
/// Ondo GM tokens on Solana use 9 decimals (Solana standard).
pub const GM_SOL_DECIMALS: u8 = 9;

// ── API types ──────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderResponse {
    pub request_id: String,
    pub in_amount: String,
    pub out_amount: String,
    pub in_usd_value: Option<f64>,
    pub out_usd_value: Option<f64>,
    /// Price impact as decimal (e.g. -0.001 = -0.1%).
    pub price_impact: Option<f64>,
    /// Price impact as string for precise display (deprecated in V2, use price_impact).
    pub price_impact_pct: Option<String>,
    /// Slippage in basis points.
    pub slippage_bps: Option<u32>,
    /// Jupiter fee in basis points.
    pub fee_bps: Option<u32>,
    pub transaction: Option<String>,
    pub error: Option<String>,
    pub error_message: Option<String>,
    // ── Swap V2 fields ─────────────────────────────────────────
    /// Whether this swap is gasless (Jupiter/MM pays gas).
    pub gasless: Option<bool>,
    /// Which router won the quote: iris, jupiterz, dflow, okx.
    pub router: Option<String>,
    /// Rent fee in lamports for ATA creation (if needed).
    pub rent_fee_lamports: Option<u64>,
    /// Who pays the rent: "jupiter", "user", or null.
    pub rent_fee_payer: Option<String>,
    /// Signature fee in lamports.
    pub signature_fee_lamports: Option<u64>,
    /// Who pays signature fee.
    pub signature_fee_payer: Option<String>,
    /// Priority fee in lamports (includes Jito tips).
    pub prioritization_fee_lamports: Option<u64>,
    /// Who pays priority fee.
    pub prioritization_fee_payer: Option<String>,
    /// "ultra" or "manual".
    pub mode: Option<String>,
    /// Last valid block height for the transaction.
    pub last_valid_block_height: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecuteRequest {
    request_id: String,
    signed_transaction: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteResponse {
    pub status: Option<String>,
    pub signature: Option<String>,
    pub code: Option<i32>,
    pub error: Option<String>,
    pub error_message: Option<String>,
    pub input_amount_result: Option<String>,
    pub output_amount_result: Option<String>,
}

// ── Public functions ───────────────────────────────────────────────────

/// Get a swap quote from Jupiter Swap V2 API.
/// Uses lite-api.jup.ag/swap/v2 — no API key required.
/// Flow: /order → wallet.sign → /execute
/// Maximum retries for transient network errors on /order.
const ORDER_MAX_RETRIES: u32 = 2;

pub async fn get_order(
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
) -> Result<OrderResponse> {
    let mut last_err = eyre!("Jupiter /order failed");

    for attempt in 0..=ORDER_MAX_RETRIES {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }

        let mut request = HTTP
            .get(format!("{SWAP_API_BASE}/order"))
            .query(&[
                ("inputMint", input_mint),
                ("outputMint", output_mint),
                ("amount", amount),
                ("taker", taker),
            ]);

        if let Some(bps) = slippage_bps {
            request = request.query(&[("slippageBps", bps.to_string())]);
        }

        let response = match request.send().await {
            Ok(r) => r,
            Err(e) if is_transient(&e) && attempt < ORDER_MAX_RETRIES => {
                last_err = eyre!("Jupiter /order network error: {e}");
                continue;
            }
            Err(e) => return Err(eyre!("Jupiter /order network error: {e}")),
        };

        let status = response.status();

        // 429 / 5xx → retry
        if (status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error())
            && attempt < ORDER_MAX_RETRIES
        {
            last_err = eyre!("Jupiter /order HTTP {status}");
            continue;
        }

        let body = response.text().await?;

        if !status.is_success() {
            if status.as_u16() == 400 && body.contains("Failed to get quotes") {
                return Err(eyre!("No swap route found. Do not run quotes in parallel — Jupiter rejects concurrent requests from the same wallet."));
            }
            return Err(eyre!("Jupiter API error (HTTP {status}): {body}"));
        }

        let resp: OrderResponse = serde_json::from_str(&body)
            .map_err(|e| eyre!("Failed to parse Jupiter response: {e}\nBody: {body}"))?;

        if let Some(err) = &resp.error {
            let msg = resp.error_message.as_deref().unwrap_or(err);
            return Err(eyre!("Jupiter API error: {msg}"));
        }

        let tx = resp.transaction.as_deref().unwrap_or("");
        if tx.is_empty() {
            let detail = resp.error_message.as_deref()
                .or(resp.error.as_deref())
                .unwrap_or("route may not exist");
            return Err(eyre!("Jupiter returned empty transaction — {detail}"));
        }

        return Ok(resp);
    }

    Err(last_err)
}

/// Check if a reqwest error is transient (timeout, connection, DNS).
fn is_transient(e: &reqwest::Error) -> bool {
    e.is_timeout() || e.is_connect() || e.is_request()
}

/// Sign and execute a swap via Jupiter Swap V2 API.
pub async fn execute_order(
    wallet: &Wallet,
    order: &OrderResponse,
) -> Result<ExecuteResponse> {
    let tx_b64 = order.transaction.as_deref()
        .ok_or_else(|| eyre!("No transaction in order"))?;
    let signed_tx = wallet.sign_transaction(tx_b64)?;

    let req = ExecuteRequest {
        request_id: order.request_id.clone(),
        signed_transaction: signed_tx,
    };

    let response = HTTP
        .post(format!("{SWAP_API_BASE}/execute"))
        .json(&req)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;

    if !status.is_success() {
        return Err(eyre!("Jupiter execute error (HTTP {status}): {body}"));
    }

    let resp: ExecuteResponse = serde_json::from_str(&body)
        .map_err(|e| eyre!("Failed to parse Jupiter execute response: {e}\nBody: {body}"))?;

    check_execute_result(&resp)?;
    Ok(resp)
}

fn check_execute_result(resp: &ExecuteResponse) -> Result<()> {
    if let Some(status) = &resp.status {
        if status == "Failed" {
            let raw_msg = resp.error_message.as_deref()
                .or(resp.error.as_deref())
                .unwrap_or("Unknown execution error");
            let hint = resp.code.map(|c| match c {
                -1 => " (missing cached order — retry)",
                -2 => " (invalid signed transaction)",
                -3 => " (invalid message bytes)",
                -1000 => " (failed to land — retry)",
                -1001 => " (unknown aggregator error)",
                -2000 => " (RFQ failed to land — retry)",
                -2001 => " (unknown RFQ error)",
                -2002 => " (invalid payload)",
                -2003 => " (quote expired — retry)",
                -2004 => " (swap rejected)",
                -2005 => " (internal error — retry)",
                _ => "",
            }).unwrap_or("");
            let code = resp.code.map(|c| format!(" (code {c})")).unwrap_or_default();
            return Err(eyre!("Swap failed{code}: {raw_msg}{hint}"));
        }
    }

    if resp.signature.is_none() {
        let msg = resp.error_message.as_deref()
            .or(resp.error.as_deref())
            .unwrap_or("No signature returned — transaction may have failed");
        return Err(eyre!("Swap failed: {msg}"));
    }
    Ok(())
}

/// Convert a human-readable USDC amount (e.g. "100.50") to on-chain units (6 decimals).
pub fn usdc_to_raw(amount: &str) -> Result<String> {
    token_to_raw(amount, USDC_DECIMALS)
}

/// Convert a human-readable token amount to on-chain units with specified decimals.
pub fn token_to_raw(amount: &str, decimals: u8) -> Result<String> {
    let amount = amount.trim();
    if amount.is_empty() {
        return Err(eyre!("Amount must be positive"));
    }
    if amount.starts_with('-') || amount.starts_with('+') {
        return Err(eyre!("Amount must be positive"));
    }

    let d = decimals as usize;
    let parts: Vec<&str> = amount.split('.').collect();
    let (integer, frac) = match parts.len() {
        1 => (parts[0], ""),
        2 => (parts[0], parts[1]),
        _ => return Err(eyre!("Invalid amount format")),
    };
    let integer = if integer.is_empty() { "0" } else { integer };
    if !integer.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
        return Err(eyre!("Invalid amount format"));
    }
    if frac.len() > d {
        return Err(eyre!(
            "Too many decimal places: got {}, max allowed is {d}",
            frac.len()
        ));
    }

    let frac_padded = format!("{:0<width$}", frac, width = d);
    let frac_trimmed = &frac_padded[..d];
    let raw = format!("{integer}{frac_trimmed}");
    let raw = raw.trim_start_matches('0');
    if raw.is_empty() {
        return Err(eyre!("Amount must be positive"));
    }
    Ok(raw.to_string())
}

/// Format on-chain amount to human-readable with given decimals.
#[must_use]
pub fn format_amount(raw: &str, decimals: u8) -> String {
    let d = decimals as usize;
    if raw.len() <= d {
        let zeros = d - raw.len();
        let frac = format!("{}{}", "0".repeat(zeros), raw);
        let frac = frac.trim_end_matches('0');
        if frac.is_empty() {
            "0".to_string()
        } else {
            format!("0.{frac}")
        }
    } else {
        let (integer, frac) = raw.split_at(raw.len() - d);
        let frac = frac.trim_end_matches('0');
        if frac.is_empty() {
            integer.to_string()
        } else {
            format!("{integer}.{frac}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── token_to_raw ──────────────────────────────────────────

    #[test]
    fn usdc_whole_number() {
        assert_eq!(usdc_to_raw("100").unwrap(), "100000000");
    }

    #[test]
    fn usdc_with_decimals() {
        assert_eq!(usdc_to_raw("100.50").unwrap(), "100500000");
    }

    #[test]
    fn usdc_small_amount() {
        assert_eq!(usdc_to_raw("0.01").unwrap(), "10000");
    }

    #[test]
    fn usdc_min_amount() {
        assert_eq!(usdc_to_raw("0.000001").unwrap(), "1");
    }

    #[test]
    fn usdc_truncates_extra_decimals() {
        assert!(usdc_to_raw("1.1234567").is_err());
    }

    #[test]
    fn token_9_decimals() {
        assert_eq!(token_to_raw("1", 9).unwrap(), "1000000000");
    }

    #[test]
    fn token_fractional_9_dec() {
        assert_eq!(token_to_raw("0.5", 9).unwrap(), "500000000");
    }

    #[test]
    fn token_tiny_amount() {
        assert_eq!(token_to_raw("0.000000001", 9).unwrap(), "1");
    }

    #[test]
    fn token_rejects_zero() {
        assert!(token_to_raw("0", 6).is_err());
    }

    #[test]
    fn token_rejects_negative() {
        assert!(token_to_raw("-5", 6).is_err());
    }

    #[test]
    fn token_rejects_garbage() {
        assert!(token_to_raw("abc", 6).is_err());
    }

    #[test]
    fn token_rejects_double_dot() {
        assert!(token_to_raw("1.2.3", 6).is_err());
    }

    // ── format_amount ─────────────────────────────────────────

    #[test]
    fn format_whole_usdc() {
        assert_eq!(format_amount("100000000", 6), "100");
    }

    #[test]
    fn format_fractional_usdc() {
        assert_eq!(format_amount("100500000", 6), "100.5");
    }

    #[test]
    fn format_tiny_usdc() {
        assert_eq!(format_amount("1", 6), "0.000001");
    }

    #[test]
    fn format_zero() {
        assert_eq!(format_amount("0", 6), "0");
    }

    #[test]
    fn format_gm_token_9dec() {
        assert_eq!(format_amount("1000000000", 9), "1");
    }

    #[test]
    fn format_gm_token_fractional() {
        assert_eq!(format_amount("500000000", 9), "0.5");
    }

    #[test]
    fn format_gm_token_tiny() {
        assert_eq!(format_amount("1", 9), "0.000000001");
    }

    #[test]
    fn format_large_amount() {
        assert_eq!(format_amount("999999999999", 6), "999999.999999");
    }

    // ── round-trip: token_to_raw → format_amount ──────────────

    #[test]
    fn roundtrip_usdc() {
        let raw = usdc_to_raw("123.45").unwrap();
        assert_eq!(format_amount(&raw, 6), "123.45");
    }

    #[test]
    fn roundtrip_gm_token() {
        let raw = token_to_raw("0.123456789", 9).unwrap();
        assert_eq!(format_amount(&raw, 9), "0.123456789");
    }

    #[test]
    fn roundtrip_whole_number() {
        let raw = token_to_raw("42", 9).unwrap();
        assert_eq!(format_amount(&raw, 9), "42");
    }

    // ── edge cases ───────────────────────────────────────────

    #[test]
    fn token_large_whole_number() {
        assert_eq!(token_to_raw("999999", 6).unwrap(), "999999000000");
    }

    #[test]
    fn token_exact_decimals() {
        // Exactly 6 decimal digits for USDC
        assert_eq!(usdc_to_raw("1.123456").unwrap(), "1123456");
    }

    #[test]
    fn token_rejects_extra_decimals() {
        assert!(token_to_raw("0.1234567891", 9).is_err());
    }

    #[test]
    fn format_amount_with_leading_zeros_in_frac() {
        // 100001 with 6 decimals = "0.100001"
        assert_eq!(format_amount("100001", 6), "0.100001");
    }

    #[test]
    fn format_amount_single_digit_raw() {
        assert_eq!(format_amount("5", 6), "0.000005");
    }

    #[test]
    fn roundtrip_many_values() {
        for (input, dec) in [("100", 6), ("0.5", 6), ("1.23", 9), ("999.999999", 6)] {
            let raw = token_to_raw(input, dec).unwrap();
            let formatted = format_amount(&raw, dec);
            assert_eq!(formatted, input, "roundtrip failed for {input} with {dec} decimals");
        }
    }
}

// ── Property-based tests ─────────────────────────────────────────────────────
// Verifies invariants that hold for ALL valid inputs, not just hand-picked examples.

#[cfg(test)]
mod prop_tests {
    use super::*;
    use proptest::prelude::*;

    /// Canonical form of a decimal string: no leading integer zeros, no trailing
    /// fractional zeros, no trailing decimal point.
    fn canonical(s: &str) -> String {
        match s.find('.') {
            None => {
                let t = s.trim_start_matches('0');
                if t.is_empty() { "0".into() } else { t.into() }
            }
            Some(dot) => {
                let int  = s[..dot].trim_start_matches('0');
                let frac = s[dot + 1..].trim_end_matches('0');
                let int  = if int.is_empty() { "0" } else { int };
                if frac.is_empty() { int.into() } else { format!("{int}.{frac}") }
            }
        }
    }

    proptest! {
        /// token_to_raw followed by format_amount returns the canonical decimal form
        /// for any positive amount with at most `decimals` fractional digits.
        #[test]
        fn roundtrip_6_decimals(
            integer in 0u32..=1_000_000u32,
            // frac is 0–999_999; we pad to exactly 6 digits below
            frac    in 0u32..=999_999u32,
        ) {
            // Skip the zero case
            if integer == 0 && frac == 0 { return Ok(()); }

            let input = format!("{integer}.{frac:06}");
            let expected = canonical(&input);
            if expected == "0" { return Ok(()); }

            let raw  = token_to_raw(&expected, 6).unwrap();
            let back = format_amount(&raw, 6);
            prop_assert_eq!(&back, &expected);
        }

        /// Same property for 9-decimal GM tokens.
        #[test]
        fn roundtrip_9_decimals(
            integer in 0u32..=100_000u32,
            frac    in 0u32..=999_999_999u32,
        ) {
            if integer == 0 && frac == 0 { return Ok(()); }

            let input    = format!("{integer}.{frac:09}");
            let expected = canonical(&input);
            if expected == "0" { return Ok(()); }

            let raw  = token_to_raw(&expected, 9).unwrap();
            let back = format_amount(&raw, 9);
            prop_assert_eq!(&back, &expected);
        }

        /// format_amount never produces a string with a leading zero on the integer
        /// part (other than the canonical "0" itself) or trailing zeros.
        #[test]
        fn format_amount_is_canonical(raw_digits in "[1-9][0-9]{0,14}", decimals in 0u8..=9u8) {
            let out = format_amount(&raw_digits, decimals);
            // No trailing zeros after decimal point
            if out.contains('.') {
                prop_assert!(!out.ends_with('0'), "trailing zero in {out:?}");
            }
            // Integer part has no leading zero (unless the whole thing is "0.xxx")
            let int_part = out.split('.').next().unwrap();
            if int_part != "0" {
                prop_assert!(!int_part.starts_with('0'), "leading zero in {out:?}");
            }
        }

        /// token_to_raw always rejects zero and negative amounts.
        #[test]
        fn token_to_raw_rejects_non_positive(
            sign in prop::sample::select(&["-", ""]),
            magnitude in 0u64..=1_000_000u64,
        ) {
            if magnitude == 0 {
                // "0", "-0", "0.0", etc. must all fail
                prop_assert!(token_to_raw("0", 6).is_err());
                prop_assert!(token_to_raw("-0", 6).is_err());
            } else if !sign.is_empty() {
                let s = format!("{sign}{magnitude}");
                prop_assert!(token_to_raw(&s, 6).is_err(), "should reject {s}");
            }
        }
    }
}
