use eyre::{Result, eyre};
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

    // ── Phase 1: fetch all orders in parallel ──
    if !json {
        println!("Fetching {} orders in parallel...", items.len());
    }

    let mut order_set: JoinSet<(String, Result<usecases::gm::SellOrderReady>)> = JoinSet::new();
    for (symbol, mint, raw) in items {
        let taker = taker.clone();
        order_set.spawn(async move {
            let sym_clone = symbol.clone();
            let result = usecases::gm::fetch_sell_order(&symbol, &mint, &raw, &taker, json).await;
            (sym_clone, result)
        });
    }

    let mut ready_orders: Vec<usecases::gm::SellOrderReady> = Vec::new();
    let mut failed: Vec<CloseFailJson> = Vec::new();

    while let Some(res) = order_set.join_next().await {
        match res {
            Ok((sym, Ok(order))) => {
                if !json { println!("  ✓ order: {} (slip {:.2}%)", sym, order.slippage_pct.unwrap_or(0.0)); }
                ready_orders.push(order);
            }
            Ok((sym, Err(e))) => {
                if !json { eprintln!("  ✗ order failed {}: {}", sym, e); }
                failed.push(CloseFailJson { token: sym, error: e.to_string() });
            }
            Err(e) => {
                if !json { eprintln!("  ✗ join error: {}", e); }
            }
        }
    }

    if ready_orders.is_empty() {
        if json {
            return json_out(&CloseAllResultJson { status: "success", sold: vec![], failed, skipped, total_usdc: "0".to_string() });
        }
        println!("All orders failed.");
        return Ok(());
    }

    // ── Phase 2: execute all in parallel ──
    if !json {
        println!("\nExecuting {} swaps in parallel...", ready_orders.len());
    }

    use std::sync::Arc;
    let wallet_arc = Arc::new(w);

    let mut exec_set: JoinSet<(String, String, Result<usecases::gm::SwapExecution>)> = JoinSet::new();
    for order in ready_orders {
        let w2 = wallet_arc.clone();
        let sym = order.symbol.clone();
        let display = order.display_amount.clone();
        exec_set.spawn(async move {
            let result = usecases::gm::execute_sell_from_order(&w2, order, json).await;
            (sym, display, result)
        });
    }

    let mut sold: Vec<CloseItemJson> = Vec::new();
    let mut total_usdc: f64 = 0.0;

    while let Some(res) = exec_set.join_next().await {
        match res {
            Ok((sym, display, Ok(exec))) => {
                let usdc_f: f64 = exec.output_amount.parse().unwrap_or(0.0);
                total_usdc += usdc_f;
                let tx = format!("https://solscan.io/tx/{}", exec.signature);
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

// ── Buy basket ─────────────────────────────────────────────

pub async fn buy_basket(
    symbols: &[String],
    usdc_each: &str,
    yes: bool,
    dry_run: bool,
    parallel: bool,
    json: bool,
    rpc_url: Option<&str>,
) -> Result<()> {
    usecases::gm::ensure_trading_open()?;

    let raw_usdc = amounts::token_to_raw(usdc_each, jupiter::USDC_DECIMALS)?;
    let per_token_display = amounts::format_amount(&raw_usdc, jupiter::USDC_DECIMALS);

    let w = load_wallet()?;
    let taker = w.pubkey();

    // ── Preflight: USDC + SOL balance check ──
    let n = symbols.len() as u128;
    let raw_per: u128 = raw_usdc.parse().map_err(|_| eyre!("Invalid USDC amount"))?;
    let total_raw = raw_per.saturating_mul(n);
    usecases::gm::preflight_basket_buy(&taker, total_raw, rpc_url).await?;

    if !json {
        let mode = if parallel { " (parallel)" } else { "" };
        println!(
            "Buying {} tokens × {} USDC each = {} USDC total{}",
            symbols.len(),
            per_token_display,
            amounts::format_amount(&total_raw.to_string(), jupiter::USDC_DECIMALS),
            mode
        );
        for sym in symbols {
            println!("  {sym}");
        }
        println!();
    }

    if !dry_run {
        let prompt = format!(
            "Buy {} tokens × {} USDC each?",
            symbols.len(),
            per_token_display
        );
        if !yes && !json && !confirm(&prompt) {
            println!("Cancelled.");
            return Ok(());
        }
        usecases::gm::ensure_trading_open()?;
    }

    if parallel || dry_run {
        return buy_basket_parallel(w, taker, symbols, &raw_usdc, dry_run, json).await;
    }

    // ── Sequential path ──
    let mut bought: Vec<BuyBasketItemJson> = Vec::new();
    let mut failed: Vec<CloseFailJson> = Vec::new();
    let mut total_usdc_spent: f64 = 0.0;

    for (i, sym) in symbols.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        if !json {
            println!("Buying {} {} USDC ...", sym, per_token_display);
        }
        match usecases::gm::fetch_buy_order(sym, &raw_usdc, &taker, json).await {
            Err(e) => {
                if !json { eprintln!("  ✗ {} — {}", sym, e); }
                failed.push(CloseFailJson { token: sym.clone(), error: e.to_string() });
            }
            Ok(order) => {
                let slip = order.slippage_pct;
                match usecases::gm::execute_buy_from_order(&w, order, json).await {
                    Ok(exec) => {
                        let usdc_f: f64 = raw_per as f64 / 10f64.powi(jupiter::USDC_DECIMALS as i32);
                        total_usdc_spent += usdc_f;
                        let tx = format!("https://solscan.io/tx/{}", exec.signature);
                        if !json {
                            print!("  ✓ {} {} USDC → {} {}", sym, per_token_display, exec.output_amount, sym);
                            if let Some(s) = slip { print!("  (spread {s:.2}%)"); }
                            println!("  tx: {tx}");
                        }
                        bought.push(BuyBasketItemJson {
                            token: sym.clone(),
                            received: exec.output_amount,
                            usdc: per_token_display.clone(),
                            tx,
                            slippage_pct: slip,
                        });
                    }
                    Err(e) => {
                        if !json { eprintln!("  ✗ {} — {}", sym, e); }
                        failed.push(CloseFailJson { token: sym.clone(), error: e.to_string() });
                    }
                }
            }
        }
    }

    let total_str = format!("{total_usdc_spent:.2}");
    if json {
        return json_out(&BuyBasketResultJson {
            status: "success",
            bought,
            failed,
            skipped: vec![],
            total_usdc_spent: total_str,
        });
    }
    println!("\nBuy-basket complete:");
    println!("  Bought:  {} tokens", bought.len());
    println!("  Total:   {} USDC", total_usdc_spent);
    if !failed.is_empty() {
        println!("  Failed:  {} tokens", failed.len());
    }
    Ok(())
}

async fn buy_basket_parallel(
    w: rwa_ondo::wallet::Wallet,
    taker: String,
    symbols: &[String],
    raw_usdc: &str,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    use tokio::task::JoinSet;

    let per_token_display = amounts::format_amount(raw_usdc, jupiter::USDC_DECIMALS);
    let raw_per: f64 = raw_usdc.parse::<u128>().unwrap_or(0) as f64
        / 10f64.powi(jupiter::USDC_DECIMALS as i32);

    // ── Phase 1: fetch all orders in parallel ──
    if !json {
        println!("Fetching {} buy orders in parallel...", symbols.len());
    }

    let mut order_set: JoinSet<(String, Result<usecases::gm::BuyOrderReady>)> = JoinSet::new();
    for sym in symbols {
        let sym = sym.clone();
        let taker = taker.clone();
        let raw = raw_usdc.to_string();
        order_set.spawn(async move {
            let sym_copy = sym.clone();
            let result = usecases::gm::fetch_buy_order(&sym, &raw, &taker, json).await;
            (sym_copy, result)
        });
    }

    let mut ready_orders: Vec<usecases::gm::BuyOrderReady> = Vec::new();
    let mut failed: Vec<CloseFailJson> = Vec::new();

    while let Some(res) = order_set.join_next().await {
        match res {
            Ok((sym, Ok(order))) => {
                if !json {
                    println!(
                        "  ✓ {} — ~{} {} (spread {:.2}%)",
                        sym,
                        amounts::format_amount(&order.order.out_amount, jupiter::GM_SOL_DECIMALS),
                        sym,
                        order.slippage_pct.unwrap_or(0.0)
                    );
                }
                ready_orders.push(order);
            }
            Ok((sym, Err(e))) => {
                if !json { eprintln!("  ✗ {} — {}", sym, e); }
                failed.push(CloseFailJson { token: sym, error: e.to_string() });
            }
            Err(e) => {
                if !json { eprintln!("  ✗ join error: {}", e); }
            }
        }
    }

    if dry_run {
        if json {
            let skipped: Vec<CloseSkipJson> = vec![];
            let preview: Vec<BuyBasketItemJson> = ready_orders
                .iter()
                .map(|o| BuyBasketItemJson {
                    token: o.symbol.clone(),
                    received: amounts::format_amount(&o.order.out_amount, jupiter::GM_SOL_DECIMALS),
                    usdc: per_token_display.clone(),
                    tx: String::new(),
                    slippage_pct: o.slippage_pct,
                })
                .collect();
            return json_out(&BuyBasketResultJson {
                status: "dry_run",
                bought: preview,
                failed,
                skipped,
                total_usdc_spent: "0".to_string(),
            });
        }
        println!("\n[DRY RUN] Would buy:");
        for o in &ready_orders {
            let recv = amounts::format_amount(&o.order.out_amount, jupiter::GM_SOL_DECIMALS);
            println!("  {} USDC → {} {}  (spread {:.2}%)",
                per_token_display, recv, o.symbol, o.slippage_pct.unwrap_or(0.0));
        }
        if !failed.is_empty() {
            println!("  {} tokens would fail (see above)", failed.len());
        }
        return Ok(());
    }

    if ready_orders.is_empty() {
        if json {
            return json_out(&BuyBasketResultJson {
                status: "success",
                bought: vec![],
                failed,
                skipped: vec![],
                total_usdc_spent: "0".to_string(),
            });
        }
        println!("All orders failed.");
        return Ok(());
    }

    // ── Phase 2: execute all in parallel ──
    if !json {
        println!("\nExecuting {} buys in parallel...", ready_orders.len());
    }

    use std::sync::Arc;
    let wallet_arc = Arc::new(w);

    let mut exec_set: JoinSet<(String, Option<f64>, Result<usecases::gm::SwapExecution>)> = JoinSet::new();
    for order in ready_orders {
        let w2 = wallet_arc.clone();
        let sym = order.symbol.clone();
        let slip = order.slippage_pct;
        exec_set.spawn(async move {
            let result = usecases::gm::execute_buy_from_order(&w2, order, json).await;
            (sym, slip, result)
        });
    }

    let mut bought: Vec<BuyBasketItemJson> = Vec::new();
    let mut total_usdc_spent: f64 = 0.0;

    while let Some(res) = exec_set.join_next().await {
        match res {
            Ok((sym, slip, Ok(exec))) => {
                total_usdc_spent += raw_per;
                let tx = format!("https://solscan.io/tx/{}", exec.signature);
                if !json {
                    print!("  ✓ {} USDC → {} {}  tx: {}", per_token_display, exec.output_amount, sym, tx);
                    if let Some(s) = slip { print!("  (spread {s:.2}%)"); }
                    println!();
                }
                bought.push(BuyBasketItemJson {
                    token: sym,
                    received: exec.output_amount,
                    usdc: per_token_display.clone(),
                    tx,
                    slippage_pct: slip,
                });
            }
            Ok((sym, _, Err(e))) => {
                if !json { eprintln!("  ✗ {} — {}", sym, e); }
                failed.push(CloseFailJson { token: sym, error: e.to_string() });
            }
            Err(e) => {
                if !json { eprintln!("  ✗ join error: {}", e); }
            }
        }
    }

    let total_str = format!("{total_usdc_spent:.2}");
    if json {
        return json_out(&BuyBasketResultJson {
            status: "success",
            bought,
            failed,
            skipped: vec![],
            total_usdc_spent: total_str,
        });
    }
    println!("\nBuy-basket (parallel) complete:");
    println!("  Bought:  {} tokens → {} USDC spent", bought.len(), total_usdc_spent);
    if !failed.is_empty() {
        println!("  Failed:  {} tokens", failed.len());
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

    // buy-basket: failed[] entry is visible to agents with token name and error string.
    #[test]
    fn buy_basket_result_json_failed_entry_has_token_and_error() {
        let result = BuyBasketResultJson {
            status: "success",
            bought: vec![BuyBasketItemJson {
                token: "JNJon".to_string(),
                received: "0.061".to_string(),
                usdc: "15".to_string(),
                tx: "https://solscan.io/tx/abc123".to_string(),
                slippage_pct: Some(-0.52),
            }],
            failed: vec![CloseFailJson {
                token: "TSLAon".to_string(),
                error: "Swap failed (code -2004): swap rejected".to_string(),
            }],
            skipped: vec![],
            total_usdc_spent: "15.00".to_string(),
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["status"], "success");
        assert_eq!(json["failed"][0]["token"], "TSLAon");
        assert!(json["failed"][0]["error"]
            .as_str()
            .unwrap()
            .contains("swap rejected"));
        assert_eq!(json["bought"][0]["token"], "JNJon");
        assert_eq!(json["total_usdc_spent"], "15.00");
    }

    // buy-basket dry-run: tx is empty string, total_usdc_spent is "0".
    #[test]
    fn buy_basket_dry_run_has_zero_total_and_empty_tx() {
        let result = BuyBasketResultJson {
            status: "dry_run",
            bought: vec![BuyBasketItemJson {
                token: "ABTon".to_string(),
                received: "0.142422983".to_string(),
                usdc: "15".to_string(),
                tx: String::new(),
                slippage_pct: Some(-0.69),
            }],
            failed: vec![],
            skipped: vec![],
            total_usdc_spent: "0".to_string(),
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["status"], "dry_run");
        assert_eq!(json["total_usdc_spent"], "0");
        assert_eq!(json["bought"][0]["tx"], "");
        // skipped is omitted when empty (skip_serializing_if)
        assert!(json.get("skipped").is_none());
    }

    // close-all parallel: partial failure — sold[] and failed[] both present in output.
    #[test]
    fn close_all_partial_failure_preserves_both_sold_and_failed() {
        let result = CloseAllResultJson {
            status: "success",
            sold: vec![CloseItemJson {
                token: "JNJon".to_string(),
                amount: "0.061930514".to_string(),
                usdc: "14.946135".to_string(),
                tx: "https://solscan.io/tx/abc123".to_string(),
            }],
            failed: vec![CloseFailJson {
                token: "TSLAon".to_string(),
                error: "Swap failed (code -2000): RFQ failed to land".to_string(),
            }],
            skipped: vec![],
            total_usdc: "14.95".to_string(),
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["status"], "success");
        assert_eq!(json["sold"].as_array().unwrap().len(), 1);
        assert_eq!(json["failed"][0]["token"], "TSLAon");
        assert!(json["failed"][0]["error"]
            .as_str()
            .unwrap()
            .contains("RFQ"));
        assert_eq!(json["total_usdc"], "14.95");
    }

    // buy-basket: slippage_pct is omitted from JSON when None.
    #[test]
    fn buy_basket_item_slippage_omitted_when_none() {
        let item = BuyBasketItemJson {
            token: "AMGNon".to_string(),
            received: "0.055".to_string(),
            usdc: "15".to_string(),
            tx: "https://solscan.io/tx/xyz".to_string(),
            slippage_pct: None,
        };
        let json = serde_json::to_value(&item).unwrap();
        assert!(json.get("slippage_pct").is_none());
        assert_eq!(json["token"], "AMGNon");
    }
}
