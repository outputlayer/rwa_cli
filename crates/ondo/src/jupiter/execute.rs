//! `/execute` flow: sign and submit a quoted order.
//!
//! Public entry is `execute_order`. Dispatches by `OrderResponse.backend`:
//! managed Jupiter backends use the `/execute` endpoint; the Metis V1 path
//! submits the signed transaction directly via Solana RPC.

use eyre::{eyre, Result, WrapErr};
use serde::Serialize;

/// Map a non-success `/execute` HTTP status to a failure kind. 429 and 5xx are
/// transient (retryable via a fresh order); everything else is a hard error.
fn execute_http_error_kind(status: reqwest::StatusCode) -> ExecuteFailureKind {
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        ExecuteFailureKind::Unavailable
    } else {
        ExecuteFailureKind::Unknown
    }
}

use crate::solana;
use crate::wallet::{ExpectedSwap, Wallet};
use crate::HTTP;

use super::{
    types::{ExecuteFailure, ExecuteFailureKind, ExecuteResponse, OrderBackend, OrderResponse},
    with_jupiter_headers, EXECUTE_SEMAPHORE, SWAP_V2_LITE_API_BASE, ULTRA_API_BASE,
    ULTRA_LITE_API_BASE,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecuteRequest {
    request_id: String,
    signed_transaction: String,
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

    let response = match with_jupiter_headers(HTTP.post(format!("{base_url}/execute")))
        .json(&req)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return Err(ExecuteFailure {
                kind: ExecuteFailureKind::Unavailable,
                code: None,
                message: format!("Jupiter /execute request failed: {e}"),
            }
            .into());
        }
    };

    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    if !status.is_success() {
        return Err(ExecuteFailure {
            kind: execute_http_error_kind(status),
            code: None,
            message: format!("Jupiter execute HTTP {status}: {body}"),
        }
        .into());
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
        return Err(ExecuteFailure {
            kind: ExecuteFailureKind::Unknown,
            code: None,
            message: format!("Swap failed: {msg}"),
        }
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_http_error_kind_maps_transient_vs_hard() {
        use reqwest::StatusCode;
        assert_eq!(execute_http_error_kind(StatusCode::TOO_MANY_REQUESTS), ExecuteFailureKind::Unavailable);
        assert_eq!(execute_http_error_kind(StatusCode::INTERNAL_SERVER_ERROR), ExecuteFailureKind::Unavailable);
        assert_eq!(execute_http_error_kind(StatusCode::BAD_GATEWAY), ExecuteFailureKind::Unavailable);
        assert_eq!(execute_http_error_kind(StatusCode::BAD_REQUEST), ExecuteFailureKind::Unknown);
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
}
