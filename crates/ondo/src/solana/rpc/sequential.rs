//! Sequential URL strategy: try each URL in `LAST_GOOD_IDX` order, with up to
//! 3 retries per URL (exponential backoff). First successful response wins.
//! Use for writes (`sendTransaction`) and ordering-sensitive reads
//! (`getLatestBlockhash` ahead of a signed tx).

use eyre::Result;
use reqwest::StatusCode;
use std::sync::atomic::Ordering;

use super::error::{SolanaRpcError, SolanaRpcErrorKind};
use super::{
    backoff_with_jitter, ordered_indices, single_batch_attempt, RpcRequest, RpcResponse,
    LAST_GOOD_IDX,
};

/// Make a single JSON-RPC call with retry across multiple RPC URLs.
pub(super) async fn rpc_call_with_retry<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    urls: &[&str],
    req: &RpcRequest<'_>,
) -> Result<RpcResponse<T>> {
    let timeout = std::time::Duration::from_secs(15);
    let mut last_err: Option<SolanaRpcError> = None;

    for (try_num, idx) in ordered_indices(urls.len()).enumerate() {
        let url = urls[idx];
        for attempt in 0..3u32 {
            if attempt > 0 {
                let delay = backoff_with_jitter(attempt);
                tokio::time::sleep(delay).await;
            } else if try_num > 0 {
                // Short delay when switching to a new URL (not a full backoff)
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            let resp = match client.post(url).json(req).timeout(timeout).send().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(SolanaRpcError::new(
                        SolanaRpcErrorKind::Network,
                        Some(req.method),
                        Some(url),
                        None,
                        None,
                        e.to_string(),
                    ));
                    break; // connection error → try next URL
                }
            };

            let status = resp.status();
            if status == StatusCode::TOO_MANY_REQUESTS {
                last_err = Some(SolanaRpcError::new(
                    SolanaRpcErrorKind::RateLimited,
                    Some(req.method),
                    Some(url),
                    Some(status),
                    None,
                    "HTTP 429 from RPC endpoint",
                ));
                continue; // rate limited → retry same URL with backoff
            }
            if status.is_server_error() {
                last_err = Some(SolanaRpcError::new(
                    SolanaRpcErrorKind::HttpStatus,
                    Some(req.method),
                    Some(url),
                    Some(status),
                    None,
                    "server-side RPC failure",
                ));
                continue; // rate limited or 5xx → retry same URL with backoff
            }
            if status.is_client_error() {
                return Err(SolanaRpcError::new(
                    SolanaRpcErrorKind::HttpStatus,
                    Some(req.method),
                    Some(url),
                    Some(status),
                    None,
                    "client-side RPC failure",
                )
                .into());
            }

            let parsed: RpcResponse<T> = match resp.json().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(SolanaRpcError::new(
                        SolanaRpcErrorKind::Decode,
                        Some(req.method),
                        Some(url),
                        None,
                        None,
                        format!("error decoding response body: {e}"),
                    ));
                    continue; // HTML / bad response → retry with backoff
                }
            };

            if let Some(ref err) = parsed.error {
                if err.message.contains("Too many requests") {
                    last_err = Some(SolanaRpcError::new(
                        SolanaRpcErrorKind::RateLimited,
                        Some(req.method),
                        Some(url),
                        None,
                        err.code,
                        err.message.clone(),
                    ));
                    continue; // retry same URL with backoff
                }
                return Err(SolanaRpcError::new(
                    SolanaRpcErrorKind::RpcResponse,
                    Some(req.method),
                    Some(url),
                    None,
                    err.code,
                    err.message.clone(),
                )
                .into());
            }
            // Malformed response: no result AND no error (per JSON-RPC 2.0, result is required on success)
            if parsed.result.is_none() {
                last_err = Some(SolanaRpcError::new(
                    SolanaRpcErrorKind::EmptyResult,
                    Some(req.method),
                    Some(url),
                    None,
                    None,
                    "RPC returned null result",
                ));
                continue; // retry — likely transient issue
            }

            LAST_GOOD_IDX.store(idx, Ordering::Relaxed);
            return Ok(parsed);
        }
    }

    Err(SolanaRpcError::new(
        SolanaRpcErrorKind::Unavailable,
        Some(req.method),
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
    .into())
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
                if !e.is_retryable() {
                    return Err(e.into());
                }
                last_err = Some(e);
            }
        }
    }

    Err(SolanaRpcError::new(
        SolanaRpcErrorKind::Unavailable,
        None,
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
    .into())
}
