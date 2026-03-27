use eyre::{Result, eyre};
use serde::{Deserialize, Serialize};

use crate::HTTP;

/// Public Solana RPC endpoints — rotated on rate-limit errors.
/// Order: most stable first. User can override with --rpc-url or RWA_RPC_URL.
const RPC_URLS: &[&str] = &[
    "https://solana-rpc.publicnode.com",           // PublicNode — 10 nodes, stable
    "https://solana-mainnet.rpc.extrnode.com",     // ExtrNode — used by wallets
    "https://api.mainnet-beta.solana.com",         // Solana Foundation — 10 req/s
    "https://rpc.ankr.com/solana",                 // Ankr — ~30 req/min free
    "https://solana.drpc.org",                     // dRPC — decentralized, free tier
];

/// Return the list of RPC URLs to try: user-provided first, then public fallbacks.
pub(crate) fn rpc_urls(custom: Option<&str>) -> Vec<&str> {
    match custom {
        Some(url) => {
            let mut urls = vec![url];
            for u in RPC_URLS {
                if *u != url {
                    urls.push(u);
                }
            }
            urls
        }
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

#[derive(Deserialize)]
pub(super) struct RpcError {
    pub message: String,
}

/// Simple RPC call: builds request, retries with URL rotation, extracts result.
pub(crate) async fn rpc_call_simple<T: serde::de::DeserializeOwned>(
    method: &str,
    params: serde_json::Value,
    rpc_url: Option<&str>,
) -> Result<T> {
    let client = &*HTTP;
    let req = RpcRequest {
        jsonrpc: "2.0",
        id: 1,
        method,
        params,
    };
    let resp: RpcResponse<T> = rpc_call_with_retry(client, &rpc_urls(rpc_url), &req).await?;
    resp.result.ok_or_else(|| eyre!("Empty response from Solana RPC ({method})"))
}

/// Make a single JSON-RPC call with retry across multiple RPC URLs.
async fn rpc_call_with_retry<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    urls: &[&str],
    req: &RpcRequest<'_>,
) -> Result<RpcResponse<T>> {
    let timeout = std::time::Duration::from_secs(15);
    let mut last_err = String::new();

    for (url_idx, url) in urls.iter().enumerate() {
        for attempt in 0..3u32 {
            if attempt > 0 || url_idx > 0 {
                let delay = backoff_with_jitter(attempt + 1);
                tokio::time::sleep(delay).await;
            }
            let resp = match client.post(*url).json(req).timeout(timeout).send().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = e.to_string();
                    break; // connection error → try next URL
                }
            };

            let status = resp.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                last_err = format!("HTTP {status}");
                continue; // rate limited or 5xx → retry same URL with backoff
            }

            let parsed: RpcResponse<T> = match resp.json().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = format!("error decoding response body: {e}");
                    continue; // HTML / bad response → retry with backoff
                }
            };

            if let Some(ref err) = parsed.error {
                if err.message.contains("Too many requests") {
                    last_err = err.message.clone();
                    continue; // retry same URL with backoff
                }
                return Err(eyre!("Solana RPC error: {}", err.message));
            }
            // Malformed response: no result AND no error (per JSON-RPC 2.0, result is required on success)
            if parsed.result.is_none() {
                last_err = "RPC returned null result".to_string();
                continue; // retry — likely transient issue
            }
            return Ok(parsed);
        }
    }

    Err(eyre!(
        "Solana RPC unavailable (all endpoints rate-limited or down).\n  \
         Last error: {last_err}\n  \
         Hint: set RWA_RPC_URL to a private RPC endpoint, or retry in a few seconds."
    ))
}

/// Make a batch RPC call (multiple requests in one HTTP request) with retry.
pub(super) async fn rpc_batch_with_retry(
    client: &reqwest::Client,
    urls: &[&str],
    reqs: &[RpcRequest<'_>],
) -> Result<Vec<serde_json::Value>> {
    let timeout = std::time::Duration::from_secs(20);
    let mut last_err = String::new();

    for (url_idx, url) in urls.iter().enumerate() {
        for attempt in 0..3u32 {
            if attempt > 0 || url_idx > 0 {
                let delay = backoff_with_jitter(attempt + 1);
                tokio::time::sleep(delay).await;
            }
            let resp = match client.post(*url).json(&reqs).timeout(timeout).send().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = e.to_string();
                    break; // connection error → try next URL
                }
            };

            let status = resp.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                last_err = format!("HTTP {status}");
                continue;
            }

            let results: Vec<serde_json::Value> = match resp.json().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = format!("error decoding response body: {e}");
                    continue; // HTML / bad response → retry with backoff
                }
            };

            if results.len() != reqs.len() {
                last_err = format!("batch: got {} responses, expected {}", results.len(), reqs.len());
                continue;
            }

            return Ok(results);
        }
    }

    Err(eyre!(
        "Solana RPC unavailable (all endpoints rate-limited or down).\n  \
         Last error: {last_err}\n  \
         Hint: set RWA_RPC_URL to a private RPC endpoint, or retry in a few seconds."
    ))
}

/// Backoff with jitter to avoid thundering herd on rate-limited endpoints.
/// Returns base delay (attempt × 1s) plus random jitter (0–500ms).
pub(super) fn backoff_with_jitter(attempt: u32) -> std::time::Duration {
    let base_ms = 1000u64 * attempt as u64;
    let jitter_ms = rand::random::<u64>() % 500;
    std::time::Duration::from_millis(base_ms + jitter_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_urls_default_returns_all() {
        let urls = rpc_urls(None);
        assert_eq!(urls.len(), RPC_URLS.len());
        assert_eq!(urls[0], RPC_URLS[0]);
    }

    #[test]
    fn rpc_urls_custom_prepends() {
        let custom = "https://my-rpc.example.com";
        let urls = rpc_urls(Some(custom));
        assert_eq!(urls[0], custom);
        assert_eq!(urls.len(), RPC_URLS.len() + 1);
    }

    #[test]
    fn rpc_urls_custom_deduplicates() {
        let urls = rpc_urls(Some(RPC_URLS[0]));
        assert_eq!(urls.len(), RPC_URLS.len());
        assert_eq!(urls[0], RPC_URLS[0]);
    }

    #[test]
    fn backoff_with_jitter_increases_with_attempt() {
        let d1 = backoff_with_jitter(1);
        let d3 = backoff_with_jitter(3);
        assert!(d1.as_millis() >= 1000 && d1.as_millis() < 1500);
        assert!(d3.as_millis() >= 3000 && d3.as_millis() < 3500);
    }

    #[test]
    fn http_client_is_initialized() {
        let _ = &*HTTP;
    }
}
