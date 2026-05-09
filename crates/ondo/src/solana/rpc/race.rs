//! Race orchestrator: fire the same request at every URL in parallel,
//! return the first `Ok`. On any task succeeding, remaining tasks are aborted.
//! Use for pure-read RPC methods where the response is cluster-wide state.

use eyre::Result;

use super::error::{SolanaRpcError, SolanaRpcErrorKind};
use super::{single_attempt, single_batch_attempt, RpcRequest, RpcResponse};

/// Owned counterpart of `RpcRequest<'a>`, used to move requests into spawned tasks.
#[derive(Clone)]
struct OwnedRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    params: serde_json::Value,
}

/// Fire the same request at every URL in parallel. Return the first `Ok`.
/// On any task returning `Ok`, remaining tasks are aborted.
/// If every task fails, returns an aggregated `SolanaRpcError` describing all URLs.
pub(super) async fn rpc_call_race<T: serde::de::DeserializeOwned + Send + 'static>(
    client: &reqwest::Client,
    urls: &[&str],
    req: &RpcRequest<'_>,
) -> Result<RpcResponse<T>> {
    let timeout = std::time::Duration::from_secs(8);

    if urls.len() == 1 {
        return single_attempt(client, urls[0], req, timeout).await.map_err(Into::into);
    }

    let mut set = tokio::task::JoinSet::new();
    for url in urls {
        let client = client.clone();
        let url = (*url).to_string();
        let method = req.method.to_string();
        let params = req.params.clone();
        let id = req.id;
        set.spawn(async move {
            let owned_req = RpcRequest {
                jsonrpc: "2.0",
                id,
                method: &method,
                params,
            };
            single_attempt::<T>(&client, &url, &owned_req, timeout).await
        });
    }

    let mut errs: Vec<SolanaRpcError> = Vec::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Ok(resp)) => {
                set.abort_all();
                return Ok(resp);
            }
            Ok(Err(e)) => errs.push(e),
            Err(_join_err) => {}
        }
    }

    let method = req.method.to_string();
    let detail = if errs.is_empty() {
        "all RPC endpoints failed with no recorded errors".to_string()
    } else {
        let summary: Vec<String> = errs
            .iter()
            .map(|e| format!("{}: {}", e.url.as_deref().unwrap_or("?"), e.detail))
            .collect();
        format!("all RPC endpoints failed: [{}]", summary.join(", "))
    };

    Err(SolanaRpcError::new(
        SolanaRpcErrorKind::Unavailable,
        Some(&method),
        None,
        None,
        None,
        detail,
    )
    .into())
}

pub(super) async fn rpc_batch_race(
    client: &reqwest::Client,
    urls: &[&str],
    reqs: &[RpcRequest<'_>],
) -> Result<Vec<serde_json::Value>> {
    let timeout = std::time::Duration::from_secs(8);

    if urls.len() == 1 {
        return single_batch_attempt(client, urls[0], reqs, timeout).await.map_err(Into::into);
    }

    let owned_reqs: Vec<OwnedRequest> = reqs
        .iter()
        .map(|r| OwnedRequest {
            jsonrpc: r.jsonrpc.to_string(),
            id: r.id,
            method: r.method.to_string(),
            params: r.params.clone(),
        })
        .collect();

    let mut set = tokio::task::JoinSet::new();
    for url in urls {
        let client = client.clone();
        let url = (*url).to_string();
        let owned_reqs = owned_reqs.clone();
        set.spawn(async move {
            let borrowed: Vec<RpcRequest<'_>> = owned_reqs
                .iter()
                .map(|r| RpcRequest {
                    jsonrpc: &r.jsonrpc,
                    id: r.id,
                    method: &r.method,
                    params: r.params.clone(),
                })
                .collect();
            single_batch_attempt(&client, &url, &borrowed, timeout).await
        });
    }

    let mut errs: Vec<SolanaRpcError> = Vec::new();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Ok(results)) => {
                set.abort_all();
                return Ok(results);
            }
            Ok(Err(e)) => errs.push(e),
            Err(_join_err) => {}
        }
    }

    let detail = if errs.is_empty() {
        "all RPC endpoints failed with no recorded errors".to_string()
    } else {
        let summary: Vec<String> = errs
            .iter()
            .map(|e| format!("{}: {}", e.url.as_deref().unwrap_or("?"), e.detail))
            .collect();
        format!("all RPC endpoints failed: [{}]", summary.join(", "))
    };

    Err(SolanaRpcError::new(SolanaRpcErrorKind::Unavailable, None, None, None, None, detail).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;
    use serde_json::json;
    use std::time::Duration;

    #[tokio::test]
    async fn race_ignores_429_when_other_url_succeeds() {
        let bad = MockServer::start_async().await;
        let good = MockServer::start_async().await;

        let _mb = bad.mock_async(|when, then| {
            when.method(POST);
            then.status(429).header("content-type", "application/json").body("rate limited");
        }).await;

        let _mg = good.mock_async(|when, then| {
            when.method(POST);
            then.status(200).header("content-type", "application/json")
                .json_body(json!({ "jsonrpc": "2.0", "id": 1, "result": "ok" }));
        }).await;

        let client = reqwest::Client::new();
        let req = RpcRequest { jsonrpc: "2.0", id: 1, method: "m", params: json!([]) };
        let bad_url = bad.base_url();
        let good_url = good.base_url();
        let urls = vec![bad_url.as_str(), good_url.as_str()];

        let resp: RpcResponse<String> = rpc_call_race(&client, &urls, &req).await.expect("race ok");
        assert_eq!(resp.result.as_deref(), Some("ok"));
    }

    #[tokio::test]
    async fn race_aggregates_errors_when_all_urls_fail() {
        let a = MockServer::start_async().await;
        let b = MockServer::start_async().await;

        let _ma = a.mock_async(|when, then| { when.method(POST); then.status(500); }).await;
        let _mb = b.mock_async(|when, then| { when.method(POST); then.status(500); }).await;

        let client = reqwest::Client::new();
        let req = RpcRequest { jsonrpc: "2.0", id: 1, method: "m", params: json!([]) };
        let a_url = a.base_url();
        let b_url = b.base_url();
        let urls = vec![a_url.as_str(), b_url.as_str()];

        let err = rpc_call_race::<u64>(&client, &urls, &req).await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("all RPC endpoints failed"), "msg={msg}");
        assert!(msg.contains(&a_url), "should list first URL: {msg}");
        assert!(msg.contains(&b_url), "should list second URL: {msg}");
    }

    #[tokio::test]
    async fn race_with_single_url_behaves_like_sequential() {
        let only = MockServer::start_async().await;

        let _m = only.mock_async(|when, then| {
            when.method(POST);
            then.status(200).json_body(json!({ "jsonrpc": "2.0", "id": 1, "result": 7u64 }));
        }).await;

        let client = reqwest::Client::new();
        let req = RpcRequest { jsonrpc: "2.0", id: 1, method: "m", params: json!([]) };
        let only_url = only.base_url();
        let urls = vec![only_url.as_str()];

        let resp: RpcResponse<u64> = rpc_call_race(&client, &urls, &req).await.unwrap();
        assert_eq!(resp.result, Some(7));
    }

    #[tokio::test]
    async fn race_succeeds_when_peer_has_nonretryable_rpc_error() {
        let bad = MockServer::start_async().await;
        let good = MockServer::start_async().await;

        let _mb = bad.mock_async(|when, then| {
            when.method(POST);
            then.status(200).json_body(json!({
                "jsonrpc": "2.0", "id": 1,
                "error": { "code": -32601, "message": "Method not found" }
            }));
        }).await;

        let _mg = good.mock_async(|when, then| {
            when.method(POST);
            then.status(200).json_body(json!({ "jsonrpc": "2.0", "id": 1, "result": 42u64 }));
        }).await;

        let client = reqwest::Client::new();
        let req = RpcRequest { jsonrpc: "2.0", id: 1, method: "m", params: json!([]) };
        let bad_url = bad.base_url();
        let good_url = good.base_url();
        let urls = vec![bad_url.as_str(), good_url.as_str()];

        let resp: RpcResponse<u64> = rpc_call_race(&client, &urls, &req).await.unwrap();
        assert_eq!(resp.result, Some(42));
    }

    #[tokio::test]
    async fn batch_race_returns_fast_url_before_slow() {
        let fast = MockServer::start_async().await;
        let slow = MockServer::start_async().await;

        let batch_body = json!([
            { "jsonrpc": "2.0", "id": 1, "result": "fast-1" },
            { "jsonrpc": "2.0", "id": 2, "result": "fast-2" }
        ]);

        let _mf = fast.mock_async(|when, then| {
            when.method(POST);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(batch_body.clone());
        }).await;

        let _ms = slow.mock_async(|when, then| {
            when.method(POST);
            then.status(200)
                .header("content-type", "application/json")
                .delay(Duration::from_secs(2))
                .json_body(json!([
                    { "jsonrpc": "2.0", "id": 1, "result": "slow-1" },
                    { "jsonrpc": "2.0", "id": 2, "result": "slow-2" }
                ]));
        }).await;

        let client = reqwest::Client::new();
        let reqs = vec![
            RpcRequest { jsonrpc: "2.0", id: 1, method: "m1", params: json!([]) },
            RpcRequest { jsonrpc: "2.0", id: 2, method: "m2", params: json!([]) },
        ];
        let slow_url = slow.base_url();
        let fast_url = fast.base_url();
        let urls = vec![slow_url.as_str(), fast_url.as_str()];

        let start = std::time::Instant::now();
        let out = rpc_batch_race(&client, &urls, &reqs).await.expect("race ok");
        let elapsed = start.elapsed();

        assert_eq!(out.len(), 2, "batch result length");
        assert_eq!(out[0].get("result").and_then(|v| v.as_str()), Some("fast-1"));
        assert_eq!(out[1].get("result").and_then(|v| v.as_str()), Some("fast-2"));
        assert!(elapsed < Duration::from_millis(500),
            "race should return in < 500 ms, took {:?}", elapsed);
    }

    #[tokio::test]
    async fn race_returns_fast_url_before_slow() {
        let fast = MockServer::start_async().await;
        let slow = MockServer::start_async().await;

        let _mf = fast.mock_async(|when, then| {
            when.method(POST);
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({ "jsonrpc": "2.0", "id": 1, "result": 42u64 }));
        }).await;

        let _ms = slow.mock_async(|when, then| {
            when.method(POST);
            then.status(200)
                .header("content-type", "application/json")
                .delay(Duration::from_secs(2))
                .json_body(json!({ "jsonrpc": "2.0", "id": 1, "result": 99u64 }));
        }).await;

        let client = reqwest::Client::new();
        let req = RpcRequest {
            jsonrpc: "2.0",
            id: 1,
            method: "getSomething",
            params: json!([]),
        };
        let slow_url = slow.base_url();
        let fast_url = fast.base_url();
        // Slow first in the URL list — sequential would wait ~2 s for it.
        let urls = vec![slow_url.as_str(), fast_url.as_str()];

        let start = std::time::Instant::now();
        let resp: RpcResponse<u64> = rpc_call_race(&client, &urls, &req).await.expect("race ok");
        let elapsed = start.elapsed();

        assert_eq!(resp.result, Some(42), "fast URL's result (42) should win");
        assert!(elapsed < Duration::from_millis(500),
            "race should return in < 500 ms, took {:?}", elapsed);
    }
}
