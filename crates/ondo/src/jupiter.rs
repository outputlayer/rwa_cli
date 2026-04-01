use eyre::{eyre, Result, WrapErr};
use serde::{Deserialize, Serialize};

use crate::wallet::Wallet;
use crate::HTTP;

const SWAP_API_BASE: &str = "https://ultra-api.jup.ag";
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

#[derive(Debug, Default, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecuteRetryAction {
    None,
    RetrySameOrder,
    RefreshOrder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecuteFailureKind {
    MissingCachedOrder,
    InvalidSignedTransaction,
    InvalidMessageBytes,
    FailedToLand,
    UnknownAggregatorError,
    RfqFailedToLand,
    UnknownRfqError,
    InvalidPayload,
    QuoteExpired,
    SwapRejected,
    InternalError,
    Unknown,
}

impl ExecuteFailureKind {
    #[must_use]
    pub fn from_code(code: Option<i32>) -> Self {
        match code {
            Some(-1) => Self::MissingCachedOrder,
            Some(-2) => Self::InvalidSignedTransaction,
            Some(-3) => Self::InvalidMessageBytes,
            Some(-1000) => Self::FailedToLand,
            Some(-1001) => Self::UnknownAggregatorError,
            Some(-2000) => Self::RfqFailedToLand,
            Some(-2001) => Self::UnknownRfqError,
            Some(-2002) => Self::InvalidPayload,
            Some(-2003) => Self::QuoteExpired,
            Some(-2004) => Self::SwapRejected,
            Some(-2005) => Self::InternalError,
            _ => Self::Unknown,
        }
    }

    #[must_use]
    pub fn retry_action(self) -> ExecuteRetryAction {
        match self {
            Self::MissingCachedOrder | Self::QuoteExpired | Self::SwapRejected | Self::InternalError => ExecuteRetryAction::RefreshOrder,
            Self::FailedToLand | Self::RfqFailedToLand => ExecuteRetryAction::RetrySameOrder,
            Self::InvalidSignedTransaction
            | Self::InvalidMessageBytes
            | Self::UnknownAggregatorError
            | Self::UnknownRfqError
            | Self::InvalidPayload
            | Self::Unknown => ExecuteRetryAction::None,
        }
    }

    #[must_use]
    pub fn hint(self) -> &'static str {
        match self {
            Self::MissingCachedOrder => "missing cached order — retry",
            Self::InvalidSignedTransaction => "invalid signed transaction",
            Self::InvalidMessageBytes => "invalid message bytes",
            Self::FailedToLand => "failed to land — retry",
            Self::UnknownAggregatorError => "unknown aggregator error",
            Self::RfqFailedToLand => "RFQ failed to land — retry",
            Self::UnknownRfqError => "unknown RFQ error",
            Self::InvalidPayload => "invalid payload",
            Self::QuoteExpired => "quote expired — retry",
            Self::SwapRejected => "swap rejected",
            Self::InternalError => "internal error — retry",
            Self::Unknown => "",
        }
    }
}

#[derive(Debug)]
pub struct ExecuteFailure {
    pub kind: ExecuteFailureKind,
    pub code: Option<i32>,
    pub message: String,
}

impl std::fmt::Display for ExecuteFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let code = self
            .code
            .map(|c| format!(" (code {c})"))
            .unwrap_or_default();
        let hint = self.kind.hint();
        if hint.is_empty() {
            write!(f, "Swap failed{code}: {}", self.message)
        } else {
            write!(f, "Swap failed{code}: {} ({hint})", self.message)
        }
    }
}

impl std::error::Error for ExecuteFailure {}

// ── Public functions ───────────────────────────────────────────────────

/// Get a swap quote from Jupiter Ultra API.
/// Uses ultra-api.jup.ag — no API key required, supports GM tokens.
/// Flow: /order → wallet.sign → /execute
/// Maximum retries for transient network errors on /order.
const ORDER_MAX_RETRIES: u32 = 2;

pub async fn get_order(
    base_url: Option<&str>,
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
) -> Result<OrderResponse> {
    let mut last_err = eyre!("Jupiter /order failed");
    let order_url = format!("{}/order", base_url.unwrap_or(SWAP_API_BASE));

    for attempt in 0..=ORDER_MAX_RETRIES {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }

        let mut request = HTTP.get(&order_url).query(&[
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
    let signed_tx = wallet.sign_transaction(tx_b64)
        .wrap_err("failed to sign swap transaction")?;

    let req = ExecuteRequest {
        request_id: order.request_id.clone(),
        signed_transaction: signed_tx,
    };

    let response = HTTP
        .post(format!("{SWAP_API_BASE}/execute"))
        .json(&req)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
        .wrap_err("Jupiter /execute request failed")?;

    let status = response.status();
    let body = response.text().await
        .wrap_err("failed to read Jupiter /execute response")?;

    if !status.is_success() {
        return Err(eyre!("Jupiter execute error (HTTP {status}): {body}"));
    }

    let resp: ExecuteResponse = serde_json::from_str(&body)
        .map_err(|e| eyre!("Failed to parse Jupiter execute response: {e}\nBody: {body}"))?;

    check_execute_result(&resp)?;
    Ok(resp)
}

fn check_execute_result(resp: &ExecuteResponse) -> Result<()> {
    if let Some(status) = &resp.status
        && status == "Failed"
    {
        let message = resp
            .error_message
            .as_deref()
            .or(resp.error.as_deref())
            .unwrap_or("Unknown execution error");
        return Err(ExecuteFailure {
            kind: ExecuteFailureKind::from_code(resp.code),
            code: resp.code,
            message: message.to_string(),
        }
        .into());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_failure_kind_maps_codes_to_retry_actions() {
        assert_eq!(
            ExecuteFailureKind::from_code(Some(-2003)).retry_action(),
            ExecuteRetryAction::RefreshOrder
        );
        assert_eq!(
            ExecuteFailureKind::from_code(Some(-1000)).retry_action(),
            ExecuteRetryAction::RetrySameOrder
        );
        assert_eq!(
            ExecuteFailureKind::from_code(Some(-2)).retry_action(),
            ExecuteRetryAction::None
        );
    }

    #[test]
    fn failed_execute_response_preserves_structured_kind() {
        let resp = ExecuteResponse {
            status: Some("Failed".to_string()),
            signature: None,
            code: Some(-2003),
            error: Some("Quote expired".to_string()),
            error_message: None,
            input_amount_result: None,
            output_amount_result: None,
        };
        let err = check_execute_result(&resp).unwrap_err();
        let failure = err.downcast_ref::<ExecuteFailure>().expect("structured execute failure");
        assert_eq!(failure.kind, ExecuteFailureKind::QuoteExpired);
        assert_eq!(failure.code, Some(-2003));
    }

    #[test]
    fn execute_failure_display_includes_hint() {
        let failure = ExecuteFailure {
            kind: ExecuteFailureKind::FailedToLand,
            code: Some(-1000),
            message: "landing failure".to_string(),
        };
        let msg = failure.to_string();
        assert!(msg.contains("code -1000"));
        assert!(msg.contains("failed to land"));
    }

    // ── get_order httpmock ────────────────────────────────────

    #[tokio::test]
    async fn get_order_returns_parsed_response_on_success() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        server.mock_async(|when, then| {
            when.method(GET).path("/order");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({
                    "requestId": "test-req-1",
                    "inAmount": "1000000",
                    "outAmount": "500000000",
                    "transaction": "AQAAAA==",
                    "gasless": true,
                    "router": "jupiterz"
                }));
        }).await;

        let order = get_order(
            Some(&server.base_url()),
            USDC_MINT,
            "So11111111111111111111111111111112",
            "1000000",
            "FakeWallet111111111111111111111111111111",
            Some(100),
        ).await.unwrap();

        assert_eq!(order.in_amount, "1000000");
        assert_eq!(order.out_amount, "500000000");
        assert_eq!(order.transaction, Some("AQAAAA==".to_string()));
        assert_eq!(order.gasless, Some(true));
        assert_eq!(order.router, Some("jupiterz".to_string()));
    }

    #[tokio::test]
    async fn get_order_returns_err_when_response_has_error_field() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        server.mock_async(|when, then| {
            when.method(GET).path("/order");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({
                    "requestId": "test-req-2",
                    "inAmount": "1000000",
                    "outAmount": "0",
                    "error": "QUOTE_ERROR",
                    "errorMessage": "no route found"
                }));
        }).await;

        let result = get_order(
            Some(&server.base_url()),
            USDC_MINT,
            "So11111111111111111111111111111112",
            "1000000",
            "FakeWallet111111111111111111111111111111",
            None,
        ).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no route found"));
    }

    #[tokio::test]
    async fn get_order_returns_err_on_empty_transaction() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        server.mock_async(|when, then| {
            when.method(GET).path("/order");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({
                    "requestId": "test-req-3",
                    "inAmount": "1000000",
                    "outAmount": "500000000",
                    "transaction": ""
                }));
        }).await;

        let result = get_order(
            Some(&server.base_url()),
            USDC_MINT,
            "So11111111111111111111111111111112",
            "1000000",
            "FakeWallet111111111111111111111111111111",
            None,
        ).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty transaction"));
    }

    #[tokio::test]
    async fn get_order_returns_err_on_http_400_failed_quotes() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        server.mock_async(|when, then| {
            when.method(GET).path("/order");
            then.status(400)
                .body("Failed to get quotes for the given input");
        }).await;

        let result = get_order(
            Some(&server.base_url()),
            USDC_MINT,
            "So11111111111111111111111111111112",
            "1000000",
            "FakeWallet111111111111111111111111111111",
            None,
        ).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No swap route found"));
    }
}

