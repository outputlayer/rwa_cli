//! `/order` flow: fetch a swap quote.
//!
//! Public entry is `get_order`. Without an explicit `base_url` it tries the
//! public Jupiter backends in priority order (SwapV2 lite → Ultra → UltraLite),
//! falling back to Metis V1 only on route-not-found errors. Each attempt is
//! gated by `ORDER_SEMAPHORE` (concurrency 2) to avoid Jupiter's per-wallet
//! routing conflicts, and retried with exponential backoff on transient
//! failures.

use eyre::{eyre, Result};

use crate::HTTP;

use super::{
    types::{ExecuteFailure, ExecuteFailureKind, OrderBackend, OrderResponse},
    with_jupiter_headers, METIS_LITE_API_BASE, ORDER_SEMAPHORE,
};

/// Maximum retries for transient errors on /order.
const ORDER_MAX_RETRIES: u32 = 4;

/// Get a swap quote from public Jupiter APIs.
/// Flow: /order -> wallet.sign -> /execute.
pub async fn get_order(
    base_url: Option<&str>,
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
) -> Result<OrderResponse> {
    get_order_impl(base_url, input_mint, output_mint, amount, taker, slippage_bps, &[]).await
}

/// Like [`get_order`], but asks Jupiter to avoid the given routers (by their
/// `router` label, e.g. `jupiterz`). Used to route around a quote that passed
/// quoting but would fail on-chain (an RFQ market maker that can't fill).
pub async fn get_order_excluding(
    base_url: Option<&str>,
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
    excluded_routers: &[String],
) -> Result<OrderResponse> {
    get_order_impl(base_url, input_mint, output_mint, amount, taker, slippage_bps, excluded_routers).await
}

async fn get_order_impl(
    base_url: Option<&str>,
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
    excluded_routers: &[String],
) -> Result<OrderResponse> {
    // `RWA_JUPITER_URL` pins all quoting to one base URL (test seam — not user-facing).
    let env_url = std::env::var("RWA_JUPITER_URL")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let base_url = base_url.or(env_url.as_deref());
    if let Some(base_url) = base_url {
        let backend = infer_backend_from_base_url(base_url);
        // Metis speaks /quote + /swap, not the managed /order endpoint —
        // dispatch a pinned Metis URL through the right flow.
        if backend == OrderBackend::MetisV1Lite {
            return get_metis_order(base_url, input_mint, output_mint, amount, taker, slippage_bps)
                .await;
        }
        return get_order_with_retries(
            base_url,
            backend,
            input_mint,
            output_mint,
            amount,
            taker,
            slippage_bps,
            excluded_routers,
        )
        .await;
    }

    let mut failures = Vec::new();
    let backends = [
        OrderBackend::SwapV2Lite,
        OrderBackend::Ultra,
        OrderBackend::UltraLite,
    ];

    for backend in backends {
        match get_order_with_retries(
            backend.base_url(),
            backend,
            input_mint,
            output_mint,
            amount,
            taker,
            slippage_bps,
            excluded_routers,
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

    match get_metis_order(METIS_LITE_API_BASE, input_mint, output_mint, amount, taker, slippage_bps).await {
        Ok(order) => Ok(order),
        Err(err) => {
            failures.push(format!("{}: {}", OrderBackend::MetisV1Lite.label(), err));
            // Every backend (RFQ MMs and the Metis AMM tail) declined this
            // order — a market/size condition, not a transport failure.
            // Surface the stable `route_unfillable` kind for agents.
            Err(ExecuteFailure {
                kind: ExecuteFailureKind::RouteUnfillable,
                code: None,
                message: format!(
                    "No swap route found across public Jupiter backends: {}",
                    failures.join(" | ")
                ),
            }
            .into())
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn get_order_with_retries(
    base_url: &str,
    backend: OrderBackend,
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
    excluded_routers: &[String],
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
                excluded_routers,
            )
            .await
        }; // permit released here

        match result {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                let msg = format!("{e}");
                if attempt < ORDER_MAX_RETRIES && is_retryable_order_error(&msg) {
                    let backoff = order_backoff(attempt);
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
/// Combine the routers the retry loop excluded with any pinned via
/// `RWA_EXCLUDE_ROUTERS` (comma-separated). Pure (env value passed in) for
/// testability; deduped, blanks trimmed, order preserved.
fn merge_excluded_routers(passed: &[String], env_raw: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = passed.to_vec();
    if let Some(raw) = env_raw {
        for r in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if !out.iter().any(|x| x == r) {
                out.push(r.to_string());
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
async fn get_order_inner(
    order_url: &str,
    backend: OrderBackend,
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
    excluded_routers: &[String],
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
    // Jupiter swap/v2 accepts a comma-separated `excludeRouters` list: avoid a
    // router whose quote would fail on-chain (e.g. an RFQ MM that can't fill) so
    // a fillable one (metis, dflow, …) is chosen. The list combines routers the
    // retry loop excluded with any pinned via `RWA_EXCLUDE_ROUTERS` (a manual
    // escape hatch for a persistently bad router).
    let excluded = merge_excluded_routers(excluded_routers, std::env::var("RWA_EXCLUDE_ROUTERS").ok().as_deref());
    if !excluded.is_empty() {
        request = request.query(&[("excludeRouters", excluded.join(","))]);
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

/// Exponential backoff before the `attempt`-th `/order` retry: 800ms · 2^attempt
/// (0.8s, 1.6s, 3.2s, 6.4s …).
fn order_backoff(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_millis(800 * 2u64.pow(attempt))
}

/// Errors from Jupiter /order that should be retried with backoff.
/// Covers transient hiccups, rate limits, server errors, and network issues.
/// "Quote not available from market maker" is deliberately NOT here: it is a
/// size/market condition that rarely resolves within the backoff window —
/// falling through to the next backend (see below) is both faster and more
/// likely to fill.
fn is_retryable_order_error(msg: &str) -> bool {
    msg.contains("Something unexpected occurred")
        || msg.contains("Winning quote has no transaction")
        || msg.contains("/order network error")
        || msg.contains("/order rate limited")
        || msg.contains("/order server error")
}

/// Errors that mean "this backend can't fill the order" — continue down the
/// fallback chain (Ultra → UltraLite → Metis AMM) instead of hard-failing.
fn is_route_like_order_error(msg: &str) -> bool {
    msg.contains("No swap route found")
        || msg.contains("Failed to get quotes")
        || msg.contains("COULD_NOT_FIND_ANY_ROUTE")
        || msg.contains("TOKEN_NOT_TRADABLE")
        || msg.contains("not available from market maker")
}

pub(super) fn compact_error_body(body: &str) -> String {
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
    } else if base_url.contains("/ultra/v1") {
        OrderBackend::UltraLite
    } else {
        OrderBackend::Ultra
    }
}

async fn get_metis_order(
    base_url: &str,
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
) -> Result<OrderResponse> {
    let quote_url = format!("{base_url}/quote");
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
    request = with_jupiter_headers(request);

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

    let swap_url = format!("{base_url}/swap");
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
    let swap_request = with_jupiter_headers(HTTP.post(&swap_url).json(&swap_body));
    let swap_response = swap_request
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::USDC_MINT;

    #[test]
    fn mm_quote_unavailable_falls_through_not_retried() {
        // Size-based MM rejection: don't burn backoff retries on the same
        // backend, do continue down the fallback chain.
        let msg = "Jupiter API error (HTTP 400 Bad Request): Quote not available from market maker";
        assert!(!is_retryable_order_error(msg));
        assert!(is_route_like_order_error(msg));
        // Transport errors stay retryable and are not route-like.
        assert!(is_retryable_order_error("/order network error: timeout"));
        assert!(!is_route_like_order_error("/order network error: timeout"));
    }

    #[test]
    fn order_backoff_is_exponential() {
        use std::time::Duration;
        assert_eq!(order_backoff(0), Duration::from_millis(800));
        assert_eq!(order_backoff(1), Duration::from_millis(1_600));
        assert_eq!(order_backoff(2), Duration::from_millis(3_200));
        assert_eq!(order_backoff(3), Duration::from_millis(6_400));
        // Sum of the 4 waits before giving up after ORDER_MAX_RETRIES.
        let total: u128 = (0..ORDER_MAX_RETRIES).map(|a| order_backoff(a).as_millis()).sum();
        assert_eq!(total, 12_000);
    }

    #[test]
    fn retryable_order_errors_are_classified() {
        // Transient → retry with backoff.
        assert!(is_retryable_order_error("Jupiter /order server error (500)"));
        assert!(is_retryable_order_error("Jupiter /order rate limited (429)"));
        assert!(is_retryable_order_error("Jupiter /order network error: connection reset"));
        assert!(is_retryable_order_error("Winning quote has no transaction"));
        // MM size rejection → not retried in place, falls through to the
        // next backend instead (see mm_quote_unavailable_falls_through_not_retried).
        assert!(!is_retryable_order_error("not available from market maker"));
        // Hard → fail fast, no retry.
        assert!(!is_retryable_order_error("Jupiter API error (HTTP 400): bad input"));
        assert!(!is_retryable_order_error("No swap route found — TOKEN_NOT_TRADABLE"));
    }

    #[test]
    fn route_like_errors_are_classified() {
        // Route-not-found → fall through to the next backend, not a hard stop.
        assert!(is_route_like_order_error("No swap route found — x"));
        assert!(is_route_like_order_error("Failed to get quotes"));
        assert!(is_route_like_order_error("COULD_NOT_FIND_ANY_ROUTE"));
        assert!(is_route_like_order_error("TOKEN_NOT_TRADABLE"));
        assert!(!is_route_like_order_error("Jupiter /order server error (500)"));
    }

    #[test]
    fn merge_excluded_routers_combines_and_dedups() {
        // retry-excluded only
        assert_eq!(
            merge_excluded_routers(&["jupiterz".to_string()], None),
            vec!["jupiterz"]
        );
        // env adds, trims blanks, dedups against passed
        assert_eq!(
            merge_excluded_routers(&["jupiterz".to_string()], Some(" jupiterz , dflow ,")),
            vec!["jupiterz", "dflow"]
        );
        // env only
        assert_eq!(
            merge_excluded_routers(&[], Some("okx")),
            vec!["okx"]
        );
        // nothing
        assert!(merge_excluded_routers(&[], None).is_empty());
        assert!(merge_excluded_routers(&[], Some("   ")).is_empty());
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

    #[tokio::test]
    async fn metis_flow_builds_order_from_quote_and_swap() {
        // A base URL containing /swap/v1 must go through the Metis flow:
        // GET {base}/quote then POST {base}/swap, assembled into an
        // OrderResponse with backend MetisV1Lite and the built transaction.
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        server.mock_async(|when, then| {
            when.method(GET).path("/swap/v1/quote");
            then.status(200).json_body(serde_json::json!({
                "inAmount": "5000000",
                "outAmount": "11786465",
                "priceImpactPct": "-0.11",
                "slippageBps": 50
            }));
        }).await;
        let swap = server.mock_async(|when, then| {
            when.method(POST)
                .path("/swap/v1/swap")
                // The swap build must echo the full quote back to Jupiter.
                .body_contains("\"quoteResponse\"")
                .body_contains("\"outAmount\":\"11786465\"");
            then.status(200).json_body(serde_json::json!({
                "swapTransaction": "AQAAAA==",
                "lastValidBlockHeight": 12345,
                "prioritizationFeeLamports": 777
            }));
        }).await;

        let base = format!("{}/swap/v1", server.base_url());
        let order = get_order(
            Some(&base),
            USDC_MINT,
            "So11111111111111111111111111111112",
            "5000000",
            "FakeWallet111111111111111111111111111111",
            Some(50),
        ).await.unwrap();

        swap.assert_hits_async(1).await;
        assert_eq!(order.backend, OrderBackend::MetisV1Lite);
        assert_eq!(order.in_amount, "5000000");
        assert_eq!(order.out_amount, "11786465");
        assert_eq!(order.transaction, Some("AQAAAA==".to_string()));
        assert_eq!(order.slippage_bps, Some(50));
        assert_eq!(order.router, Some("metis-v1-lite".to_string()));
        assert_eq!(order.last_valid_block_height, Some("12345".to_string()));
        assert_eq!(order.gasless, Some(false), "Metis path is never gasless");
    }

    #[tokio::test]
    async fn metis_flow_surfaces_no_routes_error() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let swap = server.mock_async(|when, then| {
            when.method(POST).path("/swap/v1/swap");
            then.status(200).json_body(serde_json::json!({ "swapTransaction": "AQAAAA==" }));
        }).await;
        server.mock_async(|when, then| {
            when.method(GET).path("/swap/v1/quote");
            then.status(400).json_body(serde_json::json!({
                "error": "No routes found", "errorCode": "NO_ROUTES_FOUND"
            }));
        }).await;

        let base = format!("{}/swap/v1", server.base_url());
        let err = get_order(
            Some(&base),
            USDC_MINT,
            "So11111111111111111111111111111112",
            "1000000",
            "FakeWallet111111111111111111111111111111",
            None,
        ).await.unwrap_err();

        assert!(err.to_string().contains("Metis quote error"), "err: {err}");
        assert!(err.to_string().contains("NO_ROUTES_FOUND"), "err: {err}");
        swap.assert_hits_async(0).await;
    }

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
    async fn mm_quote_rejection_is_not_retried_against_same_backend() {
        // Behavior contract (not string constants): a 400 "Quote not available
        // from market maker" must produce exactly ONE HTTP attempt — no backoff
        // retries against the same backend — so the fallback chain can move on
        // fast. Verified by the mock's hit counter.
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let mock = server.mock_async(|when, then| {
            when.method(GET).path("/order");
            then.status(400)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({
                    "requestId": "r-1",
                    "error": "Quote not available from market maker"
                }));
        }).await;

        let err = get_order(
            Some(&server.base_url()),
            USDC_MINT,
            "So11111111111111111111111111111112",
            "1000000",
            "FakeWallet111111111111111111111111111111",
            Some(100),
        ).await.unwrap_err();

        mock.assert_hits_async(1).await;
        assert!(err.to_string().contains("Quote not available"), "err: {err}");
    }

    #[tokio::test]
    async fn transient_server_error_is_retried_with_backoff() {
        // Counterpart contract: a 5xx IS retried (ORDER_MAX_RETRIES + 1
        // attempts) before giving up.
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let mock = server.mock_async(|when, then| {
            when.method(GET).path("/order");
            then.status(500).body("upstream exploded");
        }).await;

        let err = get_order(
            Some(&server.base_url()),
            USDC_MINT,
            "So11111111111111111111111111111112",
            "1000000",
            "FakeWallet111111111111111111111111111111",
            Some(100),
        ).await.unwrap_err();

        mock.assert_hits_async((ORDER_MAX_RETRIES + 1) as usize).await;
        assert!(err.to_string().contains("server error"), "err: {err}");
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
