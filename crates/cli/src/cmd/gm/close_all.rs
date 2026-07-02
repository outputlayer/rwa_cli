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
            // output_amount is produced by amounts::format_amount and is always a valid f64
            let usdc_f: f64 = exec
                .output_amount
                .parse()
                .expect("format_amount produces valid f64");
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

#[allow(clippy::too_many_arguments)]
pub async fn close_all(
    amount: Option<&str>,
    yes: bool,
    dry_run: bool,
    parallel: bool,
    json: bool,
    rpc_url: Option<&str>,
    slippage: Option<u32>,
    max_bps: Option<u32>,
    selected: Option<&str>,
) -> Result<()> {
    usecases::gm::ensure_trading_open()?;
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
        if !yes && !json && !confirm(&prompt) {
            println!("Cancelled.");
            return Ok(());
        }
        usecases::gm::ensure_trading_open()?;
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
            |c| format!("Selling {} {} ...", c.sell_display, c.symbol),
            |c| process_close_item(wallet_arc.clone(), taker.clone(), c, json, slippage, max_bps),
        )
        .await
    };

    if json {
        return json_out(&CloseAllResultJson {
            status: if dry_run { "dry_run" } else { "success" },
            sold,
            failed,
            skipped,
            total_usdc: format!("{total_usdc:.2}"),
        });
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
    Ok(())
}
