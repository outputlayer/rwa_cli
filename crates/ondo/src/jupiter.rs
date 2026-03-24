use eyre::{Result, eyre};
use serde::{Deserialize, Serialize};

use crate::wallet::Wallet;

const ULTRA_API_BASE: &str = "https://lite-api.jup.ag/ultra/v1";
/// USDC on Solana
pub const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
pub const USDC_DECIMALS: u8 = 6;
/// Ondo GM tokens on Solana use 9 decimals (Solana standard).
pub const GM_SOL_DECIMALS: u8 = 9;

// ── API types ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderResponse {
    pub request_id: String,
    pub in_amount: String,
    pub out_amount: String,
    pub in_usd_value: Option<f64>,
    pub out_usd_value: Option<f64>,
    pub price_impact: Option<f64>,
    pub transaction: String,
    // Error fields
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

// ── Public functions ───────────────────────────────────────

/// Get a swap quote from Jupiter Ultra API.
pub async fn get_order(
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
) -> Result<OrderResponse> {
    let url = format!(
        "{ULTRA_API_BASE}/order?inputMint={input_mint}&outputMint={output_mint}\
         &amount={amount}&taker={taker}"
    );

    let resp: OrderResponse = reqwest::Client::new()
        .get(&url)
        .header("x-client-platform", "rwa.cli")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await?
        .json()
        .await?;

    if let Some(err) = &resp.error {
        let msg = resp.error_message.as_deref().unwrap_or(err);
        return Err(eyre!("Jupiter API error: {msg}"));
    }

    if resp.transaction.is_empty() {
        return Err(eyre!("Jupiter returned empty transaction — route may not exist"));
    }

    Ok(resp)
}

/// Sign and execute a swap via Jupiter Ultra API.
pub async fn execute_order(
    wallet: &Wallet,
    order: &OrderResponse,
) -> Result<ExecuteResponse> {
    let signed_tx = wallet.sign_transaction(&order.transaction)?;

    let req = ExecuteRequest {
        request_id: order.request_id.clone(),
        signed_transaction: signed_tx,
    };

    let resp: ExecuteResponse = reqwest::Client::new()
        .post(format!("{ULTRA_API_BASE}/execute"))
        .header("x-client-platform", "rwa.cli")
        .json(&req)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await?
        .json()
        .await?;

    // Check for failure
    if let Some(status) = &resp.status {
        if status == "Failed" {
            let msg = resp.error_message.as_deref()
                .or(resp.error.as_deref())
                .unwrap_or("Unknown execution error");
            let code = resp.code.map(|c| format!(" (code {c})")).unwrap_or_default();
            return Err(eyre!("Swap failed{code}: {msg}"));
        }
    }

    if resp.signature.is_none() {
        let msg = resp.error_message.as_deref()
            .or(resp.error.as_deref())
            .unwrap_or("No signature returned — transaction may have failed");
        return Err(eyre!("Swap failed: {msg}"));
    }

    Ok(resp)
}

/// Convert a human-readable USDC amount (e.g. "100.50") to on-chain units (6 decimals).
pub fn usdc_to_raw(amount: &str) -> Result<String> {
    let f: f64 = amount.parse().map_err(|_| eyre!("Invalid amount: {amount}"))?;
    if f <= 0.0 {
        return Err(eyre!("Amount must be positive"));
    }
    let raw = (f * 1_000_000.0) as u64;
    Ok(raw.to_string())
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
