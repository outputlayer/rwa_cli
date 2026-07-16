//! Cross-cutting helpers shared by every gm subcommand.
//!
//! Includes wallet loading (with passphrase prompting and one-shot env-var
//! warning), JSON output, interactive y/N confirmation, mint resolution from
//! the static token list, the shared multi-item swap orchestrators used by
//! close-all and both baskets, and small string utilities used by the
//! list/search flows.

use eyre::Result;
use rwa_ondo::usecases::gm::{GmTradeError, GmTradeErrorKind};
use rwa_ondo::{jupiter, usecases, wallet};
use serde::Serialize;
use std::future::Future;
use std::io::{self, Write};
use std::time::Duration;
use tokio::task::JoinSet;

use super::types::{CloseFailJson, GasRefuelJson};

/// Inter-item pacing for `--sequential` mode (Jupiter rate-limit
/// conservatism). Shared by the real execution path (`run_swap_items`) and
/// the dry-run quote-fetch path (`fetch_orders_sequential`) so `--sequential`
/// paces identically whether or not a trade actually executes.
pub(super) const SEQUENTIAL_SPACING: Duration = Duration::from_secs(3);

/// Default delay between launching each PARALLEL quote. Firing a basket's quotes
/// all at once bursts Jupiter's per-wallet rate limit, whose 429 → retry backoff
/// (0.8/1.6/3.2s) costs far more than a small stagger saves. A short stagger
/// keeps the overlap (fast) while dodging the burst (stable). Tunable via
/// `RWA_QUOTE_STAGGER_MS`; an API key (`RWA_JUPITER_API_KEY`) raises the limit so
/// it can be lowered toward 0.
const DEFAULT_QUOTE_STAGGER_MS: u64 = 350;

/// Parse the parallel-quote stagger: an explicit `RWA_QUOTE_STAGGER_MS` always
/// wins; otherwise a configured `RWA_JUPITER_API_KEY` selects the keyed profile
/// (0ms — the key raises the per-wallet limit, so the burst-dodging stagger
/// only costs latency); keyless falls back to the default.
fn quote_stagger_from(raw: Option<&str>, has_api_key: bool) -> Duration {
    if let Some(ms) = raw.and_then(|v| v.trim().parse::<u64>().ok()) {
        return Duration::from_millis(ms);
    }
    if has_api_key {
        return Duration::ZERO;
    }
    Duration::from_millis(DEFAULT_QUOTE_STAGGER_MS)
}

/// The parallel-quote stagger, honouring `RWA_QUOTE_STAGGER_MS` and the
/// API-key auto-profile.
fn quote_stagger() -> Duration {
    quote_stagger_from(
        std::env::var("RWA_QUOTE_STAGGER_MS").ok().as_deref(),
        jupiter::has_api_key(),
    )
}

/// Gap before launching the next parallel quote. Normally the small `stagger`;
/// once Jupiter has started pushing back (retries seen), widen to the
/// rate-limit-safe `SEQUENTIAL_SPACING` so we stop feeding the burst — a
/// keyless-friendly adaptive back-off (fast when the endpoint is generous,
/// graceful serial pace when it throttles).
fn launch_gap(throttled: bool, stagger: Duration) -> Duration {
    if throttled { SEQUENTIAL_SPACING } else { stagger }
}

/// Whether the auto-gas refuel may proceed without an interactive prompt.
/// JSON mode is non-interactive: it auto-approves ONLY under explicit `-y`
/// (the same rule as trade execution since v0.6.0). Returns `None` when an
/// interactive prompt is required.
fn gas_auto_consent(yes: bool, json: bool) -> Option<bool> {
    match (yes, json) {
        (true, _) => Some(true),      // -y covers the refuel too
        (false, true) => Some(false), // json without -y: skip refuel, never prompt
        (false, false) => None,       // interactive: ask
    }
}

/// SOL auto-refuel gate for money commands: when SOL is below the low-water
/// mark and USDC covers the operation plus 5 USDC, buy SOL first so a
/// USDC-only wallet never strands itself. Consent is an interactive prompt;
/// `-y` auto-approves; `--json` without `-y` silently skips the refuel
/// (consent parity with `require_execution_consent`); `RWA_NO_AUTO_GAS`
/// disables entirely. Best-effort: never fails the caller's operation.
pub(super) async fn auto_gas(
    w: &wallet::Wallet,
    rpc_url: Option<&str>,
    yes: bool,
    json: bool,
    reserved_usdc_raw: u128,
) -> Result<(Option<GasRefuelJson>, Option<usecases::gm::BalanceSnapshot>)> {
    let disabled = std::env::var("RWA_NO_AUTO_GAS")
        .is_ok_and(|v| !v.trim().is_empty() && v.trim() != "0");
    if disabled {
        return Ok((None, None));
    }
    let (refuel, snapshot) = usecases::gm::ensure_gas(w, rpc_url, json, reserved_usdc_raw, |sol| {
        gas_auto_consent(yes, json).unwrap_or_else(|| {
            // Honest range: sizing is dynamic AFTER consent, clamped to
            // [5, 25] USDC — never promise the floor when the ceiling may hit.
            confirm(&format!(
                "SOL is low ({sol:.6}) — buy 5–25 USDC of SOL (sized to current fees) first?"
            ))
        })
    })
    .await?;
    let refuel_json = refuel.map(|r| {
        let tx = solscan_tx_url(&r.signature);
        eprintln!("Gas refuel: {} USDC -> {} SOL  tx: {tx}", r.usdc_spent, r.sol_received);
        GasRefuelJson {
            usdc: r.usdc_spent,
            sol: r.sol_received,
            tx,
        }
    });
    Ok((refuel_json, snapshot))
}

/// Orchestrate multi-item swaps for close-all and both baskets — the single
/// place that owns sequential-vs-parallel semantics, so they can't drift
/// between commands.
///
/// Sequential (`parallel=false`) awaits each item with a 3-second inter-item
/// delay (Jupiter rate-limit conservatism) and prints `announce` before each.
/// Parallel (`parallel=true`) spawns a JoinSet with the SAME staggered,
/// adaptive launch pacing as the dry-run quote fetcher (`retry_count` is the
/// injected `/order`-retry reader): each item's chain starts a quote, so
/// bursting them all at once feeds Jupiter's per-wallet 429 exactly like a
/// quote burst does — the retry backoff (0.8/1.6/3.2s) costs far more than
/// the stagger. Swap-layer concurrency stays bounded by the Jupiter
/// semaphores inside `execute_*`.
///
/// `process` does fetch + execute for one item and returns the JSON entry plus
/// its USDC delta, or a `CloseFailJson`. Returns (succeeded, failed, total).
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_swap_items<T, J, F, Fut>(
    items: Vec<T>,
    parallel: bool,
    json: bool,
    parallel_noun: &str,
    retry_count: impl Fn() -> u64,
    announce: impl Fn(&T) -> String,
    process: F,
) -> (Vec<J>, Vec<CloseFailJson>, f64)
where
    T: Send + 'static,
    J: Send + 'static,
    F: Fn(T) -> Fut,
    Fut: Future<Output = std::result::Result<(J, f64), CloseFailJson>> + Send + 'static,
{
    let mut done: Vec<J> = Vec::new();
    let mut failed: Vec<CloseFailJson> = Vec::new();
    let mut total: f64 = 0.0;

    if parallel {
        if !json {
            println!("Processing {} {} in parallel...", items.len(), parallel_noun);
        }
        let stagger = quote_stagger();
        let retries_baseline = retry_count();
        let mut joinset: JoinSet<std::result::Result<(J, f64), CloseFailJson>> = JoinSet::new();
        for (i, item) in items.into_iter().enumerate() {
            if i > 0 {
                let throttled = retry_count() > retries_baseline;
                let gap = launch_gap(throttled, stagger);
                if !gap.is_zero() {
                    tokio::time::sleep(gap).await;
                }
            }
            joinset.spawn(process(item));
        }
        while let Some(res) = joinset.join_next().await {
            match res {
                Ok(Ok((item, value))) => {
                    done.push(item);
                    total += value;
                }
                Ok(Err(fail)) => failed.push(fail),
                Err(e) => {
                    if !json {
                        eprintln!("  ✗ join error: {}", e);
                    }
                    // A task panic/cancel yields no item, so the symbol is
                    // unknown — but a money op that may have executed must still
                    // land in the report. A silent drop reads to a --json
                    // consumer (which gates on failed[]/exit) as full success.
                    failed.push(CloseFailJson {
                        token: "unknown".to_string(),
                        error: format!("task did not complete: {e}"),
                        error_kind: None,
                    });
                }
            }
        }
    } else {
        for (i, item) in items.into_iter().enumerate() {
            if i > 0 {
                tokio::time::sleep(SEQUENTIAL_SPACING).await;
            }
            if !json {
                println!("{}", announce(&item));
            }
            match process(item).await {
                Ok((item, value)) => {
                    done.push(item);
                    total += value;
                }
                Err(fail) => failed.push(fail),
            }
        }
    }

    (done, failed, total)
}

/// Fetch quotes for many items in parallel (dry-run phase shared by both
/// baskets): spawn one fetch per item, print `✓ SYM — <describe_ok>` /
/// `✗ SYM — <error>` lines, and split results into (ready, failed).
pub(super) async fn fetch_orders_parallel<I, T, F, Fut>(
    items: Vec<I>,
    json: bool,
    parallel_noun: &str,
    retry_count: impl Fn() -> u64,
    describe_ok: impl Fn(&T) -> String,
    fetch: F,
) -> (Vec<T>, Vec<CloseFailJson>)
where
    I: Send + 'static,
    T: Send + 'static,
    F: Fn(I) -> Fut,
    Fut: Future<Output = (String, Result<T>)> + Send + 'static,
{
    if !json {
        println!("Processing {} {} in parallel...", items.len(), parallel_noun);
    }

    // Stagger the launches: firing every quote at once bursts Jupiter's
    // per-wallet rate limit, whose retry backoff costs far more than the stagger.
    // A small gap keeps the overlap while dodging the burst — AND once Jupiter
    // starts pushing back (retries climb past the baseline), widen the gap to a
    // serial pace so we stop feeding the burst. Adaptive to whatever per-wallet
    // limit the user has, so a keyless public wallet gets the best of both.
    //
    // `retry_count` is injected: production wires `jupiter::order_retry_count`
    // (the process-wide `/order` retry odometer that the quote-fetch retry site
    // increments), while tests pass a local counter — so the adaptive-pacing
    // behaviour is exercised deterministically without a process-global leaking
    // between concurrent tests.
    // Trade-off (perf only): the counter counts any retryable /order pushback, so
    // a single non-rate-limit transient on an early item latches the wider pace
    // for the rest of this basket. Acceptable — it only slows launches, never
    // money movement (execution uses a fixed pace and ignores this counter).
    let stagger = quote_stagger();
    let retries_baseline = retry_count();
    let mut order_set: JoinSet<(String, Result<T>)> = JoinSet::new();
    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            let throttled = retry_count() > retries_baseline;
            let gap = launch_gap(throttled, stagger);
            if !gap.is_zero() {
                tokio::time::sleep(gap).await;
            }
        }
        order_set.spawn(fetch(item));
    }

    let mut ready: Vec<T> = Vec::new();
    let mut failed: Vec<CloseFailJson> = Vec::new();
    while let Some(res) = order_set.join_next().await {
        match res {
            Ok((sym, Ok(order))) => {
                if !json {
                    println!("  ✓ {} — {}", sym, describe_ok(&order));
                }
                ready.push(order);
            }
            Ok((sym, Err(e))) => {
                if !json {
                    eprintln!("  ✗ {} — {}", sym, e);
                }
                failed.push(fail_json(sym, &e));
            }
            Err(e) => {
                if !json {
                    eprintln!("  ✗ join error: {}", e);
                }
            }
        }
    }

    (ready, failed)
}

/// Fetch quotes for many items one at a time (the `--sequential` counterpart
/// to `fetch_orders_parallel`), pacing each item `SEQUENTIAL_SPACING` apart —
/// the same rate-limit-conservatism gap the real execution path uses in
/// `run_swap_items`. Same ready/failed split and `✓`/`✗` line format as the
/// parallel version, so dry-run output shape doesn't depend on `--sequential`.
pub(super) async fn fetch_orders_sequential<I, T, F, Fut>(
    items: Vec<I>,
    json: bool,
    describe_ok: impl Fn(&T) -> String,
    fetch: F,
) -> (Vec<T>, Vec<CloseFailJson>)
where
    F: Fn(I) -> Fut,
    Fut: Future<Output = (String, Result<T>)>,
{
    let mut ready: Vec<T> = Vec::new();
    let mut failed: Vec<CloseFailJson> = Vec::new();

    for (i, item) in items.into_iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(SEQUENTIAL_SPACING).await;
        }
        let (sym, result) = fetch(item).await;
        match result {
            Ok(order) => {
                if !json {
                    println!("  ✓ {} — {}", sym, describe_ok(&order));
                }
                ready.push(order);
            }
            Err(e) => {
                if !json {
                    eprintln!("  ✗ {} — {}", sym, e);
                }
                failed.push(fail_json(sym, &e));
            }
        }
    }

    (ready, failed)
}

pub(super) fn solscan_tx_url(sig: &str) -> String {
    format!("https://solscan.io/tx/{sig}")
}

/// Build a CloseFailJson from a token symbol and an eyre error.
/// Populates `error_kind` when the error is a known structured type.
pub(super) fn fail_json(token: String, err: &eyre::Error) -> CloseFailJson {
    CloseFailJson {
        token,
        error: err.to_string(),
        error_kind: usecases::gm::classify_error(err),
    }
}

pub(super) fn json_out(v: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(v)?);
    Ok(())
}

pub(super) fn confirm(msg: &str) -> bool {
    print!("{msg} [y/N] ");
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Execution consent gate — single chokepoint for every money-moving command.
/// `-y` always proceeds. JSON mode is non-interactive: without `-y` it fails
/// closed (typed `confirmation_required`) instead of silently executing.
/// Interactive mode falls back to the y/N prompt (untestable headlessly —
/// covered by existing manual behavior).
pub(crate) fn require_execution_consent(yes: bool, json: bool, prompt: &str) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    if json {
        return Err(GmTradeError::new(
            GmTradeErrorKind::ConfirmationRequired,
            "--json is non-interactive: add -y to execute, or use --dry-run to preview",
        )
        .into());
    }
    Ok(confirm(prompt))
}

/// Load the wallet selected for this invocation (`--wallet`/`RWA_WALLET` >
/// active > legacy default). Single chokepoint for every signing command.
pub(super) fn load_wallet(selected: Option<&str>) -> Result<wallet::Wallet> {
    crate::wallets::load_selected(selected)
}

/// Like `load_wallet`, but also returns the wallet's send policy (`None` for
/// plaintext wallets or encrypted wallets with no policy embedded). Used only
/// by `send`, which is the sole command that enforces the allowed-recipient
/// gate.
pub(super) fn load_wallet_with_policy(
    selected: Option<&str>,
) -> Result<(wallet::Wallet, Option<wallet::SendPolicy>)> {
    crate::wallets::load_selected_with_policy(selected)
}

// One canonical symbol->mint resolution path for the whole workspace.
pub(super) use usecases::gm::resolve_gm_mint;

// Shared "insufficient balance" message (wallet-displayed note included) —
// one wording for both `sell` and `send` overshoot errors.
pub(super) use usecases::gm::insufficient_balance_message;

pub(super) fn clean_name(name: &str) -> String {
    name.replace(" (Ondo Tokenized)", "")
}

pub(super) fn token_type_from_name(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if n.contains("etf")
        || n.contains(" fund")
        || n.contains(" trust")
        || n.contains(" index")
        || n.contains(" shares")
    {
        "etf"
    } else {
        "stock"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_name_removes_suffix() {
        assert_eq!(clean_name("Tesla (Ondo Tokenized)"), "Tesla");
    }

    #[test]
    fn clean_name_no_suffix() {
        assert_eq!(clean_name("Apple"), "Apple");
    }

    #[test]
    fn detect_etf() {
        assert_eq!(token_type_from_name("SPDR S&P 500 ETF Trust"), "etf");
        assert_eq!(
            token_type_from_name("Vanguard Total Stock Market Index Fund"),
            "etf"
        );
    }

    #[test]
    fn gas_auto_consent_yes_always_approves() {
        // -y covers the refuel regardless of json.
        assert_eq!(gas_auto_consent(true, true), Some(true));
        assert_eq!(gas_auto_consent(true, false), Some(true));
    }

    #[test]
    fn gas_auto_consent_json_without_yes_skips_silently() {
        // json without -y must never prompt and must never auto-approve —
        // this is the fix: previously `yes || json` auto-approved here.
        assert_eq!(gas_auto_consent(false, true), Some(false));
    }

    #[test]
    fn gas_auto_consent_interactive_requires_prompt() {
        // Neither -y nor json: caller must fall back to the interactive prompt.
        assert_eq!(gas_auto_consent(false, false), None);
    }

    #[test]
    fn consent_gate_json_without_yes_is_confirmation_required() {
        // -y always proceeds, json or not.
        assert!(require_execution_consent(true, true, "Proceed?").unwrap());
        assert!(require_execution_consent(true, false, "Proceed?").unwrap());
        // json without -y fails closed with the typed kind, before any execution.
        let err = require_execution_consent(false, true, "Proceed?").unwrap_err();
        assert_eq!(rwa_ondo::usecases::gm::classify_error(&err), Some("confirmation_required"));
        let msg = format!("{err:#}");
        assert!(msg.contains("-y"), "teaches the fix: {msg}");
        assert!(msg.contains("--dry-run"), "offers the preview path: {msg}");
    }
    // The interactive `Ok(confirm(prompt))` arm (yes=false, json=false) reads
    // stdin and is untestable headlessly — covered by existing manual behavior.

    #[test]
    fn detect_stock() {
        assert_eq!(token_type_from_name("Tesla"), "stock");
        assert_eq!(token_type_from_name("Apple Inc"), "stock");
    }

    #[tokio::test(start_paused = true)]
    async fn run_swap_items_parallel_staggers_launches_like_dry_run() {
        // The real -y path must pace its launches exactly like the dry-run
        // quote fetcher: bursting every fetch+execute chain at once feeds
        // Jupiter's per-wallet 429 whose retry backoff costs far more than the
        // stagger (this was why a real 10-token basket took ~22s while its
        // dry-run quoted in ~4s). 3 items ⇒ at least 2 default stagger gaps.
        let t0 = tokio::time::Instant::now();
        let items = vec![1i32, 2, 3];
        let (mut done, _failed, _total) = run_swap_items(
            items,
            true, // parallel
            true, // json
            "items",
            || 0, // never throttled → pure stagger
            |_| String::new(),
            |item: i32| async move { Ok::<(i32, f64), CloseFailJson>((item, 0.0)) },
        )
        .await;
        done.sort();
        assert_eq!(done, vec![1, 2, 3]);
        assert!(
            t0.elapsed() >= Duration::from_millis(DEFAULT_QUOTE_STAGGER_MS) * 2,
            "expected >= 2 stagger gaps in the REAL path, got {:?}",
            t0.elapsed()
        );
        assert!(
            t0.elapsed() < SEQUENTIAL_SPACING,
            "never-throttled run must not widen to serial pace, got {:?}",
            t0.elapsed()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn run_swap_items_parallel_widens_after_throttle_signal() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};
        // Same adaptive widen as the dry-run launcher: item 1 simulates Jupiter
        // pushback via the injected local counter; remaining launches must pace
        // at SEQUENTIAL_SPACING.
        let retries = Arc::new(AtomicU64::new(0));
        let reader = {
            let retries = retries.clone();
            move || retries.load(Ordering::Relaxed)
        };
        let t0 = tokio::time::Instant::now();
        let (mut done, failed, _total) = run_swap_items(
            vec![1i32, 2, 3],
            true,
            true,
            "items",
            reader,
            |_| String::new(),
            move |item: i32| {
                let retries = retries.clone();
                async move {
                    if item == 1 {
                        retries.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok::<(i32, f64), CloseFailJson>((item, 0.0))
                }
            },
        )
        .await;
        done.sort();
        assert_eq!(done, vec![1, 2, 3]);
        assert!(failed.is_empty());
        assert!(
            t0.elapsed() >= SEQUENTIAL_SPACING,
            "a throttle signal must widen the real-path launch pacing, got {:?}",
            t0.elapsed()
        );
    }

    #[tokio::test]
    async fn run_swap_items_parallel_records_a_panicked_task_in_failed() {
        // A task panic yields a JoinError with no item, so the symbol is
        // unknown — but a money op that may have executed must still appear in
        // the report, never vanish from both done[] and failed[] (a silent
        // drop reads to a --json consumer as "everything succeeded").
        let (done, failed, _total) = run_swap_items(
            vec![1i32, 2],
            true, // parallel
            true, // json
            "items",
            || 0,
            |_| String::new(),
            |item: i32| async move {
                if item == 2 {
                    panic!("boom in task");
                }
                Ok::<(i32, f64), CloseFailJson>((item, 0.0))
            },
        )
        .await;
        assert_eq!(done, vec![1]);
        assert_eq!(failed.len(), 1, "the panicked task must be reported, not dropped");
        assert_eq!(failed[0].token, "unknown");
        assert!(failed[0].error.contains("did not complete"), "got: {}", failed[0].error);
    }

    #[tokio::test]
    async fn run_swap_items_parallel_collects_successes_failures_and_sums_total() {
        let items = vec![1i32, 2, 3, -4];
        let (mut done, failed, total) = run_swap_items(
            items,
            true, // parallel
            true, // json (suppress prints)
            "items",
            || 0,
            |_| String::new(),
            |item: i32| async move {
                if item < 0 {
                    Err(CloseFailJson {
                        token: item.to_string(),
                        error: "boom".into(),
                        error_kind: None,
                    })
                } else {
                    Ok((item, item as f64))
                }
            },
        )
        .await;
        // JoinSet completion order is not guaranteed — sort before comparing
        // the success set. A mutant that drops failed items into `done` (or
        // vice versa) would fail either the length or the sorted-content check.
        done.sort();
        assert_eq!(done, vec![1, 2, 3]);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].token, "-4");
        // Sum of the three successful values (1 + 2 + 3), not all four inputs
        // and not the failed item's value — a mutant that folds in failures'
        // values too would give 2.0 (1+2+3-4) instead of 6.0.
        assert!((total - 6.0).abs() < 1e-9, "total was {total}");
    }

    #[tokio::test(start_paused = true)]
    async fn run_swap_items_sequential_preserves_order_and_paces_between_items() {
        let items = vec![10i32, 20, 30];
        let start = tokio::time::Instant::now();
        let (done, failed, total) = run_swap_items(
            items,
            false, // sequential
            true,  // json
            "items",
            || 0,
            |_| String::new(),
            |item: i32| async move {
                if item == 10 {
                    // First item takes longer internally. Sequential
                    // processing still finishes it before starting the next
                    // item, so the output order must stay [10, 20, 30]. A
                    // mutant that runs the "sequential" branch through the
                    // parallel JoinSet would let 20/30 (no internal delay)
                    // race ahead and reorder this.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Ok::<(i32, f64), CloseFailJson>((item, item as f64))
            },
        )
        .await;
        assert_eq!(done, vec![10, 20, 30]);
        assert!(failed.is_empty());
        assert!((total - 60.0).abs() < 1e-9, "total was {total}");
        // Sequential spacing is a documented 3s gap between items (2 gaps
        // for 3 items = 6s minimum). A mutant that drops the inter-item
        // `sleep` would finish near-instantly and fail this bound.
        assert!(
            start.elapsed() >= Duration::from_secs(6),
            "elapsed only {:?}",
            start.elapsed()
        );
    }

    // fetch_orders_sequential is the dry-run counterpart of the sequential
    // branch tested above: two instant no-op fetches must still incur one
    // inter-item gap (3s), proving `--sequential` paces dry-run quote
    // fetching the same way it paces real execution. A mutant that fetches
    // these through the parallel JoinSet instead would finish near-instantly
    // and fail this bound.
    #[test]
    fn launch_gap_widens_to_sequential_spacing_when_throttled() {
        let stagger = Duration::from_millis(350);
        assert_eq!(launch_gap(false, stagger), stagger, "healthy → small stagger");
        assert_eq!(launch_gap(true, stagger), SEQUENTIAL_SPACING, "throttled → serial pace");
    }

    #[test]
    fn quote_stagger_parses_env_with_default_fallback() {
        // Keyless: env wins, garbage/absent → default.
        assert_eq!(quote_stagger_from(None, false), Duration::from_millis(DEFAULT_QUOTE_STAGGER_MS));
        assert_eq!(quote_stagger_from(Some("120"), false), Duration::from_millis(120));
        assert_eq!(quote_stagger_from(Some("0"), false), Duration::from_millis(0));
        assert_eq!(quote_stagger_from(Some("garbage"), false), Duration::from_millis(DEFAULT_QUOTE_STAGGER_MS));
    }

    #[test]
    fn quote_stagger_api_key_profile_zeroes_unless_env_overrides() {
        // Keyed auto-profile: no env → 0ms (the key raises the per-wallet
        // limit, the stagger only costs latency). An explicit env value
        // ALWAYS wins — even over the key profile.
        assert_eq!(quote_stagger_from(None, true), Duration::ZERO);
        assert_eq!(quote_stagger_from(Some("500"), true), Duration::from_millis(500));
        // Garbage env with a key still lands on the keyed profile, not the
        // keyless default.
        assert_eq!(quote_stagger_from(Some("junk"), true), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn parallel_fetch_widens_gap_after_a_throttle_signal() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};

        // Inject a LOCAL retry counter (not the process-global): the fetch bumps
        // it to simulate Jupiter pushback and the launcher paces against the same
        // local reader, so this exercises the adaptive widen deterministically —
        // no sibling test can perturb it.
        let retries = Arc::new(AtomicU64::new(0));
        let reader = {
            let retries = retries.clone();
            move || retries.load(Ordering::Relaxed)
        };
        let t0 = tokio::time::Instant::now();
        let items = vec![1i32, 2i32, 3i32];
        let (mut ready, failed) = fetch_orders_parallel(
            items,
            true,
            "orders",
            reader,
            |_: &i32| String::new(),
            move |item: i32| {
                let retries = retries.clone();
                async move {
                    // Simulate Jupiter pushing back on the FIRST quote.
                    if item == 1 {
                        retries.fetch_add(1, Ordering::Relaxed);
                    }
                    (item.to_string(), Ok::<i32, eyre::Report>(item))
                }
            },
        )
        .await;
        ready.sort();
        assert_eq!(ready, vec![1, 2, 3], "all items still resolve");
        assert!(failed.is_empty());
        // Item 1's retry bumps the counter before item 2's launch check, so the
        // remaining launches widen to SEQUENTIAL_SPACING — at least one 3s gap.
        assert!(
            t0.elapsed() >= SEQUENTIAL_SPACING,
            "a throttle signal must widen the launch pacing, got {:?}",
            t0.elapsed()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn parallel_fetch_staggers_launches_and_still_collects_all() {
        let t0 = tokio::time::Instant::now();
        let items = vec![1i32, 2i32, 3i32];
        let (mut ready, failed) = fetch_orders_parallel(
            items,
            true, // json (suppress prints)
            "orders",
            || 0, // injected reader: never throttled → pure stagger, no widen
            |_: &i32| String::new(),
            |item: i32| async move { (item.to_string(), Ok::<i32, eyre::Report>(item)) },
        )
        .await;
        ready.sort();
        assert_eq!(ready, vec![1, 2, 3], "all items still resolve despite staggering");
        assert!(failed.is_empty());
        // 3 launches with a never-throttled reader ⇒ EXACTLY 2 default stagger
        // gaps (i=1, i=2), never widening. The injected local reader makes this
        // deterministic, so we can now bound BOTH sides (the old process-global
        // forced dropping the upper bound — a sibling test could spuriously trip
        // the widen). Lower bound pinned to the CONST, not `quote_stagger()`
        // (which collapses to `>= 0` under RWA_QUOTE_STAGGER_MS=0 and passes
        // vacuously). Upper bound < SEQUENTIAL_SPACING proves no widen occurred.
        let stagger = Duration::from_millis(DEFAULT_QUOTE_STAGGER_MS);
        assert!(
            t0.elapsed() >= stagger * 2,
            "expected >= 2 stagger gaps, got {:?}",
            t0.elapsed()
        );
        assert!(
            t0.elapsed() < SEQUENTIAL_SPACING,
            "never-throttled fetch must not widen to serial pace, got {:?}",
            t0.elapsed()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn fetch_orders_sequential_paces_items_3s_apart() {
        let t0 = tokio::time::Instant::now();
        let items = vec![1i32, 2i32];
        let (ready, failed) = fetch_orders_sequential(
            items,
            true, // json (suppress prints)
            |_: &i32| String::new(),
            |item: i32| async move { (item.to_string(), Ok::<i32, eyre::Report>(item)) },
        )
        .await;
        assert_eq!(ready, vec![1, 2]);
        assert!(failed.is_empty());
        assert!(
            t0.elapsed() >= Duration::from_secs(3),
            "elapsed only {:?}",
            t0.elapsed()
        );
    }

    // Same helper, failure path: errors are collected into `failed` with the
    // same fail_json shape the parallel path produces, and pacing still
    // applies between items regardless of success/failure.
    #[tokio::test(start_paused = true)]
    async fn fetch_orders_sequential_collects_failures() {
        let items = vec![1i32, -2i32];
        let (ready, failed) = fetch_orders_sequential(
            items,
            true,
            |_: &i32| String::new(),
            |item: i32| async move {
                if item < 0 {
                    (item.to_string(), Err(eyre::eyre!("boom")))
                } else {
                    (item.to_string(), Ok(item))
                }
            },
        )
        .await;
        assert_eq!(ready, vec![1]);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].token, "-2");
        assert!(failed[0].error.contains("boom"));
    }
}
