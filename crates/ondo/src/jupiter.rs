use eyre::{eyre, Result, WrapErr};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::solana;
use crate::wallet::{ExpectedSwap, Wallet};
use crate::HTTP;

const SWAP_V2_LITE_API_BASE: &str = "https://lite-api.jup.ag/swap/v2";
const ULTRA_API_BASE: &str = "https://ultra-api.jup.ag";
const ULTRA_LITE_API_BASE: &str = "https://lite-api.jup.ag/ultra/v1";
const METIS_LITE_API_BASE: &str = "https://lite-api.jup.ag/swap/v1";
const JUPITER_CLIENT_PLATFORM: &str = "jupiter.cli";

/// Concurrency limit for `/order` (quote) requests.
/// Jupiter routes orders per-wallet; too many concurrent `/order` calls from the
/// same taker wallet cause rejections (not rate-limit 429, but routing conflicts).
/// Tested: 4 concurrent fails, 2 concurrent reliable across 20-token baskets.
static ORDER_SEMAPHORE: Semaphore = Semaphore::const_new(2);

/// Concurrency limit for `/execute` (signed-tx submission) requests.
/// Execute uses pre-cached orders so wallet contention is lower.
static EXECUTE_SEMAPHORE: Semaphore = Semaphore::const_new(5);
pub use crate::USDC_MINT;
/// Wrapped SOL on Solana
pub const SOL_MINT: &str = "So11111111111111111111111111111111111111112";
pub const USDC_DECIMALS: u8 = 6;
/// Ondo GM tokens on Solana use 9 decimals (Solana standard).
pub const GM_SOL_DECIMALS: u8 = 9;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrderBackend {
    #[default]
    SwapV2Lite,
    Ultra,
    UltraLite,
    MetisV1Lite,
}

impl OrderBackend {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::SwapV2Lite => "swap-v2-lite",
            Self::Ultra => "ultra",
            Self::UltraLite => "ultra-lite",
            Self::MetisV1Lite => "metis-v1-lite",
        }
    }
}

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
    #[serde(skip)]
    pub backend: OrderBackend,
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

/// Get a swap quote from public Jupiter APIs.
/// Flow: /order -> wallet.sign -> /execute.
/// Maximum retries for transient errors on /order.
const ORDER_MAX_RETRIES: u32 = 4;

pub async fn get_order(
    base_url: Option<&str>,
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
) -> Result<OrderResponse> {
    if let Some(base_url) = base_url {
        let backend = infer_backend_from_base_url(base_url);
        return get_order_with_retries(
            base_url,
            backend,
            input_mint,
            output_mint,
            amount,
            taker,
            slippage_bps,
        )
        .await;
    }

    let mut failures = Vec::new();
    let backends = [
        (SWAP_V2_LITE_API_BASE, OrderBackend::SwapV2Lite),
        (ULTRA_API_BASE, OrderBackend::Ultra),
        (ULTRA_LITE_API_BASE, OrderBackend::UltraLite),
    ];

    for (base_url, backend) in backends {
        match get_order_with_retries(
            base_url,
            backend,
            input_mint,
            output_mint,
            amount,
            taker,
            slippage_bps,
        )
        .await
        {
            Ok(order) => return Ok(order),
            Err(err) => {
                let msg = err.to_string();
                failures.push(format!("{}: {msg}", backend.label()));
                if !is_route_like_order_error(&msg) {
                    return Err(eyre!(
                        "Jupiter quote failed via {}: {msg}",
                        backend.label()
                    ));
                }
            }
        }
    }

    match get_metis_order(input_mint, output_mint, amount, taker, slippage_bps).await {
        Ok(order) => Ok(order),
        Err(err) => {
            failures.push(format!("{}: {}", OrderBackend::MetisV1Lite.label(), err));
            Err(eyre!(
                "No swap route found across public Jupiter backends: {}",
                failures.join(" | ")
            ))
        }
    }
}

async fn get_order_with_retries(
    base_url: &str,
    backend: OrderBackend,
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
) -> Result<OrderResponse> {
    let order_url = format!("{base_url}/order");
    let mut last_err = eyre!("Jupiter /order failed");

    for attempt in 0..=ORDER_MAX_RETRIES {
        // Acquire semaphore only for the actual HTTP call, release before any sleep.
        let result = {
            let _permit = ORDER_SEMAPHORE.acquire().await
                .map_err(|_| eyre!("Jupiter order semaphore closed"))?;
            get_order_inner(
                &order_url,
                backend,
                input_mint,
                output_mint,
                amount,
                taker,
                slippage_bps,
            )
            .await
        }; // permit released here

        match result {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                let msg = format!("{e}");
                if attempt < ORDER_MAX_RETRIES && is_retryable_order_error(&msg) {
                    let backoff = std::time::Duration::from_millis(800 * 2u64.pow(attempt));
                    tokio::time::sleep(backoff).await;
                    last_err = e;
                    continue;
                }
                return Err(e);
            }
        }
    }

    Err(last_err)
}

/// Single /order attempt — no retries, no semaphore.
async fn get_order_inner(
    order_url: &str,
    backend: OrderBackend,
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
) -> Result<OrderResponse> {
    let mut request = HTTP.get(order_url).query(&[
        ("inputMint", input_mint),
        ("outputMint", output_mint),
        ("amount", amount),
        ("taker", taker),
    ]);

    request = with_jupiter_headers(request);

    if let Some(bps) = slippage_bps {
        request = request.query(&[("slippageBps", bps.to_string())]);
    }

    let response = request.send().await
        .map_err(|e| eyre!("Jupiter /order network error: {e}"))?;

    let status = response.status();
    let body = response.text().await?;

    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(eyre!("Jupiter /order rate limited (429)"));
    }
    if status.is_server_error() {
        return Err(eyre!("Jupiter /order server error ({status})"));
    }
    if !status.is_success() {
        if status.as_u16() == 400 && body.contains("Failed to get quotes") {
            return Err(eyre!(
                "No swap route found — {}",
                compact_error_body(&body)
            ));
        }
        return Err(eyre!("Jupiter API error (HTTP {status}): {body}"));
    }

    let mut resp: OrderResponse = serde_json::from_str(&body)
        .map_err(|e| eyre!("Failed to parse Jupiter response: {e}\nBody: {body}"))?;
    resp.backend = backend;

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

    Ok(resp)
}

/// Errors from Jupiter /order that should be retried with backoff.
/// Covers transient MM unavailability, rate limits, server errors, and
/// network issues.
fn is_retryable_order_error(msg: &str) -> bool {
    msg.contains("not available from market maker")
        || msg.contains("Something unexpected occurred")
        || msg.contains("Winning quote has no transaction")
        || msg.contains("/order network error")
        || msg.contains("/order rate limited")
        || msg.contains("/order server error")
}

fn is_route_like_order_error(msg: &str) -> bool {
    msg.contains("No swap route found")
        || msg.contains("Failed to get quotes")
        || msg.contains("COULD_NOT_FIND_ANY_ROUTE")
        || msg.contains("TOKEN_NOT_TRADABLE")
}

fn compact_error_body(body: &str) -> String {
    let trimmed = body.trim().replace('\n', " ");
    let compact = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.len() > 200 {
        format!("{}...", &compact[..200])
    } else {
        compact
    }
}

fn infer_backend_from_base_url(base_url: &str) -> OrderBackend {
    if base_url.contains("/swap/v2") {
        OrderBackend::SwapV2Lite
    } else if base_url.contains("/swap/v1") {
        OrderBackend::MetisV1Lite
    } else if base_url.contains("lite-api.jup.ag/ultra/v1") {
        OrderBackend::UltraLite
    } else {
        OrderBackend::Ultra
    }
}

fn with_jupiter_headers(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    request.header("x-client-platform", JUPITER_CLIENT_PLATFORM)
}

async fn get_metis_order(
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
) -> Result<OrderResponse> {
    let quote_url = format!("{METIS_LITE_API_BASE}/quote");
    let mut request = HTTP.get(&quote_url).query(&[
        ("inputMint", input_mint),
        ("outputMint", output_mint),
        ("amount", amount),
        ("swapMode", "ExactIn"),
        ("restrictIntermediateTokens", "true"),
        ("instructionVersion", "V2"),
    ]);
    if let Some(bps) = slippage_bps {
        request = request.query(&[("slippageBps", bps.to_string())]);
    }

    let quote_response = request
        .send()
        .await
        .map_err(|e| eyre!("Metis /quote network error: {e}"))?;
    let quote_status = quote_response.status();
    let quote_body = quote_response.text().await?;
    if !quote_status.is_success() {
        return Err(eyre!(
            "Metis quote error (HTTP {quote_status}): {}",
            compact_error_body(&quote_body)
        ));
    }

    let quote_json: serde_json::Value = serde_json::from_str(&quote_body)
        .map_err(|e| eyre!("Failed to parse Metis quote response: {e}\nBody: {quote_body}"))?;
    let in_amount = quote_json["inAmount"]
        .as_str()
        .ok_or_else(|| eyre!("Metis quote missing inAmount"))?
        .to_string();
    let out_amount = quote_json["outAmount"]
        .as_str()
        .ok_or_else(|| eyre!("Metis quote missing outAmount"))?
        .to_string();
    let price_impact = quote_json["priceImpactPct"]
        .as_str()
        .and_then(|value| value.parse::<f64>().ok());
    let slippage_bps = quote_json["slippageBps"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .or(slippage_bps);

    let swap_url = format!("{METIS_LITE_API_BASE}/swap");
    let swap_body = serde_json::json!({
        "userPublicKey": taker,
        "quoteResponse": quote_json,
        "dynamicComputeUnitLimit": true,
        "prioritizationFeeLamports": {
            "priorityLevelWithMaxLamports": {
                "priorityLevel": "veryHigh",
                "maxLamports": 1_000_000u64
            }
        }
    });
    let swap_response = HTTP
        .post(&swap_url)
        .json(&swap_body)
        .send()
        .await
        .map_err(|e| eyre!("Metis /swap network error: {e}"))?;
    let swap_status = swap_response.status();
    let swap_body = swap_response.text().await?;
    if !swap_status.is_success() {
        return Err(eyre!(
            "Metis swap build error (HTTP {swap_status}): {}",
            compact_error_body(&swap_body)
        ));
    }

    let swap_json: serde_json::Value = serde_json::from_str(&swap_body)
        .map_err(|e| eyre!("Failed to parse Metis swap response: {e}\nBody: {swap_body}"))?;
    let transaction = swap_json["swapTransaction"]
        .as_str()
        .ok_or_else(|| eyre!("Metis swap response missing swapTransaction"))?
        .to_string();
    let last_valid_block_height = swap_json["lastValidBlockHeight"]
        .as_u64()
        .map(|value| value.to_string());

    Ok(OrderResponse {
        request_id: format!("metis:{}:{}:{}", input_mint, output_mint, amount),
        in_amount,
        out_amount,
        in_usd_value: None,
        out_usd_value: None,
        price_impact,
        price_impact_pct: quote_json["priceImpactPct"].as_str().map(str::to_string),
        slippage_bps,
        fee_bps: None,
        transaction: Some(transaction),
        error: None,
        error_message: None,
        gasless: Some(false),
        router: Some("metis-v1-lite".to_string()),
        rent_fee_lamports: None,
        rent_fee_payer: None,
        signature_fee_lamports: None,
        signature_fee_payer: None,
        prioritization_fee_lamports: swap_json["prioritizationFeeLamports"].as_u64(),
        prioritization_fee_payer: None,
        mode: Some("manual".to_string()),
        last_valid_block_height,
        backend: OrderBackend::MetisV1Lite,
    })
}

/// Sign and execute a swap via the backend that produced the order.
///
/// `expected` describes what the caller intends to swap; before signing, the
/// wallet decodes the Jupiter-supplied transaction and refuses to sign if it
/// doesn't match the intent (wrong mint, wrong amount, foreign recipient).
pub async fn execute_order(
    wallet: &Wallet,
    order: &OrderResponse,
    expected: &ExpectedSwap,
) -> Result<ExecuteResponse> {
    let _permit = EXECUTE_SEMAPHORE.acquire().await
        .map_err(|_| eyre!("Jupiter execute semaphore closed"))?;
    match order.backend {
        OrderBackend::SwapV2Lite => execute_managed_order(wallet, order, expected, SWAP_V2_LITE_API_BASE).await,
        OrderBackend::Ultra => execute_managed_order(wallet, order, expected, ULTRA_API_BASE).await,
        OrderBackend::UltraLite => execute_managed_order(wallet, order, expected, ULTRA_LITE_API_BASE).await,
        OrderBackend::MetisV1Lite => execute_metis_order(wallet, order, expected).await,
    }
}

async fn execute_managed_order(
    wallet: &Wallet,
    order: &OrderResponse,
    expected: &ExpectedSwap,
    base_url: &str,
) -> Result<ExecuteResponse> {
    let tx_b64 = order
        .transaction
        .as_deref()
        .ok_or_else(|| eyre!("No transaction in order"))?;
    let signed_tx = wallet.sign_jupiter_swap(tx_b64, expected)
        .wrap_err("failed to sign swap transaction")?;

    let req = ExecuteRequest {
        request_id: order.request_id.clone(),
        signed_transaction: signed_tx,
    };

    let response = with_jupiter_headers(HTTP.post(format!("{base_url}/execute")))
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

async fn execute_metis_order(
    wallet: &Wallet,
    order: &OrderResponse,
    expected: &ExpectedSwap,
) -> Result<ExecuteResponse> {
    let tx_b64 = order
        .transaction
        .as_deref()
        .ok_or_else(|| eyre!("No transaction in order"))?;
    let signed_tx = wallet
        .sign_jupiter_swap(tx_b64, expected)
        .wrap_err("failed to sign Metis swap transaction")?;
    let tx = solana::send_signed_transaction(&signed_tx, None).await?;
    Ok(ExecuteResponse {
        status: Some("Success".to_string()),
        signature: Some(tx.signature),
        code: None,
        error: None,
        error_message: None,
        input_amount_result: None,
        output_amount_result: None,
    })
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

    #[test]
    fn infer_backend_from_base_url_matches_public_paths() {
        assert_eq!(
            infer_backend_from_base_url("https://lite-api.jup.ag/swap/v2"),
            OrderBackend::SwapV2Lite
        );
        assert_eq!(
            infer_backend_from_base_url("https://ultra-api.jup.ag"),
            OrderBackend::Ultra
        );
        assert_eq!(
            infer_backend_from_base_url("https://lite-api.jup.ag/ultra/v1"),
            OrderBackend::UltraLite
        );
        assert_eq!(
            infer_backend_from_base_url("https://lite-api.jup.ag/swap/v1"),
            OrderBackend::MetisV1Lite
        );
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

