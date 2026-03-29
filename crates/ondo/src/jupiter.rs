use eyre::{eyre, Result};
use serde::{Deserialize, Serialize};

use crate::amounts;
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

        let mut request = HTTP.get(format!("{SWAP_API_BASE}/order")).query(&[
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
            let detail = resp
                .error_message
                .as_deref()
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
pub async fn execute_order(wallet: &Wallet, order: &OrderResponse) -> Result<ExecuteResponse> {
    let tx_b64 = order
        .transaction
        .as_deref()
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
            let raw_msg = resp
                .error_message
                .as_deref()
                .or(resp.error.as_deref())
                .unwrap_or("Unknown execution error");
            let hint = resp
                .code
                .map(|c| match c {
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
                })
                .unwrap_or("");
            let code = resp
                .code
                .map(|c| format!(" (code {c})"))
                .unwrap_or_default();
            return Err(eyre!("Swap failed{code}: {raw_msg}{hint}"));
        }
    }

    if resp.signature.is_none() {
        let msg = resp
            .error_message
            .as_deref()
            .or(resp.error.as_deref())
            .unwrap_or("No signature returned — transaction may have failed");
        return Err(eyre!("Swap failed: {msg}"));
    }
    Ok(())
}

/// Convert a human-readable USDC amount (e.g. "100.50") to on-chain units (6 decimals).
pub fn usdc_to_raw(amount: &str) -> Result<String> {
    amounts::token_to_raw(amount, USDC_DECIMALS)
}

/// Convert a human-readable token amount to on-chain units with specified decimals.
pub fn token_to_raw(amount: &str, decimals: u8) -> Result<String> {
    amounts::token_to_raw(amount, decimals)
}

/// Format on-chain amount to human-readable with given decimals.
#[must_use]
pub fn format_amount(raw: &str, decimals: u8) -> String {
    amounts::format_amount(raw, decimals)
}
