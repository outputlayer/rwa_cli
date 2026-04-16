use eyre::Result;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::HTTP;

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
    #[allow(dead_code)] // Used from Task 5 onward — enabled now so Task 1 compiles clean.
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

#[derive(Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SolanaRpcErrorKind {
    Network,
    RateLimited,
    HttpStatus,
    Decode,
    RpcResponse,
    EmptyResult,
    BatchShape,
    Unavailable,
}

#[derive(Debug)]
pub(super) struct SolanaRpcError {
    pub kind: SolanaRpcErrorKind,
    pub method: Option<String>,
    pub url: Option<String>,
    pub status: Option<StatusCode>,
    pub code: Option<i64>,
    pub detail: String,
}

impl SolanaRpcError {
    fn new(
        kind: SolanaRpcErrorKind,
        method: Option<&str>,
        url: Option<&str>,
        status: Option<StatusCode>,
        code: Option<i64>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            method: method.map(str::to_string),
            url: url.map(str::to_string),
            status,
            code,
            detail: detail.into(),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.kind,
            SolanaRpcErrorKind::Network
                | SolanaRpcErrorKind::RateLimited
                | SolanaRpcErrorKind::Decode
                | SolanaRpcErrorKind::EmptyResult
                | SolanaRpcErrorKind::BatchShape
                | SolanaRpcErrorKind::Unavailable
        )
    }
}

impl std::fmt::Display for SolanaRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let method = self.method.as_deref().unwrap_or("batch");
        let url = self.url.as_deref().unwrap_or("<unknown>");
        match (self.status, self.code) {
            (Some(status), Some(code)) => write!(
                f,
                "Solana RPC error [{}] method={} url={} status={} code={}: {}",
                self.kind, method, url, status, code, self.detail
            ),
            (Some(status), None) => write!(
                f,
                "Solana RPC error [{}] method={} url={} status={}: {}",
                self.kind, method, url, status, self.detail
            ),
            (None, Some(code)) => write!(
                f,
                "Solana RPC error [{}] method={} url={} code={}: {}",
                self.kind, method, url, code, self.detail
            ),
            (None, None) => write!(
                f,
                "Solana RPC error [{}] method={} url={}: {}",
                self.kind, method, url, self.detail
            ),
        }
    }
}

impl std::fmt::Display for SolanaRpcErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::Network => "network",
            Self::RateLimited => "rate_limited",
            Self::HttpStatus => "http_status",
            Self::Decode => "decode",
            Self::RpcResponse => "rpc_response",
            Self::EmptyResult => "empty_result",
            Self::BatchShape => "batch_shape",
            Self::Unavailable => "unavailable",
        };
        f.write_str(label)
    }
}

impl std::error::Error for SolanaRpcError {}

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
        RpcMode::Sequential => rpc_call_with_retry(client, &urls, &req).await?,
        // TEMP: race impl in Task 2 — still sequential so behavior is unchanged.
        RpcMode::Race => rpc_call_with_retry(client, &urls, &req).await?,
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

/// Iterate URL indices starting from the last known good endpoint, wrapping around.
fn ordered_indices(len: usize) -> impl Iterator<Item = usize> {
    let start = LAST_GOOD_IDX.load(Ordering::Relaxed) % len;
    (0..len).map(move |i| (start + i) % len)
}

/// Make a single JSON-RPC call with retry across multiple RPC URLs.
async fn rpc_call_with_retry<T: serde::de::DeserializeOwned>(
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

/// Make a batch RPC call (multiple requests in one HTTP request) with retry.
pub(super) async fn rpc_batch_with_retry(
    client: &reqwest::Client,
    urls: &[&str],
    reqs: &[RpcRequest<'_>],
    mode: RpcMode,
) -> Result<Vec<serde_json::Value>> {
    match mode {
        RpcMode::Sequential => rpc_batch_sequential(client, urls, reqs).await,
        // TEMP: race impl in Task 3 — still sequential so behavior is unchanged.
        RpcMode::Race => rpc_batch_sequential(client, urls, reqs).await,
    }
}

async fn rpc_batch_sequential(
    client: &reqwest::Client,
    urls: &[&str],
    reqs: &[RpcRequest<'_>],
) -> Result<Vec<serde_json::Value>> {
    let timeout = std::time::Duration::from_secs(20);
    let mut last_err: Option<SolanaRpcError> = None;

    for (try_num, idx) in ordered_indices(urls.len()).enumerate() {
        let url = urls[idx];
        for attempt in 0..3u32 {
            if attempt > 0 {
                let delay = backoff_with_jitter(attempt);
                tokio::time::sleep(delay).await;
            } else if try_num > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            let resp = match client.post(url).json(&reqs).timeout(timeout).send().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(SolanaRpcError::new(
                        SolanaRpcErrorKind::Network,
                        None,
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
                    None,
                    Some(url),
                    Some(status),
                    None,
                    "HTTP 429 from RPC endpoint",
                ));
                continue; // rate limited or 5xx → retry same URL with backoff
            }
            if status.is_server_error() {
                last_err = Some(SolanaRpcError::new(
                    SolanaRpcErrorKind::HttpStatus,
                    None,
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
                    None,
                    Some(url),
                    Some(status),
                    None,
                    "client-side RPC failure",
                )
                .into());
            }

            let mut results: Vec<serde_json::Value> = match resp.json().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(SolanaRpcError::new(
                        SolanaRpcErrorKind::Decode,
                        None,
                        Some(url),
                        None,
                        None,
                        format!("error decoding response body: {e}"),
                    ));
                    continue; // HTML / bad response → retry with backoff
                }
            };

            if results.len() != reqs.len() {
                last_err = Some(SolanaRpcError::new(
                    SolanaRpcErrorKind::BatchShape,
                    None,
                    Some(url),
                    None,
                    None,
                    format!("got {} responses, expected {}", results.len(), reqs.len()),
                ));
                continue;
            }

            // Check if any individual response contains an RPC error
            if let Some((err_code, err_msg)) = results.iter().find_map(|r| {
                let error = r.get("error")?;
                let msg = error.get("message")?.as_str()?;
                let code = error.get("code").and_then(|code| code.as_i64());
                Some((code, msg))
            }) {
                if err_msg.contains("Too many requests") {
                    last_err = Some(SolanaRpcError::new(
                        SolanaRpcErrorKind::RateLimited,
                        None,
                        Some(url),
                        None,
                        err_code,
                        err_msg.to_string(),
                    ));
                    continue;
                }
                return Err(SolanaRpcError::new(
                    SolanaRpcErrorKind::RpcResponse,
                    None,
                    Some(url),
                    None,
                    err_code,
                    format!("RPC error in batch: {err_msg}"),
                )
                .into());
            }

            // All responses must have a "result" field (JSON-RPC 2.0 spec)
            if results.iter().any(|r| r.get("result").is_none()) {
                last_err = Some(SolanaRpcError::new(
                    SolanaRpcErrorKind::BatchShape,
                    None,
                    Some(url),
                    None,
                    None,
                    "missing 'result' in one or more responses",
                ));
                continue; // malformed response → retry
            }

            // JSON-RPC 2.0 spec: batch responses may arrive in any order.
            // Sort by id to match request order so callers can index by position.
            results.sort_by_key(|r| r.get("id").and_then(|id| id.as_u64()).unwrap_or(u64::MAX));

            LAST_GOOD_IDX.store(idx, Ordering::Relaxed);
            return Ok(results);
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

/// Backoff with jitter to avoid thundering herd on rate-limited endpoints.
/// Exponential: 1s, 2s, 4s (capped at 10s) plus random jitter (0–500ms).
pub(super) fn backoff_with_jitter(attempt: u32) -> std::time::Duration {
    let base_ms = (1000u64 << attempt.saturating_sub(1)).min(10_000);
    let jitter_ms = rand::random::<u64>() % 500;
    std::time::Duration::from_millis(base_ms + jitter_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn solana_rpc_error_rate_limit_is_retryable() {
        let err = SolanaRpcError::new(
            SolanaRpcErrorKind::RateLimited,
            Some("getBalance"),
            Some("https://rpc.example"),
            Some(StatusCode::TOO_MANY_REQUESTS),
            None,
            "HTTP 429 from RPC endpoint",
        );
        assert!(err.is_retryable());
        assert!(err.to_string().contains("rate_limited"));
    }

    #[test]
    fn solana_rpc_error_client_http_is_not_retryable() {
        let err = SolanaRpcError::new(
            SolanaRpcErrorKind::HttpStatus,
            Some("getBalance"),
            Some("https://rpc.example"),
            Some(StatusCode::UNAUTHORIZED),
            None,
            "client-side RPC failure",
        );
        assert!(!err.is_retryable());
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
