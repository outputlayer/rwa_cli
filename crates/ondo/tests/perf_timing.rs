//! End-to-end timing of the Jupiter `/order` retry path against a mock server.
//!
//! This is the real-time companion to the fast unit tests in `jupiter::order`
//! (`order_backoff_is_exponential`, `retryable_order_errors_are_classified`).
//! It actually waits out the exponential backoff (~8.8 s), so it is `#[ignore]`d
//! by default. Run explicitly:
//!
//!   cargo test -p rwa-ondo --test perf_timing -- --ignored --nocapture

use httpmock::prelude::*;
use std::time::{Duration, Instant};

use rwa_ondo::{USDC_MINT, jupiter};

const OUT_MINT: &str = "gEGtLTPNQ7jcg25zTetkbmF7teoDLcrfTnQfmn2ondo";
const TAKER: &str = "5CjgV1J2FE8yyxsHKGs2v4GJULBS7AiYtRo7DFYiuZ47";

/// A persistently failing retryable error (HTTP 500) is retried the full
/// `ORDER_MAX_RETRIES` (5 attempts total) and the cumulative backoff
/// (0.8 + 1.6 + 3.2 + 3.2 = 8.8 s, capped at 3.2s per wait) actually elapses —
/// well inside the ~20s retry budget, so the budget never trips here.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "waits out ~8.8s of real backoff; run with --ignored"]
async fn order_retries_retryable_error_with_backoff() {
    let server = MockServer::start_async().await;
    let m = server
        .mock_async(|when, then| {
            when.method(GET).path("/order");
            then.status(500).body("upstream server error");
        })
        .await;

    let start = Instant::now();
    let res = jupiter::get_order(
        Some(&server.base_url()),
        USDC_MINT,
        OUT_MINT,
        "1000000",
        TAKER,
        Some(100),
    )
    .await;
    let elapsed = start.elapsed();

    assert!(res.is_err(), "persistent 500 must ultimately fail");
    assert_eq!(m.hits_async().await, 5, "should retry ORDER_MAX_RETRIES then stop");
    assert!(
        elapsed >= Duration::from_millis(8_300),
        "cumulative backoff should be ~8.8s, got {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(15_000),
        "backoff should not run far past ~8.8s, got {elapsed:?}"
    );
}

/// A non-retryable error (HTTP 400) fails fast on the first attempt — no backoff.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn order_does_not_retry_hard_error() {
    let server = MockServer::start_async().await;
    let m = server
        .mock_async(|when, then| {
            when.method(GET).path("/order");
            then.status(400).body("bad request: malformed input");
        })
        .await;

    let start = Instant::now();
    let res = jupiter::get_order(
        Some(&server.base_url()),
        USDC_MINT,
        OUT_MINT,
        "1000000",
        TAKER,
        Some(100),
    )
    .await;
    let elapsed = start.elapsed();

    assert!(res.is_err(), "400 must fail");
    assert_eq!(m.hits_async().await, 1, "hard error must not be retried");
    assert!(
        elapsed < Duration::from_millis(2_000),
        "no backoff for a hard error, got {elapsed:?}"
    );
}
