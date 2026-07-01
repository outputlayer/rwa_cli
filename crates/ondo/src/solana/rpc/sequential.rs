//! Sequential URL strategy: try each URL in `LAST_GOOD_IDX` order, with up to
//! 3 retries per URL (exponential backoff). First successful response wins.
//! Use for writes (`sendTransaction`) and ordering-sensitive reads
//! (`getLatestBlockhash` ahead of a signed tx).
//!
//! Per-URL retry/classification lives in `single_attempt`/`single_batch_attempt`
//! (shared with the race path); the loops here only own URL ordering and the
//! final `Unavailable` error.

use eyre::Result;
use std::sync::atomic::Ordering;

use super::error::{SolanaRpcError, SolanaRpcErrorKind};
use super::{
    ordered_indices, single_attempt, single_batch_attempt, RpcRequest, RpcResponse, LAST_GOOD_IDX,
};

/// Whether an exhausted single-URL attempt still warrants trying the next URL.
/// Retryable kinds (network, rate limit, decode, empty result) obviously do;
/// a server-side 5xx does too — it condemns that endpoint, not the request.
/// Client errors (4xx) and RPC-level errors are request problems: fail fast.
fn should_try_next_url(e: &SolanaRpcError) -> bool {
    e.is_retryable()
        || (e.kind == SolanaRpcErrorKind::HttpStatus
            && e.status.is_some_and(|s| s.is_server_error()))
}

fn all_exhausted(method: Option<&str>, last_err: Option<SolanaRpcError>) -> SolanaRpcError {
    SolanaRpcError::new(
        SolanaRpcErrorKind::Unavailable,
        method,
        None,
        None,
        None,
        format!(
            "all RPC endpoints were exhausted; last error: {}. Hint: set RWA_RPC_URL to a private RPC endpoint, or retry in a few seconds.",
            last_err
                .map(|err| err.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        ),
    )
}

/// Make a single JSON-RPC call with retry across multiple RPC URLs.
pub(super) async fn rpc_call_with_retry<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    urls: &[&str],
    req: &RpcRequest<'_>,
) -> Result<RpcResponse<T>> {
    let timeout = std::time::Duration::from_secs(15);
    let mut last_err: Option<SolanaRpcError> = None;

    for (try_num, idx) in ordered_indices(urls.len()).enumerate() {
        if try_num > 0 {
            // Short delay when switching to a new URL (not a full backoff)
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let url = urls[idx];
        match single_attempt(client, url, req, timeout).await {
            Ok(resp) => {
                LAST_GOOD_IDX.store(idx, Ordering::Relaxed);
                return Ok(resp);
            }
            Err(e) => {
                if !should_try_next_url(&e) {
                    return Err(e.into());
                }
                last_err = Some(e);
            }
        }
    }

    Err(all_exhausted(Some(req.method), last_err).into())
}

/// Sequential batch RPC: try each URL with retries; first URL that returns
/// a well-formed batch response wins.
pub(super) async fn rpc_batch_sequential(
    client: &reqwest::Client,
    urls: &[&str],
    reqs: &[RpcRequest<'_>],
) -> Result<Vec<serde_json::Value>> {
    let timeout = std::time::Duration::from_secs(20);
    let mut last_err: Option<SolanaRpcError> = None;

    for (try_num, idx) in ordered_indices(urls.len()).enumerate() {
        if try_num > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let url = urls[idx];
        match single_batch_attempt(client, url, reqs, timeout).await {
            Ok(results) => {
                LAST_GOOD_IDX.store(idx, Ordering::Relaxed);
                return Ok(results);
            }
            Err(e) => {
                if !should_try_next_url(&e) {
                    return Err(e.into());
                }
                last_err = Some(e);
            }
        }
    }

    Err(all_exhausted(None, last_err).into())
}
