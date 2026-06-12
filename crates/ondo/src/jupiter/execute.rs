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

/// Pre-sign economic gate shared by every execute path: simulate the exact
/// Jupiter transaction and confirm the real balance deltas before signing —
/// debit ≤ expected input, expected output mint credited at least the quoted
/// amount minus slippage tolerance (an RFQ MM or a stale route can otherwise
/// bake a far worse fill into the tx than its own quote claimed).
async fn presign_simulation_gate(
    tx_b64: &str,
    order: &OrderResponse,
    expected: &ExpectedSwap,
) -> Result<()> {
    let min_output = solana::min_output_floor(
        order.out_amount.parse().unwrap_or(0),
        order.slippage_bps,
    );
    if let Err(e) = solana::verify_swap_simulation(
        tx_b64,
        &expected.input_mint,
        expected.input_amount,
        &expected.output_mint,
        min_output,
        &expected.owner_pubkey,
        None,
    )
    .await
    {
        // An unfillable route (RFQ MM can't fill) or a dishonest fill (credits
        // materially less than quoted) is retryable with that router excluded;
        // an unsafe delta or unreachable RPC is a hard, non-retryable refusal.
        // Map accordingly so `execute_with_retry` can route around it.
        let kind = match e.downcast_ref::<solana::SwapSimError>() {
            Some(solana::SwapSimError::OnChainWouldFail(_))
            | Some(solana::SwapSimError::OutputBelowQuote(_)) => {
                ExecuteFailureKind::RouteUnfillable
            }
            _ => ExecuteFailureKind::Unknown,
        };
        return Err(ExecuteFailure {
            kind,
            code: None,
            message: format!("pre-sign swap simulation check failed: {e}"),
        }
        .into());
    }
    Ok(())
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

    presign_simulation_gate(tx_b64, order, expected).await?;

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
            // Only a connection-phase failure proves Jupiter never received the
            // request — safe to retry. A timeout is ambiguous (Jupiter may have
            // already received and executed the swap), so do NOT retry it:
            // retrying with a fresh order could double-submit the trade.
            let kind = if e.is_connect() {
                ExecuteFailureKind::Unavailable
            } else {
                ExecuteFailureKind::Unknown
            };
            return Err(ExecuteFailure {
                kind,
                code: None,
                message: format!("Jupiter /execute request failed: {e}"),
            }
            .into());
        }
    };

    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|e| format!("<body read error: {e}>"));

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
    // Same economic gate as managed routes: Metis v1 carries minOut on-chain,
    // but defense-in-depth says verify the simulated deltas before signing too.
    presign_simulation_gate(tx_b64, order, expected).await?;
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
