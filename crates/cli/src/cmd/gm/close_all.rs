use eyre::Result;
use rwa_ondo::{amounts, api, jupiter, solana, token_list, usecases};
use std::sync::Arc;

use super::*;

use usecases::gm::ClosePosition as CloseCandidate;

/// Filter phase: from raw balances to (candidates, skipped[]).
/// The math lives in `usecases::gm::filter_close_positions`; this wrapper only
/// shapes the JSON entries — no printing here, see the doc comment below on
/// why the per-skip stderr line was removed.
fn filter_close_items(
    balances: &[solana::SolanaTokenBalance],
    sell_pct: f64,
    assets: &[api::OndoAsset],
    tradable_set: &std::collections::HashSet<String>,
) -> Result<(Vec<CloseCandidate>, Vec<CloseSkipJson>)> {
    let (candidates, skips) =
        usecases::gm::filter_close_positions(balances, sell_pct, assets, tradable_set)?;
    // No per-skip eprintln here: `render_close_preview`'s "Not selling" block
    // (printed right after this call, before the confirmation) shows the same
    // tokens with their real reason AND retryable/permanent status — a second
    // stderr line per skip would just be noise ahead of that fuller table.
    let skipped = skips
        .into_iter()
        .map(|skip| CloseSkipJson {
            token: skip.token,
            estimated_usd: skip.estimated_usd,
            reason: skip.reason,
            retryable: skip.retryable,
        })
        .collect();
    Ok((candidates, skipped))
}

/// Per-item processor: fetch a sell order and execute it.
async fn process_close_item(
    wallet: Arc<rwa_ondo::wallet::Wallet>,
    taker: String,
    candidate: CloseCandidate,
    json: bool,
    slippage: Option<u32>,
    max_bps: Option<u32>,
) -> std::result::Result<(CloseItemJson, f64), CloseFailJson> {
    let order = match usecases::gm::fetch_sell_order(
        &candidate.symbol,
        &candidate.mint,
        &candidate.sell_raw,
        &taker,
        json,
        slippage,
        max_bps,
    )
    .await
    {
        Ok(o) => o,
        Err(e) => {
            if !json {
                eprintln!("  ✗ {} — {}", candidate.symbol, e);
            }
            return Err(fail_json(candidate.symbol, &e));
        }
    };
    let display = order.display_amount.clone();

    match usecases::gm::execute_sell_from_order(&wallet, order, json).await {
        Ok(exec) => {
            // output_amount comes from amounts::format_amount (valid f64); it
            // only feeds the display total — degrade to 0 rather than panic
            // after a swap that already landed on-chain.
            let usdc_f: f64 = exec.output_amount.parse().unwrap_or(0.0);
            let tx = solscan_tx_url(&exec.signature);
            if !json {
                println!(
                    "  ✓ {} {} → {} USDC  tx: {}",
                    display, candidate.symbol, exec.output_amount, tx
                );
            }
            Ok((
                CloseItemJson {
                    token: candidate.symbol,
                    amount: display,
                    usdc: exec.output_amount,
                    tx,
                },
                usdc_f,
            ))
        }
        Err(e) => {
            if !json {
                eprintln!("  ✗ {} — {}", candidate.symbol, e);
            }
            Err(fail_json(candidate.symbol, &e))
        }
    }
}

/// Sum of the quoted USDC across previewed items. Unparseable amounts are
/// skipped, and so are parseable-but-non-finite ones — Rust's `f64::from_str`
/// happily accepts "NaN"/"inf"/"-inf"/"Infinity" as `Ok`, so a bare `.parse()`
/// filter does NOT catch those; an explicit `is_finite()` check is required
/// to keep either one from poisoning the total (a preview must never print
/// `NaN USDC` or `inf USDC`).
fn sum_quoted_usdc(items: &[CloseItemJson]) -> f64 {
    items
        .iter()
        .filter_map(|i| i.usdc.parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .sum()
}

/// Dry-run: fetch-only, no execute. Sequential (Jupiter rate-limit conservatism).
async fn run_close_dry_run(
    taker: &str,
    candidates: Vec<CloseCandidate>,
    json: bool,
    slippage: Option<u32>,
    max_bps: Option<u32>,
) -> (Vec<CloseItemJson>, Vec<CloseFailJson>, f64) {
    let mut sold = Vec::new();
    let mut failed = Vec::new();

    for c in candidates {
        match usecases::gm::fetch_sell_order(&c.symbol, &c.mint, &c.sell_raw, taker, json, slippage, max_bps).await {
            Ok(order) => {
                let quoted_usdc =
                    amounts::format_amount(&order.order.out_amount, jupiter::USDC_DECIMALS);
                if !json {
                    println!(
                        "  [DRY RUN] Would sell {} {} -> ~{} USDC",
                        c.sell_display, c.symbol, quoted_usdc
                    );
                }
                sold.push(CloseItemJson {
                    token: c.symbol,
                    amount: c.sell_display,
                    usdc: quoted_usdc,
                    tx: String::new(),
                });
            }
            Err(e) => {
                if !json {
                    eprintln!("  [DRY RUN] ✗ {} — {}", c.symbol, e);
                }
                failed.push(fail_json(c.symbol, &e));
            }
        }
    }

    let total = sum_quoted_usdc(&sold);
    (sold, failed, total)
}

pub async fn close_all(
    amount: Option<&str>,
    opts: ExecOpts,
    parallel: bool,
    tuning: TradeTuning,
    rpc_url: Option<&str>,
    selected: Option<&str>,
) -> Result<()> {
    let ExecOpts { yes, dry_run, json } = opts;
    let TradeTuning { slippage, max_bps } = tuning;
    let sell_pct = usecases::gm::parse_sell_pct(amount)?;

    let tokens = token_list::get_token_list();
    let w = load_wallet(selected)?;
    let taker = w.pubkey();

    let (balances_res, assets, tradable_set) = tokio::join!(
        solana::get_all_balances(&taker, tokens, rpc_url),
        api::fetch_assets(),
        usecases::gm::fetch_tradable_set(None)
    );
    let balances = balances_res?;
    let assets = assets.unwrap_or_default();
    if balances.is_empty() {
        if json {
            return json_out(&CloseAllResultJson {
                status: "success",
                sold: vec![],
                failed: vec![],
                skipped: vec![],
                total_usdc: "0".to_string(),
                incomplete_reason: None,
            });
        }
        println!("No GM positions to close.");
        return Ok(());
    }

    let pct_label = if sell_pct < 100.0 {
        format!(" ({}%)", sell_pct)
    } else {
        String::new()
    };

    // Filter FIRST: the confirmation must show what will actually be sold, not
    // the raw balance list that filtering then silently trims.
    let (candidates, skipped) =
        filter_close_items(&balances, sell_pct, &assets, &tradable_set)?;

    // Portfolio value for the share figure: everything we looked at, sellable
    // or not — otherwise the share would always read ~100%.
    let gm_total_usd: f64 = candidates.iter().map(|c| c.est_value).sum::<f64>()
        + skipped.iter().map(|s| s.estimated_usd).sum::<f64>();
    let sellable_usd: f64 = candidates.iter().map(|c| c.est_value).sum();

    if !json {
        print!(
            "{}",
            render_close_preview(&candidates, &skipped, balances.len(), gm_total_usd, sell_pct)
        );
        println!();
    }

    // Emptiness check BEFORE the prompt: an all-skipped wallet (e.g. every
    // position weekend-paused) has nothing to confirm — asking "Sell 0
    // positions for ~0.00 USDC?" would be a wart, and more importantly the
    // status/exit here must obey the same rule as the main path below, not a
    // blanket "success". This is exactly the case the whole plan is about: a
    // retryable skip means the portfolio is NOT closed.
    if candidates.is_empty() {
        // sold=0, failed=0 here (nothing was ever attempted), so
        // close_status can only return "success" or "partial" — never
        // "error" — matching the real multi-trade path's own rules.
        let real_outcome = close_status(0, 0, &skipped);
        // `w` (the live ed25519 SigningKey) is never moved anywhere on this
        // early-return path — drop it explicitly before either possible exit
        // below so a leftover secret can never survive a `std::process::exit`
        // (which runs no destructors), on the real path where exit_code can
        // be 75, and on dry-run for the same reason even though its
        // exit_code is pinned to 0 today.
        drop(w);
        let outcome = if dry_run {
            // A preview must warn exactly as much as the real run would: keep
            // the reason, but status/exit stay dry-run's fixed "never fails".
            CloseOutcome {
                status: "dry_run",
                exit_code: 0,
                incomplete_reason: real_outcome.incomplete_reason,
            }
        } else {
            real_outcome
        };

        if json {
            json_out(&CloseAllResultJson {
                status: outcome.status,
                sold: vec![],
                failed: vec![],
                skipped,
                total_usdc: "0".to_string(),
                incomplete_reason: outcome.incomplete_reason.clone(),
            })?;
            if outcome.exit_code != 0 {
                std::process::exit(outcome.exit_code);
            }
            return Ok(());
        }

        println!("Nothing to sell after filtering.");
        if let Some(reason) = &outcome.incomplete_reason {
            println!("  NOT fully closed — {reason}. Your portfolio is not empty.");
        }
        if outcome.exit_code != 0 {
            std::process::exit(outcome.exit_code);
        }
        return Ok(());
    }

    if !dry_run {
        let prompt = if sell_pct < 100.0 {
            format!(
                "Sell {sell_pct}% of {} positions for ~{sellable_usd:.2} USDC?",
                candidates.len()
            )
        } else {
            format!("Sell {} positions for ~{sellable_usd:.2} USDC?", candidates.len())
        };
        if !require_execution_consent(yes, json, &prompt)? {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let (sold, failed, total_usdc) = if dry_run {
        // `w` is never used on the dry-run path (only `taker`, an owned
        // String, is needed) — drop it explicitly rather than leaving it
        // alive until function exit relying on dry-run's exit_code staying
        // pinned to 0 (see the same reasoning at the empty-candidates
        // early-return above).
        drop(w);
        run_close_dry_run(&taker, candidates, json, slippage, max_bps).await
    } else {
        let wallet_arc = Arc::new(w);
        run_swap_items(
            candidates,
            parallel,
            json,
            "positions",
            jupiter::order_retry_count,
            |c| format!("Selling {} {} ...", c.sell_display, c.symbol),
            |c| process_close_item(wallet_arc.clone(), taker.clone(), c, json, slippage, max_bps),
        )
        .await
    };

    // Status/exit stay "dry_run"/0 unconditionally (a dry-run never "fails"
    // the invocation; it only previews) — but the reason is still populated
    // from the same rule the real path uses, so a preview warns exactly as
    // much as the real run would (a dry-run that hides a retryable skip's
    // reason would tell less truth than the run it's supposed to preview).
    let outcome = if dry_run {
        CloseOutcome {
            status: "dry_run",
            exit_code: 0,
            incomplete_reason: close_status(sold.len(), failed.len(), &skipped).incomplete_reason,
        }
    } else {
        close_status(sold.len(), failed.len(), &skipped)
    };
    let status = outcome.status;
    // Captured before `failed` is moved into the JSON envelope below — feeds
    // `exit_if_all_failed`'s transient/permanent exit-code decision and the
    // human-mode error typing further down.
    let failed_kinds: Vec<Option<String>> =
        failed.iter().map(|f| f.error_kind.map(str::to_string)).collect();

    if json {
        json_out(&CloseAllResultJson {
            status,
            sold,
            failed,
            skipped,
            total_usdc: format!("{total_usdc:.2}"),
            incomplete_reason: outcome.incomplete_reason.clone(),
        })?;
        // Unlike the baskets there is no wallet on the stack to leak here:
        // `wallet_arc` (an ed25519 SigningKey that zeroizes on Drop) is scoped
        // INSIDE the `else` block above and was already dropped when that
        // block ended — all run_swap_items tasks join before it does, so the
        // key is zeroized well before the possible exit in
        // `exit_if_all_failed` (see its doc comment: no drop needed here).
        exit_if_all_failed(status, &failed_kinds);
        if outcome.exit_code != 0 {
            std::process::exit(outcome.exit_code);
        }
        return Ok(());
    }

    let label = if dry_run {
        "[DRY RUN] Would close-all"
    } else if parallel {
        "Close-all (parallel)"
    } else {
        "Close-all"
    };
    println!("\n{}{} complete:", label, pct_label);
    println!(
        "  Sold:    {} positions → {:.2} USDC",
        sold.len(),
        total_usdc
    );
    if !skipped.is_empty() {
        println!("  Skipped: {} ({})", skipped.len(), skipped_summary(&skipped));
    }
    if !failed.is_empty() {
        println!("  Failed:  {} positions", failed.len());
    }
    if let Some(reason) = &outcome.incomplete_reason {
        println!("  NOT fully closed — {reason}. Your portfolio is not empty.");
    }
    if status == "error" {
        return Err(all_failed_error(
            format!("close-all: all {} position(s) failed", failed.len()),
            &failed_kinds,
        ));
    }
    if outcome.exit_code != 0 {
        std::process::exit(outcome.exit_code);
    }
    Ok(())
}

/// Outcome of a close-all run: what to report, what to exit with, and why it
/// is not a plain success.
pub(super) struct CloseOutcome {
    pub status: &'static str,
    pub exit_code: i32,
    /// Present iff temporary skips remain — the sentence shown to a human and
    /// carried in JSON as `incomplete_reason`.
    pub incomplete_reason: Option<String>,
}

/// Status and exit code for a completed close-all.
///
/// A skip that a repeat run would clear (`retryable`) means the portfolio is
/// NOT closed, so the run reports `partial` and exits 75 — the canonical full
/// exit is a shell chain (`close-all && reclaim && send`), and `&&` reads only
/// the exit code. Dust never triggers this: no retry can sell it, so a run
/// that skipped nothing else is a genuine success.
fn close_status(sold: usize, failed: usize, skipped: &[CloseSkipJson]) -> CloseOutcome {
    let retryable = skipped.iter().filter(|s| s.retryable).count();
    let base = multi_status(sold, failed);

    if retryable == 0 {
        return CloseOutcome { status: base, exit_code: 0, incomplete_reason: None };
    }

    // The reason is computed unconditionally once a retryable skip exists —
    // an agent reading `incomplete_reason` must learn the portfolio isn't
    // empty regardless of whether this run also happened to fail everything
    // it attempted. Only the status/exit DECISION is gated on `base` below.
    let plural = if retryable == 1 { "position" } else { "positions" };
    let incomplete_reason = Some(format!(
        "{retryable} {plural} temporarily unsellable — retry later"
    ));

    // `error` (nothing sold, everything failed) stays error/exit 0 — its own
    // exit path (`exit_if_all_failed`) already classifies transient vs
    // permanent for that case — but the reason is still reported.
    if base == "error" {
        return CloseOutcome { status: base, exit_code: 0, incomplete_reason };
    }

    CloseOutcome { status: "partial", exit_code: 75, incomplete_reason }
}

/// Skips grouped by their real reason, in first-seen order:
/// `trading_paused: MRNAon, XYZon; below $1.50 minimum: CAPRon`.
/// Replaces the old summary that labelled every skip as dust.
fn skipped_summary(skipped: &[CloseSkipJson]) -> String {
    let mut groups: Vec<(&str, Vec<&str>)> = Vec::new();
    for s in skipped {
        match groups.iter_mut().find(|(reason, _)| *reason == s.reason) {
            Some((_, tokens)) => tokens.push(s.token.as_str()),
            None => groups.push((s.reason, vec![s.token.as_str()])),
        }
    }
    groups
        .into_iter()
        .map(|(reason, tokens)| format!("{reason}: {}", tokens.join(", ")))
        .collect::<Vec<_>>()
        .join("; ")
}

/// The block shown before the y/N prompt (and in --dry-run). Pure: returns the
/// text so its invariants are unit-testable without spawning the binary.
///
/// Shows what WILL be sold — the caller filters first — with per-position and
/// total USD, the share of the whole portfolio, and the skipped positions with
/// their real reasons split into "retry later" and "permanent".
fn render_close_preview(
    candidates: &[CloseCandidate],
    skipped: &[CloseSkipJson],
    total_positions: usize,
    gm_total_usd: f64,
    sell_pct: f64,
) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let pct_label = if sell_pct < 100.0 { format!(" ({sell_pct}%)") } else { String::new() };
    let sellable_usd: f64 = candidates.iter().map(|c| c.est_value).sum();

    let _ = writeln!(o, "Positions to close{pct_label} ({} of {}):", candidates.len(), total_positions);
    for c in candidates {
        let _ = writeln!(o, "  {:<10} {:>14} tokens  ~{:>10.2} USDC", c.symbol, c.sell_display, c.est_value);
    }
    let _ = writeln!(o, "  {}", "-".repeat(46));
    let share = if gm_total_usd.abs() > f64::EPSILON {
        sellable_usd / gm_total_usd * 100.0
    } else {
        0.0
    };
    let plural = if candidates.len() == 1 { "position" } else { "positions" };
    let _ = writeln!(
        o,
        "  {} {}  ~{:.2} USDC  ({:.1}% of GM portfolio)",
        candidates.len(), plural, sellable_usd, share
    );

    if !skipped.is_empty() {
        let _ = writeln!(o, "\nNot selling ({}):", skipped.len());
        for s in skipped {
            let note = if s.retryable { "retry later" } else { "permanent" };
            let _ = writeln!(o, "  {:<10} — {} ({})", s.token, s.reason, note);
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skip(token: &str, reason: &'static str, retryable: bool) -> CloseSkipJson {
        CloseSkipJson {
            token: token.to_string(),
            estimated_usd: 1.0,
            reason,
            retryable,
        }
    }

    /// breaks if: a temporary skip stops driving the status. This is the whole
    /// point: an agent chaining `close-all && reclaim && send` must not be told
    /// the portfolio is empty while sellable positions remain.
    #[test]
    fn a_retryable_skip_makes_it_partial_even_when_everything_else_sold() {
        let skipped = vec![skip("MRNAon", "trading_paused", true)];
        let out = close_status(7, 0, &skipped);
        assert_eq!(out.status, "partial");
        assert_eq!(out.exit_code, 75, "shell chains read the exit code, not JSON");
        let reason = out.incomplete_reason.expect("must explain the skip");
        // breaks if: pluralisation is hardcoded to "positions" — a single
        // retryable skip must read "1 position", not "1 positions".
        assert!(reason.contains("1 position "), "singular wording: {reason}");
        assert!(!reason.contains("positions"), "must not pluralize a single skip: {reason}");
    }

    /// breaks if: dust starts blocking the exit. A $0.47 position is unsellable
    /// forever; reporting anything but success would make every real portfolio
    /// look incomplete.
    #[test]
    fn dust_only_skips_stay_success_with_exit_zero() {
        let skipped = vec![skip("CAPRon", "below $1.50 minimum", false)];
        let out = close_status(3, 0, &skipped);
        assert_eq!(out.status, "success");
        assert_eq!(out.exit_code, 0);
        assert!(out.incomplete_reason.is_none(), "nothing to retry, nothing to explain");
    }

    /// breaks if: the new retryable rule leaks into the existing failed[] rule.
    /// A partial caused by failures keeps exit 0, exactly as the baskets do.
    #[test]
    fn failures_without_retryable_skips_keep_exit_zero() {
        let out = close_status(2, 1, &[]);
        assert_eq!(out.status, "partial");
        assert_eq!(out.exit_code, 0, "failed[] partial must not change exit code");
        assert!(out.incomplete_reason.is_none());
    }

    /// breaks if: the all-failed case stops being an error, or starts being
    /// reported as merely incomplete.
    #[test]
    fn everything_failed_is_error() {
        let out = close_status(0, 3, &[]);
        assert_eq!(out.status, "error");
    }

    /// breaks if: the `base == "error"` guard is removed. A retryable skip on
    /// top of a totally-failed run must NOT turn "error" into "partial" — the
    /// error path's own exit classification (`exit_if_all_failed`) already
    /// owns transient-vs-permanent here, so this function must stay out of it.
    /// It ALSO breaks if `incomplete_reason` goes missing on this branch: an
    /// all-failed run with paused positions must still tell an agent reading
    /// `incomplete_reason` (per llms.txt: "check `incomplete_reason` before
    /// assuming any close-all run finished the job") that the portfolio is
    /// not empty — `status == "error"` alone does not say that, since a
    /// plain all-failed run with no skips also reports "error".
    #[test]
    fn a_retryable_skip_cannot_turn_all_failed_error_into_partial() {
        let skipped = vec![skip("MRNAon", "trading_paused", true)];
        let out = close_status(0, 3, &skipped);
        assert_eq!(out.status, "error", "error stays error even with a retryable skip present");
        assert_eq!(out.exit_code, 0, "exit_if_all_failed owns this exit code, not close_status");
        assert!(
            out.incomplete_reason.is_some(),
            "an all-failed run with a retryable skip must still explain the skip, not just the failures"
        );
    }

    /// breaks if: an empty wallet is reported as anything but done.
    #[test]
    fn nothing_to_do_is_success() {
        let out = close_status(0, 0, &[]);
        assert_eq!(out.status, "success");
        assert_eq!(out.exit_code, 0);
    }

    /// breaks if: close_status stops treating (0 sold, 0 failed) plus a
    /// retryable skip as partial/exit-75. This is the exact input the
    /// all-skipped fix-round bug fed it: a wallet where every position was
    /// filtered out (e.g. every GM token weekend-paused) never sold or
    /// failed anything, but the portfolio is still not empty — it must not
    /// read as `nothing_to_do_is_success`'s plain "success" above just
    /// because the counts are the same (0, 0); the skip list is what
    /// distinguishes "nothing to do" from "nothing done, something left".
    #[test]
    fn nothing_attempted_but_a_retryable_skip_remains_is_still_partial() {
        let skipped = vec![skip("AALon", "trading_paused", true)];
        let out = close_status(0, 0, &skipped);
        assert_eq!(out.status, "partial");
        assert_eq!(out.exit_code, 75);
        assert!(out.incomplete_reason.is_some());
    }

    /// breaks if: the reason stops naming what to do or how much is left —
    /// the human summary and the JSON field both read it.
    #[test]
    fn incomplete_reason_counts_only_retryable_skips_and_says_retry() {
        let skipped = vec![
            skip("MRNAon", "trading_paused", true),
            skip("XYZon", "not tradable in current session", true),
            skip("CAPRon", "below $1.50 minimum", false),
        ];
        let reason = close_status(1, 0, &skipped).incomplete_reason.unwrap();
        assert!(reason.contains('2'), "two retryable, not three: {reason}");
        assert!(reason.to_lowercase().contains("retry"), "must say what to do: {reason}");
        // breaks if: pluralisation is hardcoded to "positions" for ANY count —
        // this test alone wouldn't catch that (2 is plural either way), but
        // paired with the singular check above it pins both directions.
        assert!(reason.contains("positions"), "two retryable must pluralize: {reason}");
    }

    /// breaks if: a retryable skip alongside failures loses the 75 — the
    /// portfolio is still not empty, so the chain must still stop.
    #[test]
    fn retryable_skip_wins_over_failed_partial_for_the_exit_code() {
        let skipped = vec![skip("MRNAon", "trading_paused", true)];
        let out = close_status(1, 1, &skipped);
        assert_eq!(out.status, "partial");
        assert_eq!(out.exit_code, 75);
    }

    fn candidate(symbol: &str, display: &str, est: f64) -> CloseCandidate {
        CloseCandidate {
            symbol: symbol.to_string(),
            mint: "MintFake1111111111111111111111111111111111".to_string(),
            sell_raw: "1000000000".to_string(),
            sell_display: display.to_string(),
            est_value: est,
        }
    }

    /// breaks if: the confirmation stops telling the user how much money is
    /// involved, or the count/sum in the table stops matching what will
    /// actually be sold. Today the list is printed BEFORE filtering, so it
    /// shows positions that are then silently dropped.
    #[test]
    fn preview_shows_only_sellable_positions_with_sums() {
        let candidates = vec![
            candidate("ABBVon", "6.8517", 1789.46),
            candidate("JNJon", "6.3855", 1645.92),
        ];
        let skipped = vec![skip("CAPRon", "below $1.50 minimum", false)];
        let out = render_close_preview(&candidates, &skipped, 3, 8635.49, 100.0);

        assert!(out.contains("2 of 3"), "count of sellable vs total:\n{out}");
        assert!(out.contains("1789.46") && out.contains("1645.92"), "per-position USD:\n{out}");
        assert!(out.contains("3435.38"), "sum of what will be sold:\n{out}");
        assert!(out.contains("CAPRon") && out.contains("below $1.50"), "skips with reasons:\n{out}");
        assert!(!out.contains("ABBVon — 6.8517 tokens\n"), "old sum-less line must be gone:\n{out}");
    }

    /// breaks if: temporary and permanent skips become indistinguishable in
    /// the preview — the user cannot tell "come back later" from "never".
    #[test]
    fn preview_marks_retryable_skips_differently_from_permanent_ones() {
        let candidates = vec![candidate("ABBVon", "1.0", 100.0)];
        let skipped = vec![
            skip("MRNAon", "trading_paused", true),
            skip("CAPRon", "below $1.50 minimum", false),
        ];
        let out = render_close_preview(&candidates, &skipped, 3, 1000.0, 100.0);
        let paused_line = out.lines().find(|l| l.contains("MRNAon")).expect("paused line");
        let dust_line = out.lines().find(|l| l.contains("CAPRon")).expect("dust line");
        assert!(paused_line.contains("retry later"), "got {paused_line}");
        assert!(dust_line.contains("permanent"), "got {dust_line}");
    }

    /// breaks if: the share of the portfolio is computed against the sellable
    /// subset instead of the whole portfolio — it would then always read
    /// ~100% and tell the user nothing.
    #[test]
    fn preview_share_is_of_the_whole_portfolio() {
        let candidates = vec![candidate("ABBVon", "1.0", 250.0)];
        let out = render_close_preview(&candidates, &[], 4, 1000.0, 100.0);
        assert!(out.contains("25.0%"), "250 of 1000 is a quarter:\n{out}");
    }

    /// breaks if: the zero-total guard is removed. Float division by zero
    /// does not panic in Rust — it silently yields NaN/inf — so an empty-
    /// valued portfolio (gm_total_usd: 0.0) would print "NaN% of GM
    /// portfolio" to a human instead of a safe 0.0%.
    #[test]
    fn preview_share_is_zero_when_the_portfolio_total_is_zero() {
        let candidates = vec![candidate("ABBVon", "1.0", 0.0)];
        let out = render_close_preview(&candidates, &[], 1, 0.0, 100.0);
        assert!(out.contains("0.0%"), "expected a safe 0.0%, got:\n{out}");
        assert!(!out.to_lowercase().contains("nan"), "NaN leaked into output:\n{out}");
        assert!(!out.to_lowercase().contains("inf"), "inf leaked into output:\n{out}");
    }

    /// breaks if: the "Not selling" block appears when nothing was skipped —
    /// an empty section reads as a problem where there is none.
    #[test]
    fn preview_omits_the_not_selling_block_when_nothing_was_skipped() {
        let candidates = vec![candidate("ABBVon", "1.0", 100.0)];
        let out = render_close_preview(&candidates, &[], 1, 100.0, 100.0);
        assert!(!out.contains("Not selling"), "{out}");
        // breaks if: the summary line hardcodes "positions" regardless of
        // count — a single sellable position must read "1 position", not
        // "1 positions" (mirrors the same trap already found in close_status).
        assert!(out.contains("1 position "), "singular wording:\n{out}");
        assert!(!out.contains("1 positions"), "must not pluralize a single position:\n{out}");
    }

    /// breaks if: a partial-percentage close-all (e.g. `close-all 50%`) stops
    /// telling the user only a slice is being sold — the preview would then
    /// read identically to a full close-all, hiding the `sell_pct` the caller
    /// passed in.
    #[test]
    fn preview_shows_the_partial_sell_percentage() {
        let candidates = vec![candidate("ABBVon", "0.5", 50.0)];
        let out = render_close_preview(&candidates, &[], 1, 100.0, 50.0);
        assert!(out.contains("(50%)"), "partial-sell label missing:\n{out}");
    }

    /// breaks if: the `sell_pct < 100.0` boundary flips to `<=` (or otherwise
    /// stops treating exactly 100.0 as "full close"). A full close-all must
    /// print a bare "Positions to close (N of M):" header with NO percentage
    /// label — `render_close_preview_shows_the_partial_sell_percentage` above
    /// already pins that 50% DOES get a label, but nothing previously pinned
    /// the other side of that same boundary at exactly 100.0, which is the
    /// value a real full `close-all` always passes.
    #[test]
    fn preview_shows_no_percentage_label_at_exactly_100_percent() {
        let candidates = vec![candidate("ABBVon", "1.0", 100.0)];
        let out = render_close_preview(&candidates, &[], 1, 100.0, 100.0);
        // The header line carries the `sell_pct` label; the summary line
        // below it legitimately contains a `%` (the portfolio-share figure),
        // so the assertion is scoped to the header, not the whole output.
        let header = out.lines().next().expect("header line");
        assert_eq!(
            header, "Positions to close (1 of 1):",
            "full-close header must be bare, no ` (100%)` suffix: {header}"
        );
    }

    /// breaks if: the skip summary keeps claiming everything was dust. Today
    /// close_all.rs prints "(below $1.50: …)" for every skip regardless of
    /// reason, including paused positions worth thousands.
    #[test]
    fn skipped_summary_groups_by_real_reason() {
        let skipped = vec![
            skip("MRNAon", "trading_paused", true),
            skip("XYZon", "trading_paused", true),
            skip("CAPRon", "below $1.50 minimum", false),
        ];
        let s = skipped_summary(&skipped);
        assert!(s.contains("trading_paused: MRNAon, XYZon"), "grouped by reason: {s}");
        assert!(s.contains("below $1.50 minimum: CAPRon"), "dust named separately: {s}");
    }

    /// breaks if: the dry-run total goes back to a constant. Today
    /// close_all.rs:230 hardcodes 0.0, so a preview of a full close reports
    /// "→ 0.00 USDC" while printing the real per-position quotes above it.
    #[test]
    fn dry_run_total_is_the_sum_of_quoted_items() {
        let items = vec![
            CloseItemJson {
                token: "AALon".to_string(),
                amount: "0.64".to_string(),
                usdc: "9.943261".to_string(),
                tx: String::new(),
            },
            CloseItemJson {
                token: "TSLAon".to_string(),
                amount: "0.03".to_string(),
                usdc: "9.9641".to_string(),
                tx: String::new(),
            },
        ];
        let total = sum_quoted_usdc(&items);
        assert!((total - 19.907361).abs() < 1e-6, "got {total}");
    }

    /// breaks if: an unparseable quote string panics or silently poisons the
    /// total with NaN instead of being skipped.
    #[test]
    fn sum_quoted_usdc_ignores_unparseable_amounts() {
        let items = vec![CloseItemJson {
            token: "X".to_string(),
            amount: "1".to_string(),
            usdc: "not-a-number".to_string(),
            tx: String::new(),
        }];
        assert_eq!(sum_quoted_usdc(&items), 0.0);
    }

    /// breaks if: the finiteness filter is missing. Unlike "not-a-number",
    /// `"NaN"` and `"inf"`/`"-inf"` DO parse successfully via Rust's
    /// `f64::from_str` (`Ok(f64::NAN)` / `Ok(f64::INFINITY)`), so a bare
    /// `.parse().ok()` filter does not catch them — only an explicit
    /// `is_finite()` check does. A valid row is mixed in so the assertion
    /// can't pass merely because non-finite inputs vanish; the surviving sum
    /// must equal the one finite row, not 0.0 and not NaN/inf.
    #[test]
    fn sum_quoted_usdc_ignores_non_finite_amounts() {
        let items = vec![
            CloseItemJson {
                token: "GOOD".to_string(),
                amount: "1".to_string(),
                usdc: "5.5".to_string(),
                tx: String::new(),
            },
            CloseItemJson {
                token: "NAN".to_string(),
                amount: "1".to_string(),
                usdc: "NaN".to_string(),
                tx: String::new(),
            },
            CloseItemJson {
                token: "INF".to_string(),
                amount: "1".to_string(),
                usdc: "inf".to_string(),
                tx: String::new(),
            },
            CloseItemJson {
                token: "NEGINF".to_string(),
                amount: "1".to_string(),
                usdc: "-inf".to_string(),
                tx: String::new(),
            },
        ];
        let total = sum_quoted_usdc(&items);
        assert!(total.is_finite(), "total must be finite, got {total}");
        assert_eq!(total, 5.5, "only the finite row should count, got {total}");
    }
}
