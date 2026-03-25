use eyre::{Result, eyre};
use serde::{Deserialize, Serialize};

use crate::wallet::Wallet;
use crate::HTTP;

const ULTRA_API_BASE: &str = "https://lite-api.jup.ag/ultra/v1";
pub use crate::USDC_MINT;
/// Wrapped SOL on Solana
pub const SOL_MINT: &str = "So11111111111111111111111111111111111111112";
pub const USDC_DECIMALS: u8 = 6;
/// Ondo GM tokens on Solana use 9 decimals (Solana standard).
pub const GM_SOL_DECIMALS: u8 = 9;

// ── API types ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderResponse {
    pub request_id: String,
    pub in_amount: String,
    pub out_amount: String,
    pub in_usd_value: Option<f64>,
    pub out_usd_value: Option<f64>,
    /// Price impact as percentage (from Jupiter API).
    pub price_impact: Option<f64>,
    /// Price impact as string for precise display (e.g. "-0.00012376932391608222").
    pub price_impact_pct: Option<String>,
    /// Slippage in basis points (set by Jupiter, typically 0 for Ultra API).
    pub slippage_bps: Option<u32>,
    /// Jupiter fee in basis points.
    pub fee_bps: Option<u32>,
    pub transaction: Option<String>,
    pub error: Option<String>,
    pub error_message: Option<String>,
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

/// Get a swap quote from Jupiter Ultra API.
pub async fn get_order(
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
) -> Result<OrderResponse> {
    let response = HTTP
        .get(format!("{ULTRA_API_BASE}/order"))
        .query(&[
            ("inputMint", input_mint),
            ("outputMint", output_mint),
            ("amount", amount),
            ("taker", taker),
        ])
        .send()
        .await?;

    let status = response.status();
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

    Ok(resp)
}

/// Sign and execute a swap via Jupiter Ultra API.
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
        .post(format!("{ULTRA_API_BASE}/execute"))
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
    let f: f64 = amount.parse().map_err(|_| eyre!("Invalid amount: {amount}"))?;
    if f <= 0.0 {
        return Err(eyre!("Amount must be positive"));
    }
    let d = decimals as usize;
    let parts: Vec<&str> = amount.split('.').collect();
    let (integer, frac) = match parts.len() {
        1 => (parts[0], ""),
        2 => (parts[0], parts[1]),
        _ => return Err(eyre!("Invalid amount format")),
    };
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
        // 7 decimal places, but USDC has 6 — truncate to 6
        assert_eq!(usdc_to_raw("1.1234567").unwrap(), "1123456");
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
}
