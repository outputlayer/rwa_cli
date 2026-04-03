use eyre::Result;
use rwa_ondo::{solana, token_list, usecases};

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
        println!("  Would buy:   ~{} {}", plan.amount, plan.symbol);
        println!("  Would spend:  {} USDC", plan.counter_amount);
        if let Some(s) = plan.slippage_pct {
            println!("  Spread/cost:  {s:.4}%");
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
    if let Some(s) = plan.slippage_pct.filter(|s| s.abs() > 0.05) {
        println!("  Spread:    {s:.4}%");
    }
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
        println!("  Would sell:    {} {}", plan.amount, plan.symbol);
        println!("  Would receive: ~{} USDC", plan.counter_amount);
        if let Some(s) = plan.slippage_pct {
            println!("  Spread/cost:   {s:.4}%");
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
    if let Some(s) = plan.slippage_pct.filter(|s| s.abs() > 0.05) {
        println!("  Spread:    {s:.4}%");
    }
    println!("  Tx:        https://solscan.io/tx/{}", result.signature);
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
