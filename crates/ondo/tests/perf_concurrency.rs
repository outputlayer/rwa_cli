//! Concurrency-limit timing for Jupiter `/order`.
//!
//! `ORDER_SEMAPHORE` caps concurrent `/order` calls at 2 (Jupiter rejects too
//! many parallel quotes from one wallet). With a fixed server-side delay `D`,
//! four concurrent calls must complete in ~2·D (two batches) — not ~D (all at
//! once) and not ~4·D (fully serial). Own test binary so the process-global
//! semaphore isn't contended by unrelated tests.

use httpmock::prelude::*;
use std::time::{Duration, Instant};

use rwa_ondo::{USDC_MINT, jupiter};

const OUT_MINT: &str = "gEGtLTPNQ7jcg25zTetkbmF7teoDLcrfTnQfmn2ondo";
const TAKER: &str = "5CjgV1J2FE8yyxsHKGs2v4GJULBS7AiYtRo7DFYiuZ47";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn order_semaphore_limits_concurrency_to_two() {
    // D is deliberately large so the batch signal dominates scheduler/IO
    // overhead: expected 2-batch time is ~2·D=1000ms, all-parallel ~D=500ms,
    // fully-serial ~4·D=2000ms. The wide bounds below (800/1600) leave ~600ms
    // of overhead headroom before either can trip, so a loaded runner can't
    // flake this — the lower bound only fails if the semaphore stops limiting,
    // the upper only if it goes fully serial.
    const D: Duration = Duration::from_millis(500);
    let server = MockServer::start_async().await;
    // 400 is a hard error → exactly one request per call, no retry/backoff to
    // confound the timing. The delay holds the semaphore permit for ~D.
    let m = server
        .mock_async(|when, then| {
            when.method(GET).path("/order");
            then.status(400).delay(D).body("bad request");
        })
        .await;

    let url = server.base_url();
    let call = || jupiter::get_order(Some(&url), USDC_MINT, OUT_MINT, "1000000", TAKER, Some(100));

    let start = Instant::now();
    let (_a, _b, _c, _d) = tokio::join!(call(), call(), call(), call());
    let elapsed = start.elapsed();

    assert_eq!(m.hits_async().await, 4, "each call makes exactly one request");
    // ~2·D = 1000ms. Reject the all-parallel (~500ms) and fully-serial (~2000ms)
    // cases with generous margins for scheduler/IO overhead.
    assert!(
        elapsed >= Duration::from_millis(800),
        "semaphore=2 must serialize beyond 2 in flight; got {elapsed:?} (too fast → no limit)"
    );
    assert!(
        elapsed < Duration::from_millis(1_600),
        "semaphore=2 should run in ~2 batches, not fully serial; got {elapsed:?}"
    );
}
