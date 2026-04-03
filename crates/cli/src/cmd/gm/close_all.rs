use eyre::Result;
use rwa_ondo::{amounts, api, jupiter, solana, token_list, usecases};

use super::*;

pub async fn close_all(
    amount: Option<&str>,
    yes: bool,
    dry_run: bool,
    parallel: bool,
    json: bool,
    rpc_url: Option<&str>,
) -> Result<()> {
    usecases::gm::ensure_trading_open()?;
    let sell_pct = usecases::gm::parse_sell_pct(amount)?;

    let tokens = token_list::get_token_list();
    let w = load_wallet()?;
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

    let mut sold = Vec::new();
    let mut failed = Vec::new();
    let mut skipped = Vec::new();
    let mut total_usdc: f64 = 0.0;

    if parallel && !dry_run {
        return close_all_parallel(
            w, taker, balances, assets, tradable_set,
            sell_pct, json,
        ).await;
    }

    for (i, tb) in balances.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }

        let sell_raw = if sell_pct < 100.0 {
            let raw: u128 = tb.raw_amount.parse().map_err(|_| {
                eyre::eyre!(
                    "Invalid on-chain amount for {}: {}",
                    tb.symbol,
                    tb.raw_amount
                )
            })?;
            let partial = amounts::pct_of_u128(raw, sell_pct);
            if partial == 0 {
                continue;
            }
            partial.to_string()
        } else {
            tb.raw_amount.clone()
        };

        let sell_balance = if sell_pct < 100.0 {
            tb.balance * sell_pct / 100.0
        } else {
            tb.balance
        };
        let est_value = match api::market_snapshot_for_symbol(&tb.symbol, &assets) {
            Ok((price, _)) => sell_balance * price,
            Err(_) => {
                if !json {
                    eprintln!("  Skipping {} — market data unavailable", tb.symbol);
                }
                skipped.push(CloseSkipJson {
                    token: tb.symbol.clone(),
                    estimated_usd: 0.0,
                    reason: "market data unavailable",
                });
                continue;
            }
        };

        if let Some(skip) = usecases::gm::should_skip_position(&tb.symbol, est_value, &tradable_set)
        {
            if !json {
                eprintln!("  Skipping {} — {}", skip.token, skip.reason);
            }
            skipped.push(CloseSkipJson {
                token: skip.token,
                estimated_usd: skip.estimated_usd,
                reason: skip.reason,
            });
            continue;
        }

        let sell_display = amounts::format_amount(&sell_raw, jupiter::GM_SOL_DECIMALS);

        if dry_run {
            let mint = tb.mint.to_string();
            match usecases::gm::fetch_sell_order(&tb.symbol, &mint, &sell_raw, &taker, json).await {
                Ok(order) => {
                    let quoted_usdc = amounts::format_amount(&order.order.out_amount, jupiter::USDC_DECIMALS);
                    if !json {
                        println!("  [DRY RUN] Would sell {} {} -> ~{} USDC", sell_display, tb.symbol, quoted_usdc);
                    }
                    sold.push(CloseItemJson {
                        token: tb.symbol.clone(),
                        amount: sell_display,
                        usdc: quoted_usdc,
                        tx: String::new(),
                    });
                }
                Err(e) => {
                    if !json {
                        eprintln!("  [DRY RUN] ✗ {} — {}", tb.symbol, e);
                    }
                    failed.push(CloseFailJson {
                        token: tb.symbol.clone(),
                        error: e.to_string(),
                    });
                }
            }
            continue;
        }

        if !json {
            println!("Selling {} {} ...", sell_display, tb.symbol);
        }

        match usecases::gm::execute_sell_raw(&w, &tb.mint, &sell_raw, &taker, json).await {
            Ok(result) => {
                let usdc_f: f64 = result.output_amount.parse().unwrap_or(0.0);
                total_usdc += usdc_f;
                let tx = solscan_tx_url(&result.signature);
                if !json {
                    println!(
                        "  ✓ {} {} → {} USDC  tx: {}",
                        sell_display, tb.symbol, result.output_amount, tx
                    );
                }
                sold.push(CloseItemJson {
                    token: tb.symbol.clone(),
                    amount: sell_display,
                    usdc: result.output_amount,
                    tx,
                });
            }
            Err(e) => {
                if !json {
                    eprintln!("  ✗ {} — {}", tb.symbol, e);
                }
                failed.push(CloseFailJson {
                    token: tb.symbol.clone(),
                    error: e.to_string(),
                });
            }
        }
    }

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

// ── Parallel close-all ──────────────────────────────────────

async fn close_all_parallel(
    w: rwa_ondo::wallet::Wallet,
    taker: String,
    balances: Vec<solana::SolanaTokenBalance>,
    assets: Vec<api::OndoAsset>,
    tradable_set: std::collections::HashSet<String>,
    sell_pct: f64,
    json: bool,
) -> Result<()> {
    use tokio::task::JoinSet;

    // Build list of (symbol, mint, raw_amount) for tradable, non-tiny positions
    let mut items: Vec<(String, String, String)> = Vec::new();
    let mut skipped: Vec<CloseSkipJson> = Vec::new();

    for tb in &balances {
        let sell_raw = if sell_pct < 100.0 {
            let raw: u128 = tb.raw_amount.parse()?;
            let partial = amounts::pct_of_u128(raw, sell_pct);
            if partial == 0 { continue; }
            partial.to_string()
        } else {
            tb.raw_amount.clone()
        };

        let sell_balance = tb.balance * sell_pct / 100.0;
        let est_value = match api::market_snapshot_for_symbol(&tb.symbol, &assets) {
            Ok((price, _)) => sell_balance * price,
            Err(_) => {
                if !json { eprintln!("  Skipping {} — market data unavailable", tb.symbol); }
                skipped.push(CloseSkipJson { token: tb.symbol.clone(), estimated_usd: 0.0, reason: "market data unavailable" });
                continue;
            }
        };

        if let Some(skip) = usecases::gm::should_skip_position(&tb.symbol, est_value, &tradable_set) {
            if !json { eprintln!("  Skipping {} — {}", skip.token, skip.reason); }
            skipped.push(CloseSkipJson { token: skip.token, estimated_usd: skip.estimated_usd, reason: skip.reason });
            continue;
        }

        items.push((tb.symbol.clone(), tb.mint.to_string(), sell_raw));
    }

    if items.is_empty() {
        if json {
            return json_out(&CloseAllResultJson { status: "success", sold: vec![], failed: vec![], skipped, total_usdc: "0".to_string() });
        }
        println!("Nothing to sell after filtering.");
        return Ok(());
    }

    // ── Pipeline: fetch→execute per token in parallel ──
    // Each task fetches its order and immediately executes, keeping the number
    // of pending (unfulfilled) orders at the market maker low.
    if !json {
        println!("Processing {} positions in parallel...", items.len());
    }

    use std::sync::Arc;
    let wallet_arc = Arc::new(w);
    let mut failed: Vec<CloseFailJson> = Vec::new();

    let mut pipeline: JoinSet<(String, String, Result<usecases::gm::SwapExecution>)> = JoinSet::new();
    for (symbol, mint, raw) in items {
        let taker = taker.clone();
        let w2 = wallet_arc.clone();
        pipeline.spawn(async move {
            let sym_clone = symbol.clone();
            let order = match usecases::gm::fetch_sell_order(&symbol, &mint, &raw, &taker, json).await {
                Ok(o) => o,
                Err(e) => return (sym_clone, String::new(), Err(e)),
            };
            let display = order.display_amount.clone();
            let result = usecases::gm::execute_sell_from_order(&w2, order, json).await;
            (sym_clone, display, result)
        });
    }

    let mut sold: Vec<CloseItemJson> = Vec::new();
    let mut total_usdc: f64 = 0.0;

    while let Some(res) = pipeline.join_next().await {
        match res {
            Ok((sym, display, Ok(exec))) => {
                let usdc_f: f64 = exec.output_amount.parse().unwrap_or(0.0);
                total_usdc += usdc_f;
                let tx = solscan_tx_url(&exec.signature);
                if !json { println!("  ✓ {} {} → {} USDC  tx: {}", display, sym, exec.output_amount, tx); }
                sold.push(CloseItemJson { token: sym, amount: display, usdc: exec.output_amount, tx });
            }
            Ok((sym, _display, Err(e))) => {
                if !json { eprintln!("  ✗ {} — {}", sym, e); }
                failed.push(CloseFailJson { token: sym, error: e.to_string() });
            }
            Err(e) => {
                if !json { eprintln!("  ✗ join error: {}", e); }
            }
        }
    }

    if json {
        return json_out(&CloseAllResultJson { status: "success", sold, failed, skipped, total_usdc: format!("{total_usdc:.2}") });
    }

    println!("\nClose-all (parallel) complete:");
    println!("  Sold:    {} positions → {:.2} USDC", sold.len(), total_usdc);
    if !skipped.is_empty() {
        let names: Vec<&str> = skipped.iter().map(|s| s.token.as_str()).collect();
        println!("  Skipped: {} ({})", skipped.len(), names.join(", "));
    }
    if !failed.is_empty() {
        println!("  Failed:  {} positions", failed.len());
    }
    Ok(())
}
