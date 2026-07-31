use eyre::Result;
use rwa_ondo::{amounts, api, jupiter, solana, token_list, usecases};
use std::sync::Arc;

use super::*;

use usecases::gm::ClosePosition as CloseCandidate;

/// Filter phase: from raw balances to (candidates, skipped[]).
/// The math lives in `usecases::gm::filter_close_positions`; this wrapper only
/// prints the human skip lines and shapes the JSON entries.
fn filter_close_items(
    balances: &[solana::SolanaTokenBalance],
    sell_pct: f64,
    assets: &[api::OndoAsset],
    tradable_set: &std::collections::HashSet<String>,
    json: bool,
) -> Result<(Vec<CloseCandidate>, Vec<CloseSkipJson>)> {
    let (candidates, skips) =
        usecases::gm::filter_close_positions(balances, sell_pct, assets, tradable_set)?;
    let skipped = skips
        .into_iter()
        .map(|skip| {
            if !json {
                eprintln!("  Skipping {} — {}", skip.token, skip.reason);
            }
            CloseSkipJson {
                token: skip.token,
                estimated_usd: skip.estimated_usd,
                reason: skip.reason,
                retryable: skip.retryable,
            }
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

/// Dry-run: fetch-only, no execute. Sequential (Jupiter rate-limit conservatism).
async fn run_close_dry_run(
    taker: &str,
    candidates: Vec<CloseCandidate>,
    json: bool,
    slippage: Option<u32>,
    max_bps: Option<u32>,
) -> (Vec<CloseItemJson>, Vec<CloseFailJson>) {
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

    (sold, failed)
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
    if !json {
        println!("Positions to close{}:", pct_label);
        for b in &balances {
            if sell_pct < 100.0 {
                println!(
                    "  {} — {:.4} of {} tokens",
                    b.symbol,
                    b.balance * sell_pct / 100.0,
                    b.balance
                );
            } else {
                println!("  {} — {} tokens", b.symbol, b.balance);
            }
        }
        println!();
    }

    if !dry_run {
        let prompt = if sell_pct < 100.0 {
            format!("Sell {}% of all positions?", sell_pct)
        } else {
            "Sell all positions?".to_string()
        };
        if !require_execution_consent(yes, json, &prompt)? {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let (candidates, skipped) =
        filter_close_items(&balances, sell_pct, &assets, &tradable_set, json)?;

    if candidates.is_empty() {
        if json {
            return json_out(&CloseAllResultJson {
                status: if dry_run { "dry_run" } else { "success" },
                sold: vec![],
                failed: vec![],
                skipped,
                total_usdc: "0".to_string(),
            });
        }
        println!("Nothing to sell after filtering.");
        return Ok(());
    }

    let (sold, failed, total_usdc) = if dry_run {
        let (sold, failed) = run_close_dry_run(&taker, candidates, json, slippage, max_bps).await;
        (sold, failed, 0.0)
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

    // Real path only — the dry-run status stays "dry_run" unconditionally
    // (a dry-run never "fails" the invocation; it only previews).
    let status = if dry_run { "dry_run" } else { multi_status(sold.len(), failed.len()) };
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
        })?;
        // Unlike the baskets there is no wallet on the stack to leak here:
        // `wallet_arc` (an ed25519 SigningKey that zeroizes on Drop) is scoped
        // INSIDE the `else` block above and was already dropped when that
        // block ended — all run_swap_items tasks join before it does, so the
        // key is zeroized well before the possible exit in
        // `exit_if_all_failed` (see its doc comment: no drop needed here).
        exit_if_all_failed(status, &failed_kinds);
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
        let names: Vec<&str> = skipped.iter().map(|s| s.token.as_str()).collect();
        println!(
            "  Skipped: {} (below ${:.2}: {})",
            skipped.len(),
            usecases::gm::MIN_SELL_VALUE_USD,
            names.join(", ")
        );
    }
    if !failed.is_empty() {
        println!("  Failed:  {} positions", failed.len());
    }
    if status == "error" {
        return Err(all_failed_error(
            format!("close-all: all {} position(s) failed", failed.len()),
            &failed_kinds,
        ));
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

    // `error` (nothing sold, everything failed) stays error — its own exit
    // path already classifies transient vs permanent.
    if base == "error" {
        return CloseOutcome { status: base, exit_code: 0, incomplete_reason: None };
    }

    let plural = if retryable == 1 { "position" } else { "positions" };
    CloseOutcome {
        status: "partial",
        exit_code: 75,
        incomplete_reason: Some(format!(
            "{retryable} {plural} temporarily unsellable — retry later"
        )),
    }
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
    #[test]
    fn a_retryable_skip_cannot_turn_all_failed_error_into_partial() {
        let skipped = vec![skip("MRNAon", "trading_paused", true)];
        let out = close_status(0, 3, &skipped);
        assert_eq!(out.status, "error", "error stays error even with a retryable skip present");
        assert_eq!(out.exit_code, 0, "exit_if_all_failed owns this exit code, not close_status");
    }

    /// breaks if: an empty wallet is reported as anything but done.
    #[test]
    fn nothing_to_do_is_success() {
        let out = close_status(0, 0, &[]);
        assert_eq!(out.status, "success");
        assert_eq!(out.exit_code, 0);
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
}
