//! `/execute` flow: sign and submit a quoted order.
//!
//! Public entry is `execute_order`. Dispatches by `OrderResponse.backend`:
//! managed Jupiter backends use the `/execute` endpoint; the Metis V1 path
//! submits the signed transaction directly via Solana RPC.

use eyre::{eyre, Result, WrapErr};
use serde::Serialize;

/// Map a non-success `/execute` HTTP status to a failure kind.
///
/// Only statuses that PROVE Jupiter did not execute the swap are retryable
/// (fresh-order retry): 429 (rejected before processing) and 503 (service
/// refused). Gateway statuses — 502 Bad Gateway, 504 Gateway Timeout — mean a
/// proxy gave up while Jupiter may have already received and EXECUTED the
/// request; retrying with a fresh order could double-submit the trade, so
/// they are hard errors, same as the ambiguous-timeout rule above. 500 is
/// treated the same: "internal error" says nothing about whether execution
/// happened.
fn execute_http_error_kind(status: reqwest::StatusCode) -> ExecuteFailureKind {
    use reqwest::StatusCode;
    if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE {
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
    with_jupiter_headers, EXECUTE_SEMAPHORE,
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
/// `max_slippage_bps` is the user's requested slippage (`--slippage`/default),
/// used to clamp the pre-sign under-delivery floor so a hostile/dynamic
/// `/order` response can't widen it past what the user consented to.
pub async fn execute_order(
    wallet: &Wallet,
    order: &OrderResponse,
    expected: &ExpectedSwap,
    max_slippage_bps: Option<u32>,
) -> Result<ExecuteResponse> {
    let _permit = EXECUTE_SEMAPHORE.acquire().await
        .map_err(|_| eyre!("Jupiter execute semaphore closed"))?;
    match order.backend {
        OrderBackend::MetisV1Lite => execute_metis_order(wallet, order, expected, max_slippage_bps).await,
        managed => execute_managed_order(wallet, order, expected, managed.base_url(), max_slippage_bps).await,
    }
}

/// Pre-sign economic gate shared by every execute path: simulate the exact
/// Jupiter transaction and confirm the real balance deltas before signing —
/// debit ≤ expected input, expected output mint credited at least the quoted
/// amount minus slippage tolerance (an RFQ MM or a stale route can otherwise
/// bake a far worse fill into the tx than its own quote claimed).
/// Parse the quoted output amount. An unparseable quote must refuse to sign
/// (fail closed), never silently disable the under-delivery floor with 0.
fn quoted_out_amount(order: &OrderResponse) -> Result<u64, ExecuteFailure> {
    order.out_amount.parse().map_err(|_| ExecuteFailure {
        kind: ExecuteFailureKind::Unknown,
        code: None,
        message: format!(
            "order has unparseable outAmount '{}' — refusing to sign",
            order.out_amount
        ),
    })
}

/// A route that funds the fill just-in-time — an RFQ market maker mints/positions
/// inventory at `/execute` (same-block), which a pre-execute simulation can't
/// see. Detected via `gasless`: RFQ makers sponsor gas; AMM routes (Metis) don't.
fn route_funds_just_in_time(order: &OrderResponse) -> bool {
    order.gasless == Some(true)
}

/// Whether the pre-sign gate may relax on an errored simulation for this order.
/// ONLY a managed `/execute` route may — there Jupiter co-signs and
/// server-validates, and the maker funds a gasless RFQ fill just-in-time. The
/// Metis route direct-submits via RPC with no server backstop, so it is ALWAYS
/// strict regardless of `gasless`. Keying off `order.backend` (rather than a
/// per-call-site bool) keeps this money-safety decision in ONE tested place: a
/// Metis route that failed simulation can never be signed.
fn allow_jit_fill_for(order: &OrderResponse) -> bool {
    !matches!(order.backend, OrderBackend::MetisV1Lite) && route_funds_just_in_time(order)
}

/// Whether a FAILED pre-sign simulation is nonetheless safe to submit. The ONLY
/// safe case is a simulation that merely ERRORED (`OnChainWouldFail`) on a route
/// that funds the fill just-in-time: the maker mints at `/execute` (invisible to
/// the pre-execute sim) and the on-chain min-output enforces honesty. A sim that
/// SUCCEEDED but showed dishonest deltas (`UnsafeDelta`/`OutputBelowQuote`) or
/// that could not run (`RpcUnavailable`) is NEVER submitted.
fn proceed_despite_sim_failure(err: &solana::SwapSimError, allow_jit_fill: bool) -> bool {
    allow_jit_fill && matches!(err, solana::SwapSimError::OnChainWouldFail(_))
}

/// The execute-failure kind to surface for a REFUSED simulation.
fn sim_failure_kind(err: &solana::SwapSimError) -> ExecuteFailureKind {
    match err {
        solana::SwapSimError::OnChainWouldFail(_)
        | solana::SwapSimError::OutputBelowQuote(_) => ExecuteFailureKind::RouteUnfillable,
        // A sim that couldn't reach the RPC is a transient network failure —
        // surface it as execute_unavailable (exit 75), not the permanent
        // Unknown bucket used for the UnsafeDelta security refusal.
        solana::SwapSimError::RpcUnavailable(_) => ExecuteFailureKind::Unavailable,
        solana::SwapSimError::UnsafeDelta(_) => ExecuteFailureKind::Unknown,
    }
}

/// Turn a pre-sign simulation result into a submit/refuse decision — the
/// safety-critical wiring, isolated so it can be tested without a network. `Ok`
/// stays `Ok`. A failure decodes to its cause and is submitted anyway ONLY when
/// safe (see [`proceed_despite_sim_failure`]); otherwise it is refused with the
/// mapped kind. A non-`SwapSimError` failure is refused (fail-closed).
fn interpret_sim_result(result: Result<()>, allow_jit_fill: bool) -> Result<()> {
    let Err(e) = result else {
        return Ok(());
    };
    // A SwapSimError decodes to a precise cause; anything else is an unexpected
    // failure and is refused (fail-closed).
    let Some(sim) = e.downcast_ref::<solana::SwapSimError>() else {
        return Err(ExecuteFailure {
            kind: ExecuteFailureKind::Unknown,
            code: None,
            message: format!("pre-sign swap simulation check failed: {e}"),
        }
        .into());
    };
    // A sim that merely ERRORED on a just-in-time-funding RFQ route is a false
    // negative — the maker mints the fill at /execute (invisible here) and the
    // on-chain min-output enforces honesty. Submit anyway.
    if proceed_despite_sim_failure(sim, allow_jit_fill) {
        eprintln!(
            "note: pre-sign simulation couldn't confirm the fill (RFQ maker funds \
             just-in-time); submitting — Jupiter /execute enforces min-output on-chain"
        );
        return Ok(());
    }
    // Dishonest deltas (UnsafeDelta/OutputBelowQuote), an unreachable RPC, or a
    // non-JIT route: a hard refusal. OnChainWouldFail/OutputBelowQuote are
    // retryable with the router excluded; the rest are not.
    Err(ExecuteFailure {
        kind: sim_failure_kind(sim),
        code: None,
        message: format!("pre-sign swap simulation check failed: {e}"),
    }
    .into())
}

async fn presign_simulation_gate(
    tx_b64: &str,
    order: &OrderResponse,
    expected: &ExpectedSwap,
    allow_jit_fill: bool,
    max_slippage_bps: Option<u32>,
) -> Result<()> {
    // Clamp the floor tolerance to the user's requested slippage: the order's
    // own `slippage_bps` is echoed by Jupiter and a hostile/dynamic response
    // could report a value large enough to slacken or disable this independent
    // under-delivery check.
    let tolerance = solana::floor_tolerance_bps(order.slippage_bps, max_slippage_bps);
    let min_output = solana::min_output_floor(quoted_out_amount(order)?, tolerance);
    let result = solana::verify_swap_simulation(
        tx_b64,
        &expected.input_mint,
        expected.input_amount,
        &expected.output_mint,
        min_output,
        &expected.owner_pubkey,
        None,
    )
    .await;
    interpret_sim_result(result, allow_jit_fill)
}

async fn execute_managed_order(
    wallet: &Wallet,
    order: &OrderResponse,
    expected: &ExpectedSwap,
    base_url: &str,
    max_slippage_bps: Option<u32>,
) -> Result<ExecuteResponse> {
    let tx_b64 = order
        .transaction
        .as_deref()
        .ok_or_else(|| eyre!("No transaction in order"))?;

    // Managed /execute: Jupiter co-signs + validates server-side and the maker
    // funds RFQ fills just-in-time, so a sim that only ERRORED on a JIT route is
    // submitted anyway (on-chain min-output still enforces honesty).
    presign_simulation_gate(tx_b64, order, expected, allow_jit_fill_for(order), max_slippage_bps).await?;

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
    max_slippage_bps: Option<u32>,
) -> Result<ExecuteResponse> {
    let tx_b64 = order
        .transaction
        .as_deref()
        .ok_or_else(|| eyre!("No transaction in order"))?;
    // Metis submits the signed tx directly via RPC (no Jupiter /execute), so the
    // simulation IS the last line of defence — always strict, never the JIT relax.
    presign_simulation_gate(tx_b64, order, expected, allow_jit_fill_for(order), max_slippage_bps).await?;
    let signed_tx = wallet
        .sign_jupiter_swap(tx_b64, expected)
        .wrap_err("failed to sign Metis swap transaction")?;
    let tx = solana::send_signed_transaction(&signed_tx, None).await?;
    metis_result_to_response(tx)
}

/// Turn a Metis direct-submit `TransactionResult` into an execute response —
/// the money-safety gate, isolated so it can be tested without a network.
///
/// `send_signed_transaction` already turns a proven revert (`OnChainFailure`)
/// into `Err`, so what reaches here is either a confirmed success or
/// `confirmed == false` (confirmation timed out). A timeout is ambiguous — the
/// tx may not have landed, or may have reverted outside the poll window — so
/// reporting `status:"Success"` (and letting the caller write the tamper-evident
/// ledger) for it is the same phantom-trade bug the audit opened. Surface a
/// typed `confirmation_timeout` instead: transient → exit 75, and the caller's
/// `Ok`-only ledger gate skips it. NOT auto-resubmitted — it's a
/// `TransactionError`, not an `ExecuteFailure`, so `execute_with_retry` returns
/// it immediately rather than blindly re-sending a possibly-landed tx.
fn metis_result_to_response(tx: solana::TransactionResult) -> Result<ExecuteResponse> {
    if !tx.confirmed {
        return Err(solana::TransactionError {
            kind: solana::TransactionErrorKind::ConfirmationTimeout,
            detail: format!("Metis swap submitted but confirmation timed out: {}", tx.signature),
        }
        .into());
    }
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
    use solana::SwapSimError as E;

    #[test]
    fn jit_proceed_only_on_would_fail_and_only_when_allowed() {
        // The one safe case: sim ERRORED on a just-in-time-funding route.
        assert!(proceed_despite_sim_failure(&E::OnChainWouldFail("x".into()), true));
        // Same error but the route can't fund JIT (e.g. Metis) → refuse.
        assert!(!proceed_despite_sim_failure(&E::OnChainWouldFail("x".into()), false));
        // SAFETY INVARIANT: a sim that SUCCEEDED but is dishonest, or couldn't
        // run, is NEVER submitted — even on a JIT-funding route.
        assert!(!proceed_despite_sim_failure(&E::OutputBelowQuote("x".into()), true));
        assert!(!proceed_despite_sim_failure(&E::UnsafeDelta("x".into()), true));
        assert!(!proceed_despite_sim_failure(&E::RpcUnavailable("x".into()), true));
    }

    #[test]
    fn sim_failure_kind_maps_each_variant() {
        assert_eq!(sim_failure_kind(&E::OnChainWouldFail("x".into())), ExecuteFailureKind::RouteUnfillable);
        assert_eq!(sim_failure_kind(&E::OutputBelowQuote("x".into())), ExecuteFailureKind::RouteUnfillable);
        assert_eq!(sim_failure_kind(&E::UnsafeDelta("x".into())), ExecuteFailureKind::Unknown);
        // A sim that couldn't reach the RPC is transient, not the permanent
        // Unknown bucket used for the UnsafeDelta security refusal.
        assert_eq!(sim_failure_kind(&E::RpcUnavailable("x".into())), ExecuteFailureKind::Unavailable);
    }

    fn refused_kind(err: eyre::Error) -> ExecuteFailureKind {
        err.downcast_ref::<ExecuteFailure>()
            .expect("refusal carries an ExecuteFailure so error_kind survives")
            .kind
    }

    #[test]
    fn allow_jit_fill_for_is_managed_gasless_only_metis_always_strict() {
        let mk = |backend, gasless| OrderResponse { backend, gasless, ..OrderResponse::default() };
        // Managed /execute may relax only for a gasless (JIT-funding) route.
        assert!(allow_jit_fill_for(&mk(OrderBackend::SwapV2Lite, Some(true))));
        assert!(allow_jit_fill_for(&mk(OrderBackend::Ultra, Some(true))));
        assert!(!allow_jit_fill_for(&mk(OrderBackend::SwapV2Lite, Some(false))));
        assert!(!allow_jit_fill_for(&mk(OrderBackend::Ultra, None)));
        // SAFETY: the Metis direct-submit path is NEVER allowed to relax, even if
        // the order erroneously carried gasless:true — the sim is its only backstop.
        assert!(!allow_jit_fill_for(&mk(OrderBackend::MetisV1Lite, Some(true))));
        assert!(!allow_jit_fill_for(&mk(OrderBackend::MetisV1Lite, Some(false))));
    }

    #[test]
    fn gasless_route_is_treated_as_just_in_time_funded() {
        // RFQ makers sponsor gas → gasless == Some(true) marks the JIT path.
        let rfq = OrderResponse { gasless: Some(true), ..OrderResponse::default() };
        assert!(route_funds_just_in_time(&rfq));
        // AMM routes (Metis) are not gasless → strict.
        let amm = OrderResponse { gasless: Some(false), ..OrderResponse::default() };
        assert!(!route_funds_just_in_time(&amm));
        // Absent field is NOT treated as JIT (fail-safe toward strict).
        let unknown = OrderResponse { gasless: None, ..OrderResponse::default() };
        assert!(!route_funds_just_in_time(&unknown));
    }

    #[test]
    fn interpret_passes_a_clean_simulation() {
        assert!(interpret_sim_result(Ok(()), false).is_ok());
        assert!(interpret_sim_result(Ok(()), true).is_ok());
    }

    #[test]
    fn interpret_submits_errored_sim_only_on_jit_route() {
        // Gasless/JIT route: an errored sim (maker funds at /execute) is submitted.
        assert!(interpret_sim_result(Err(E::OnChainWouldFail("x".into()).into()), true).is_ok());
        // Non-JIT route (e.g. Metis): the same errored sim is refused.
        let refused = interpret_sim_result(Err(E::OnChainWouldFail("x".into()).into()), false).unwrap_err();
        assert_eq!(refused_kind(refused), ExecuteFailureKind::RouteUnfillable);
    }

    #[test]
    fn interpret_never_submits_dishonest_or_unrunnable_sim_even_on_jit() {
        // SAFETY INVARIANT: a sim that SUCCEEDED but is dishonest, or that could
        // not run, is NEVER submitted — even on a just-in-time-funding route.
        assert_eq!(
            refused_kind(interpret_sim_result(Err(E::OutputBelowQuote("x".into()).into()), true).unwrap_err()),
            ExecuteFailureKind::RouteUnfillable
        );
        assert_eq!(
            refused_kind(interpret_sim_result(Err(E::UnsafeDelta("x".into()).into()), true).unwrap_err()),
            ExecuteFailureKind::Unknown
        );
        assert_eq!(
            refused_kind(interpret_sim_result(Err(E::RpcUnavailable("x".into()).into()), true).unwrap_err()),
            ExecuteFailureKind::Unavailable
        );
    }

    #[test]
    fn interpret_fails_closed_on_non_sim_error() {
        // An unexpected (non-SwapSimError) failure is refused, never submitted.
        let refused = interpret_sim_result(Err(eyre!("rpc socket exploded")), true).unwrap_err();
        assert_eq!(refused_kind(refused), ExecuteFailureKind::Unknown);
    }

    #[test]
    fn execute_http_error_kind_maps_transient_vs_hard() {
        use reqwest::StatusCode;
        // Provably-not-executed → retryable with a fresh order.
        assert_eq!(execute_http_error_kind(StatusCode::TOO_MANY_REQUESTS), ExecuteFailureKind::Unavailable);
        assert_eq!(execute_http_error_kind(StatusCode::SERVICE_UNAVAILABLE), ExecuteFailureKind::Unavailable);
        // Ambiguous — Jupiter may have executed before the gateway gave up:
        // retrying could double-submit the trade. Hard errors.
        assert_eq!(execute_http_error_kind(StatusCode::BAD_GATEWAY), ExecuteFailureKind::Unknown);
        assert_eq!(execute_http_error_kind(StatusCode::GATEWAY_TIMEOUT), ExecuteFailureKind::Unknown);
        assert_eq!(execute_http_error_kind(StatusCode::INTERNAL_SERVER_ERROR), ExecuteFailureKind::Unknown);
        assert_eq!(execute_http_error_kind(StatusCode::BAD_REQUEST), ExecuteFailureKind::Unknown);
    }

    #[test]
    fn unparseable_out_amount_refuses_to_sign() {
        let order = OrderResponse {
            out_amount: "not-a-number".to_string(),
            ..OrderResponse::default()
        };
        let err = quoted_out_amount(&order).unwrap_err();
        assert_eq!(err.kind, ExecuteFailureKind::Unknown);
        assert!(err.message.contains("refusing to sign"));

        let order = OrderResponse {
            out_amount: "40273156".to_string(),
            ..OrderResponse::default()
        };
        assert_eq!(quoted_out_amount(&order).unwrap(), 40_273_156);
    }

    #[test]
    fn metis_confirmed_result_builds_success_response() {
        // A genuinely confirmed Metis submit stays exactly as before: a Success
        // response carrying the signature (this is what feeds the ledger).
        let tx = solana::TransactionResult {
            signature: "Sig111".to_string(),
            confirmed: true,
        };
        let resp = metis_result_to_response(tx).expect("confirmed tx → Success");
        assert_eq!(resp.status.as_deref(), Some("Success"));
        assert_eq!(resp.signature.as_deref(), Some("Sig111"));
    }

    #[test]
    fn metis_timeout_surfaces_confirmation_timeout_not_fake_success() {
        // Audit M1 (timeout half): a submitted-but-unconfirmed Metis tx is
        // ambiguous — it must NOT be reported as a completed Success, and must
        // NOT reach the ledger (the caller's Ok-only gate skips an Err).
        let tx = solana::TransactionResult {
            signature: "SigTimeout".to_string(),
            confirmed: false,
        };
        let err = metis_result_to_response(tx)
            .expect_err("an unconfirmed (timed-out) tx must be Err, not a phantom Success");

        // Classified as confirmation_timeout for agents/scripts …
        assert_eq!(
            crate::usecases::gm::classify_error(&err),
            Some("confirmation_timeout"),
        );
        // … which is transient (exit 75 — user re-checks/retries), not a hard 1.
        assert!(crate::usecases::gm::is_transient_kind("confirmation_timeout"));

        // NOT auto-resubmitted: it is a TransactionError, not an ExecuteFailure,
        // so execute_with_retry's downcast yields no retry_action and the error
        // is returned immediately rather than re-sending a possibly-landed tx.
        assert!(
            err.downcast_ref::<ExecuteFailure>().is_none(),
            "a timeout must not carry an ExecuteFailure retry_action — no blind resubmit",
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
}
