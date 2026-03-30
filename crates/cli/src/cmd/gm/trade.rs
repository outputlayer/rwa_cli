use eyre::Result;
use rwa_ondo::{amounts, api, jupiter, solana, token_list, usecases};
use rwa_ondo::types::Symbol;

use super::*;

pub async fn buy(
    symbol: &str,
    amount: &str,
    yes: bool,
    dry_run: bool,
    json: bool,
    rpc_url: Option<&str>,
    slippage: Option<u32>,
) -> Result<()> {
    let w = load_wallet()?;
    let symbol = Symbol::from(symbol);
    let plan = usecases::gm::prepare_buy(&w, &symbol, amount, rpc_url, slippage, json).await?;

    if !json {
        println!(
            "Getting quote for {} USDC -> {} ...",
            plan.counter_amount, plan.symbol
        );
        println!("You will receive ~{} {}", plan.amount, plan.symbol);
    }

    if dry_run {
        if json {
            return json_out(&TradeJson {
                status: "dry_run",
                amount: plan.amount,
                token: plan.symbol.to_string(),
                counter_amount: plan.counter_amount,
                counter_token: "USDC",
                tx: String::new(),
                slippage_pct: plan.slippage_pct,
                actual_slippage_pct: None,
                price_impact_pct: plan.order.price_impact,
                fee_bps: plan.order.fee_bps,
                gasless: plan.order.gasless,
                router: plan.order.router.clone(),
            });
        }
        println!("\n[DRY RUN] Trade not executed.");
        println!("  Would buy: ~{} {}", plan.amount, plan.symbol);
        println!("  Would spend: {} USDC", plan.counter_amount);
        if let Some(pi) = plan.order.price_impact {
            println!("  Price impact: {pi:.4}%");
        }
        return Ok(());
    }

    if !yes && !json && !confirm("Proceed?") {
        println!("Cancelled.");
        return Ok(());
    }

    if !json {
        println!("Executing swap...");
    }
    let result = usecases::gm::execute_swap(&w, &plan, json).await?;

    if json {
        return json_out(&TradeJson {
            status: "success",
            amount: result.output_amount,
            token: plan.symbol.to_string(),
            counter_amount: plan.counter_amount,
            counter_token: "USDC",
            tx: format!("https://solscan.io/tx/{}", result.signature),
            slippage_pct: plan.slippage_pct,
            actual_slippage_pct: result.actual_slippage_pct,
            price_impact_pct: plan.order.price_impact,
            fee_bps: plan.order.fee_bps,
            gasless: plan.order.gasless,
            router: plan.order.router.clone(),
        });
    }

    println!("\nSwap successful!");
    println!("  Bought:    {} {}", result.output_amount, plan.symbol);
    println!("  Spent:     {} USDC", plan.counter_amount);
    println!("  Tx:        https://solscan.io/tx/{}", result.signature);
    Ok(())
}

pub async fn sell(
    symbol: &str,
    amount: &str,
    yes: bool,
    dry_run: bool,
    json: bool,
    rpc_url: Option<&str>,
    slippage: Option<u32>,
) -> Result<()> {
    let w = load_wallet()?;
    let symbol = Symbol::from(symbol);
    let plan = usecases::gm::prepare_sell(&w, &symbol, amount, rpc_url, slippage, json).await?;

    if !json {
        println!(
            "Getting quote for {} {} -> USDC ...",
            plan.amount, plan.symbol
        );
        println!("You will receive ~{} USDC", plan.counter_amount);
    }

    if dry_run {
        if json {
            return json_out(&TradeJson {
                status: "dry_run",
                amount: plan.amount,
                token: plan.symbol.to_string(),
                counter_amount: plan.counter_amount,
                counter_token: "USDC",
                tx: String::new(),
                slippage_pct: plan.slippage_pct,
                actual_slippage_pct: None,
                price_impact_pct: plan.order.price_impact,
                fee_bps: plan.order.fee_bps,
                gasless: plan.order.gasless,
                router: plan.order.router.clone(),
            });
        }
        println!("\n[DRY RUN] Trade not executed.");
        println!("  Would sell: {} {}", plan.amount, plan.symbol);
        println!("  Would receive: ~{} USDC", plan.counter_amount);
        if let Some(pi) = plan.order.price_impact {
            println!("  Price impact: {pi:.4}%");
        }
        return Ok(());
    }

    if !yes && !json && !confirm("Proceed?") {
        println!("Cancelled.");
        return Ok(());
    }

    if !json {
        println!("Executing swap...");
    }
    let result = usecases::gm::execute_swap(&w, &plan, json).await?;

    if json {
        return json_out(&TradeJson {
            status: "success",
            amount: plan.amount,
            token: plan.symbol.to_string(),
            counter_amount: result.output_amount,
            counter_token: "USDC",
            tx: format!("https://solscan.io/tx/{}", result.signature),
            slippage_pct: plan.slippage_pct,
            actual_slippage_pct: result.actual_slippage_pct,
            price_impact_pct: plan.order.price_impact,
            fee_bps: plan.order.fee_bps,
            gasless: plan.order.gasless,
            router: plan.order.router.clone(),
        });
    }

    println!("\nSwap successful!");
    println!("  Sold:      {} {}", plan.amount, plan.symbol);
    println!("  Received:  {} USDC", result.output_amount);
    println!("  Tx:        https://solscan.io/tx/{}", result.signature);
    Ok(())
}

pub async fn close_all(
    amount: Option<&str>,
    yes: bool,
    dry_run: bool,
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
            if !json {
                println!("  [DRY RUN] Would sell {} {}", sell_display, tb.symbol);
            }
            sold.push(CloseItemJson {
                token: tb.symbol.clone(),
                amount: sell_display,
                usdc: String::new(),
                tx: String::new(),
            });
            continue;
        }

        if !json {
            println!("Selling {} {} ...", sell_display, tb.symbol);
        }

        match usecases::gm::execute_sell_raw(&w, &tb.mint, &sell_raw, &taker, json).await {
            Ok(result) => {
                let usdc_f: f64 = result.output_amount.parse().unwrap_or(0.0);
                total_usdc += usdc_f;
                let tx = format!("https://solscan.io/tx/{}", result.signature);
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

// ── Reclaim ────────────────────────────────────────────────

pub async fn reclaim(token_filter: Option<&str>, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let w = load_wallet()?;
    let pubkey = w.pubkey();

    let mut empty = solana::get_empty_token_accounts(&pubkey, rpc_url).await?;

    // Filter by token if specified
    if let Some(filter) = token_filter {
        let filter_upper = filter.to_uppercase();
        let tokens = token_list::get_token_list();
        // Try to resolve symbol → mint
        let filter_mint = tokens
            .iter()
            .find(|t| {
                t.symbol.eq_ignore_ascii_case(&filter_upper)
                    || t.symbol
                        .strip_suffix("on")
                        .unwrap_or(t.symbol)
                        .eq_ignore_ascii_case(&filter_upper)
            })
            .and_then(|t| t.solana_address)
            .map(|s| s.to_string())
            .unwrap_or_else(|| filter.to_string());

        empty.retain(|a| a.mint == filter_mint);
    }

    if empty.is_empty() {
        if json {
            return json_out(&ReclaimJson {
                status: "success",
                accounts_closed: 0,
                sol_reclaimed: "0".to_string(),
                signatures: vec![],
            });
        }
        println!("No empty token accounts found — nothing to reclaim.");
        return Ok(());
    }

    let total_lamports: u64 = empty.iter().map(|a| a.lamports).sum();
    let sol_estimate = total_lamports as f64 / 1_000_000_000.0;

    if !json {
        println!(
            "Found {} empty token account(s) — ~{:.6} SOL reclaimable",
            empty.len(),
            sol_estimate
        );
    }

    let found_count = empty.len();
    let (signatures, reclaimed_lamports) =
        solana::close_empty_accounts(&w, &empty, rpc_url).await?;

    let sol_reclaimed = reclaimed_lamports as f64 / 1_000_000_000.0;

    if json {
        let status = if signatures.is_empty() && found_count > 0 {
            "error"
        } else {
            "success"
        };
        return json_out(&ReclaimJson {
            status,
            accounts_closed: signatures.len(),
            sol_reclaimed: format!("{sol_reclaimed:.9}"),
            signatures,
        });
    }

    if signatures.is_empty() && found_count > 0 {
        println!(
            "Found {} empty account(s) but all close attempts failed.",
            found_count
        );
    } else {
        println!(
            "Closed {} account(s), reclaimed {:.6} SOL",
            signatures.len(),
            sol_reclaimed
        );
    }
    for sig in &signatures {
        println!("  Tx: https://solscan.io/tx/{sig}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verify the JSON shape of a market-data-unavailable skip entry that close_all produces.
    #[test]
    fn close_skip_json_market_data_unavailable_has_correct_shape() {
        let skip = CloseSkipJson {
            token: "TSLAon".to_string(),
            estimated_usd: 0.0,
            reason: "market data unavailable",
        };
        let json = serde_json::to_value(&skip).unwrap();
        assert_eq!(json.pointer("/token"), Some(&serde_json::Value::from("TSLAon")));
        assert_eq!(json.pointer("/reason"), Some(&serde_json::Value::from("market data unavailable")));
    }
}
