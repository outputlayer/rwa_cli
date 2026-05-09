//! Solana JSON-RPC client with multi-URL strategies.
//!
//! Public entry points:
//! - [`rpc_call_simple`] — single JSON-RPC request, sequential or race mode.
//! - [`rpc_batch_with_retry`] — batched JSON-RPC request, sequential or race
//!   mode (used by sibling modules within `solana`).
//!
//! Sub-modules:
//! - `error` — `SolanaRpcError` taxonomy + retryability classifier.
//! - `sequential` — try-each-URL strategy with per-URL retries.
//! - `race` — fire-all-URLs strategy.

use eyre::Result;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::atomic::AtomicUsize;

use crate::HTTP;

mod error;
mod race;
mod sequential;

pub(super) use error::{SolanaRpcError, SolanaRpcErrorKind};

/// How to distribute RPC calls across the configured URLs.
///
/// `Sequential` tries one URL at a time in `LAST_GOOD_IDX` order, with up to
/// 3 retries per URL (backoff between retries). First successful response wins.
/// Use for writes (`sendTransaction`) and ordering-sensitive reads
/// (`getLatestBlockhash` ahead of a signed tx).
///
/// `Race` fires all configured URLs in parallel and returns the first success.
/// Losing in-flight requests are aborted. Use for any pure-read RPC method
/// where the response is cluster-wide state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RpcMode {
    Sequential,
    Race,
}

/// Index of the last RPC URL that responded successfully.
/// Subsequent calls start from this index, skipping known-dead endpoints.
static LAST_GOOD_IDX: AtomicUsize = AtomicUsize::new(0);

/// Public Solana RPC endpoints — rotated on rate-limit errors.
/// Only nodes that support unauthenticated JSON-RPC batch requests are listed here.
/// Order: most stable first. User can override with --rpc-url or RWA_RPC_URL.
///
/// Excluded (tested, broken for free-tier batch):
///   extrnode (solana-mainnet.rpc.extrnode.com) — returns 401, requires auth
///   drpc (solana.drpc.org) — returns 400 on all requests, auth-gated
const RPC_URLS: &[&str] = &[
    "https://api.mainnet-beta.solana.com", // Solana Foundation — most reliable
    "https://solana-rpc.publicnode.com",   // PublicNode — stable, no auth required
];

/// Return the list of RPC URLs to try.
/// Custom URL: use only that URL — the user knows what they want, no silent fallback.
/// No custom URL: rotate through the public fallback list.
pub(crate) fn rpc_urls(custom: Option<&str>) -> Vec<&str> {
    match custom {
        Some(url) => vec![url],
        None => RPC_URLS.to_vec(),
    }
}

#[derive(Serialize)]
pub(super) struct RpcRequest<'a> {
    pub jsonrpc: &'a str,
    pub id: u64,
    pub method: &'a str,
    pub params: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub(super) struct RpcResponse<T> {
    pub result: Option<T>,
    pub error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RpcError {
    #[serde(default)]
    pub code: Option<i64>,
    pub message: String,
}

/// Simple RPC call: builds request, retries with URL rotation, extracts result.
pub(crate) async fn rpc_call_simple<T: serde::de::DeserializeOwned + Send + 'static>(
    method: &str,
    params: serde_json::Value,
    rpc_url: Option<&str>,
    mode: RpcMode,
) -> Result<T> {
    let client = &*HTTP;
    let req = RpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method,
        params,
    };
    let urls = rpc_urls(rpc_url);
    let resp: RpcResponse<T> = match mode {
        RpcMode::Sequential => sequential::rpc_call_with_retry(client, &urls, &req).await?,
        RpcMode::Race => race::rpc_call_race(client, &urls, &req).await?,
    };
    resp.result.ok_or_else(|| {
        SolanaRpcError::new(
            SolanaRpcErrorKind::EmptyResult,
            Some(method),
            None,
            None,
            None,
            "successful JSON-RPC response did not include result",
        )
        .into()
    })
}

/// Make a batch RPC call (multiple requests in one HTTP request) with retry.
pub(super) async fn rpc_batch_with_retry(
    client: &reqwest::Client,
    urls: &[&str],
    reqs: &[RpcRequest<'_>],
    mode: RpcMode,
) -> Result<Vec<serde_json::Value>> {
    match mode {
        RpcMode::Sequential => sequential::rpc_batch_sequential(client, urls, reqs).await,
        RpcMode::Race => race::rpc_batch_race(client, urls, reqs).await,
    }
}

/// Iterate URL indices starting from the last known good endpoint, wrapping around.
pub(super) fn ordered_indices(len: usize) -> impl Iterator<Item = usize> {
    use std::sync::atomic::Ordering;
    let start = LAST_GOOD_IDX.load(Ordering::Relaxed) % len;
    (0..len).map(move |i| (start + i) % len)
}

/// Backoff with jitter to avoid thundering herd on rate-limited endpoints.
/// Exponential: 1s, 2s, 4s (capped at 10s) plus random jitter (0–500ms).
pub(super) fn backoff_with_jitter(attempt: u32) -> std::time::Duration {
    use rand::Rng;
    let base_ms = (1000u64 << attempt.saturating_sub(1)).min(10_000);
    let jitter_ms = rand::thread_rng().gen_range(0..500);
    std::time::Duration::from_millis(base_ms + jitter_ms)
}

/// Make up to 3 attempts against a single URL with exponential backoff.
/// Returns the parsed `RpcResponse` on the first success.
/// Returns `Err(SolanaRpcError)` after exhausting retries or on a non-retryable
/// error (e.g. 4xx client error, RPC error that isn't "Too many requests").
///
/// Used by the race orchestrator (each URL gets its own task running this).
pub(super) async fn single_attempt<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
    req: &RpcRequest<'_>,
    timeout: std::time::Duration,
) -> std::result::Result<RpcResponse<T>, SolanaRpcError> {
    let mut last_err: Option<SolanaRpcError> = None;

    for attempt in 0..3u32 {
        if attempt > 0 {
            let delay = backoff_with_jitter(attempt);
            tokio::time::sleep(delay).await;
        }

        let resp = match client.post(url).json(req).timeout(timeout).send().await {
            Ok(r) => r,
            Err(e) => {
                return Err(SolanaRpcError::new(
                    SolanaRpcErrorKind::Network,
                    Some(req.method),
                    Some(url),
                    None,
                    None,
                    e.to_string(),
                ));
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
            continue;
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
            continue;
        }
        if status.is_client_error() {
            return Err(SolanaRpcError::new(
                SolanaRpcErrorKind::HttpStatus,
                Some(req.method),
                Some(url),
                Some(status),
                None,
                "client-side RPC failure",
            ));
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
                continue;
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
                continue;
            }
            return Err(SolanaRpcError::new(
                SolanaRpcErrorKind::RpcResponse,
                Some(req.method),
                Some(url),
                None,
                err.code,
                err.message.clone(),
            ));
        }

        if parsed.result.is_none() {
            last_err = Some(SolanaRpcError::new(
                SolanaRpcErrorKind::EmptyResult,
                Some(req.method),
                Some(url),
                None,
                None,
                "RPC returned null result",
            ));
            continue;
        }

        return Ok(parsed);
    }

    Err(last_err.unwrap_or_else(|| {
        SolanaRpcError::new(
            SolanaRpcErrorKind::Unavailable,
            Some(req.method),
            Some(url),
            None,
            None,
            "all retry attempts exhausted",
        )
    }))
}

/// One batch attempt against one URL with up to 3 retries.
/// Used by both the sequential and race batch paths.
pub(super) async fn single_batch_attempt(
    client: &reqwest::Client,
    url: &str,
    reqs: &[RpcRequest<'_>],
    timeout: std::time::Duration,
) -> std::result::Result<Vec<serde_json::Value>, SolanaRpcError> {
    let mut last_err: Option<SolanaRpcError> = None;

    for attempt in 0..3u32 {
        if attempt > 0 {
            let delay = backoff_with_jitter(attempt);
            tokio::time::sleep(delay).await;
        }

        let resp = match client.post(url).json(reqs).timeout(timeout).send().await {
            Ok(r) => r,
            Err(e) => {
                return Err(SolanaRpcError::new(
                    SolanaRpcErrorKind::Network,
                    None, Some(url), None, None,
                    e.to_string(),
                ));
            }
        };

        let status = resp.status();
        if status == StatusCode::TOO_MANY_REQUESTS {
            last_err = Some(SolanaRpcError::new(
                SolanaRpcErrorKind::RateLimited,
                None, Some(url), Some(status), None,
                "HTTP 429 from RPC endpoint",
            ));
            continue;
        }
        if status.is_server_error() {
            last_err = Some(SolanaRpcError::new(
                SolanaRpcErrorKind::HttpStatus,
                None, Some(url), Some(status), None,
                "server-side RPC failure",
            ));
            continue;
        }
        if status.is_client_error() {
            return Err(SolanaRpcError::new(
                SolanaRpcErrorKind::HttpStatus,
                None, Some(url), Some(status), None,
                "client-side RPC failure",
            ));
        }

        let mut results: Vec<serde_json::Value> = match resp.json().await {
            Ok(r) => r,
            Err(e) => {
                last_err = Some(SolanaRpcError::new(
                    SolanaRpcErrorKind::Decode,
                    None, Some(url), None, None,
                    format!("error decoding response body: {e}"),
                ));
                continue;
            }
        };

        if results.len() != reqs.len() {
            last_err = Some(SolanaRpcError::new(
                SolanaRpcErrorKind::BatchShape,
                None, Some(url), None, None,
                format!("got {} responses, expected {}", results.len(), reqs.len()),
            ));
            continue;
        }

        if let Some((err_code, err_msg)) = results.iter().find_map(|r| {
            let error = r.get("error")?;
            let msg = error.get("message")?.as_str()?;
            let code = error.get("code").and_then(|code| code.as_i64());
            Some((code, msg))
        }) {
            if err_msg.contains("Too many requests") {
                last_err = Some(SolanaRpcError::new(
                    SolanaRpcErrorKind::RateLimited,
                    None, Some(url), None, err_code,
                    err_msg.to_string(),
                ));
                continue;
            }
            return Err(SolanaRpcError::new(
                SolanaRpcErrorKind::RpcResponse,
                None, Some(url), None, err_code,
                format!("RPC error in batch: {err_msg}"),
            ));
        }

        if results.iter().any(|r| r.get("result").is_none()) {
            last_err = Some(SolanaRpcError::new(
                SolanaRpcErrorKind::BatchShape,
                None, Some(url), None, None,
                "missing 'result' in one or more responses",
            ));
            continue;
        }

        results.sort_by_key(|r| r.get("id").and_then(|id| id.as_u64()).unwrap_or(u64::MAX));
        return Ok(results);
    }

    Err(last_err.unwrap_or_else(|| {
        SolanaRpcError::new(
            SolanaRpcErrorKind::Unavailable,
            None, Some(url), None, None,
            "all batch retry attempts exhausted",
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::Ordering;

    #[test]
    fn rpc_urls_default_returns_all() {
        let urls = rpc_urls(None);
        assert_eq!(urls.len(), RPC_URLS.len());
        assert_eq!(urls[0], RPC_URLS[0]);
    }

    #[test]
    fn rpc_urls_custom_returns_only_that_url() {
        let custom = "https://my-rpc.example.com";
        let urls = rpc_urls(Some(custom));
        assert_eq!(urls, vec![custom]);
    }

    #[test]
    fn backoff_with_jitter_is_exponential() {
        let d1 = backoff_with_jitter(1);
        let d2 = backoff_with_jitter(2);
        let d3 = backoff_with_jitter(3);
        // attempt=1: base=1000ms, jitter 0-499ms → 1000..1499
        assert!(d1.as_millis() >= 1000 && d1.as_millis() < 1500, "d1={}", d1.as_millis());
        // attempt=2: base=2000ms, jitter 0-499ms → 2000..2499
        assert!(d2.as_millis() >= 2000 && d2.as_millis() < 2500, "d2={}", d2.as_millis());
        // attempt=3: base=4000ms, jitter 0-499ms → 4000..4499
        assert!(d3.as_millis() >= 4000 && d3.as_millis() < 4500, "d3={}", d3.as_millis());
    }

    #[test]
    fn backoff_with_jitter_caps_at_10s() {
        // attempt=4: base=8000ms; attempt=5: base=16000 → capped to 10000
        let d4 = backoff_with_jitter(4);
        let d5 = backoff_with_jitter(5);
        assert!(d4.as_millis() < 8500, "d4={}", d4.as_millis());
        assert!(d5.as_millis() < 10_500, "d5={}", d5.as_millis());
    }

    #[test]
    fn ordered_indices_respects_last_good() {
        // Test starting from middle
        LAST_GOOD_IDX.store(2, Ordering::Relaxed);
        let indices: Vec<usize> = ordered_indices(5).collect();
        assert_eq!(indices, vec![2, 3, 4, 0, 1]);

        // Test wrapping from end
        LAST_GOOD_IDX.store(4, Ordering::Relaxed);
        let indices: Vec<usize> = ordered_indices(5).collect();
        assert_eq!(indices, vec![4, 0, 1, 2, 3]);

        // Test starting from 0 (default)
        LAST_GOOD_IDX.store(0, Ordering::Relaxed);
        let indices: Vec<usize> = ordered_indices(5).collect();
        assert_eq!(indices, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn http_client_is_initialized() {
        let _ = &*HTTP;
    }

    #[test]
    fn rpc_error_deserializes_code_and_message() {
        let response: RpcResponse<serde_json::Value> = serde_json::from_value(json!({
            "result": null,
            "error": {
                "code": -32005,
                "message": "Too many requests"
            }
        }))
        .unwrap();

        let err = response.error.expect("rpc error");
        assert_eq!(err.code, Some(-32005));
        assert_eq!(err.message, "Too many requests");
    }
}
