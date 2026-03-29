use eyre::Result;
use rwa_ondo::{api, jupiter, solana, token_list};

use super::*;

pub async fn buy(symbol: &str, amount: &str, yes: bool, dry_run: bool, json: bool, rpc_url: Option<&str>, slippage: Option<u32>) -> Result<()> {
    let tokens = token_list::get_token_list();
    let (sym, gm_mint) = resolve_gm_mint(symbol, tokens)?;
    let w = load_wallet()?;
    let taker = w.pubkey();

    let raw_usdc = resolve_amount_to_raw(amount, jupiter::USDC_DECIMALS, || {
        let t = taker.clone();
        let rpc = rpc_url.map(str::to_string);
        async move {
            let (_, raw) = solana::get_usdc_balance_raw(&t, rpc.as_deref()).await?;
            Ok(raw)
        }
    }).await?;
    let usdc_str = jupiter::format_amount(&raw_usdc, jupiter::USDC_DECIMALS);

    // Parallel: preflight (USDC balance check) + tradable check
    let (preflight_res, tradable_res) = tokio::join!(
        preflight_buy_raw(&taker, &raw_usdc, rpc_url),
        check_tradable(&sym),
    );
    preflight_res?;
    tradable_res?;

    let gm_dec = jupiter::GM_SOL_DECIMALS;

    if !json {
        println!("Getting quote for {} USDC -> {} ...", usdc_str, sym);
    }
    let (order, slippage_pct) = get_order_checked(jupiter::USDC_MINT, &gm_mint, &raw_usdc, &taker, slippage, json).await?;

    let out_fmt = jupiter::format_amount(&order.out_amount, gm_dec);
    if !json {
        println!("You will receive ~{} {}", out_fmt, sym);
    }

    if dry_run {
        if json {
            return json_out(&TradeJson {
                status: "dry_run",
                amount: out_fmt,
                token: sym,
                counter_amount: usdc_str,
                counter_token: "USDC",
                tx: String::new(),
                slippage_pct,
                price_impact_pct: order.price_impact,
                fee_bps: order.fee_bps,
                gasless: order.gasless,
                router: order.router.clone(),
            });
        }
        println!("\n[DRY RUN] Trade not executed.");
        println!("  Would buy: ~{} {}", out_fmt, sym);
        println!("  Would spend: {} USDC", usdc_str);
        if let Some(pi) = order.price_impact { println!("  Price impact: {pi:.4}%"); }
        return Ok(());
    }

    if !yes && !json && !confirm("Proceed?") {
        println!("Cancelled.");
        return Ok(());
    }

    if !json {
        println!("Executing swap...");
    }
    let swap = SwapParams { input_mint: jupiter::USDC_MINT, output_mint: &gm_mint, raw_amount: &raw_usdc, taker: &taker, slippage_bps: slippage };
    let result = execute_with_retry(&w, &order, json, &swap).await?;

    let final_out = result.output_amount_result.as_deref()
        .map(|r| jupiter::format_amount(r, gm_dec))
        .unwrap_or(out_fmt);
    let sig = result.signature.as_deref().unwrap_or("unknown");

    if json {
        return json_out(&TradeJson {
            status: "success",
            amount: final_out,
            token: sym,
            counter_amount: usdc_str,
            counter_token: "USDC",
            tx: format!("https://solscan.io/tx/{}", sig),
            slippage_pct,
            price_impact_pct: order.price_impact,
            fee_bps: order.fee_bps,
            gasless: order.gasless,
            router: order.router.clone(),
        });
    }

    println!("\nSwap successful!");
    println!("  Bought:    {} {}", final_out, sym);
    println!("  Spent:     {} USDC", usdc_str);
    println!("  Tx:        https://solscan.io/tx/{}", sig);
    Ok(())
}

pub async fn sell(symbol: &str, amount: &str, yes: bool, dry_run: bool, json: bool, rpc_url: Option<&str>, slippage: Option<u32>) -> Result<()> {
    let tokens = token_list::get_token_list();
    let (sym, gm_mint) = resolve_gm_mint(symbol, tokens)?;
    let w = load_wallet()?;
    let taker = w.pubkey();

    let gm_dec = jupiter::GM_SOL_DECIMALS;
    let is_all = amount.trim().eq_ignore_ascii_case("all");
    let is_pct = amount.trim().ends_with('%');

    // Preflight: trading hours (sync) + tradable check + token balance (parallel)
    preflight_sell()?;
    let (tradable_res, bal_res) = tokio::join!(
        check_tradable(&sym),
        solana::get_balance(&taker, &gm_mint, rpc_url),
    );
    tradable_res?;
    let bal = bal_res?;
    if bal.balance <= 0.0 {
        return Err(eyre::eyre!("Balance is 0 — nothing to trade"));
    }

    let (sell_str, raw_gm) = if is_all || is_pct {
        if is_all {
            let sell_display = jupiter::format_amount(&bal.raw_amount, gm_dec);
            (sell_display, bal.raw_amount)
        } else {
            let pct_str = amount.trim().strip_suffix('%')
                .ok_or_else(|| eyre::eyre!("expected percentage suffix"))?;
            let pct: f64 = pct_str.parse().map_err(|_| eyre::eyre!("Invalid percentage: {}", amount))?;
            if !(0.0..=100.0).contains(&pct) {
                return Err(eyre::eyre!("Percentage must be 0–100, got {pct}"));
            }
            let raw: u128 = bal.raw_amount.parse()
                .map_err(|_| eyre::eyre!("Invalid on-chain amount: {}", bal.raw_amount))?;
            let pct_raw = pct_of_u128(raw, pct);
            let pct_raw_str = pct_raw.to_string();
            let sell_display = jupiter::format_amount(&pct_raw_str, gm_dec);
            (sell_display, pct_raw_str)
        }
    } else {
        let raw = jupiter::token_to_raw(amount, gm_dec)?;
        let raw_sell: u128 = raw.parse().map_err(|_| eyre::eyre!("Invalid amount: {amount}"))?;
        let raw_balance: u128 = bal.raw_amount.parse()
            .map_err(|_| eyre::eyre!("Invalid on-chain amount: {}", bal.raw_amount))?;
        if raw_sell > raw_balance {
            return Err(eyre::eyre!(
                "Insufficient {sym} balance: have {}, trying to sell {}",
                jupiter::format_amount(&bal.raw_amount, gm_dec),
                jupiter::format_amount(&raw, gm_dec)
            ));
        }
        let sell_display = jupiter::format_amount(&raw, gm_dec);
        (sell_display, raw)
    };

    if !json {
        println!("Getting quote for {} {} -> USDC ...", sell_str, sym);
    }
    let (order, slippage_pct) = get_order_checked(&gm_mint, jupiter::USDC_MINT, &raw_gm, &taker, slippage, json).await?;

    let out_fmt = jupiter::format_amount(&order.out_amount, jupiter::USDC_DECIMALS);
    if !json {
        println!("You will receive ~{} USDC", out_fmt);
    }

    if dry_run {
        if json {
            return json_out(&TradeJson {
                status: "dry_run",
                amount: sell_str,
                token: sym,
                counter_amount: out_fmt,
                counter_token: "USDC",
                tx: String::new(),
                slippage_pct,
                price_impact_pct: order.price_impact,
                fee_bps: order.fee_bps,
                gasless: order.gasless,
                router: order.router.clone(),
            });
        }
        println!("\n[DRY RUN] Trade not executed.");
        println!("  Would sell: {} {}", sell_str, sym);
        println!("  Would receive: ~{} USDC", out_fmt);
        if let Some(pi) = order.price_impact { println!("  Price impact: {pi:.4}%"); }
        return Ok(());
    }

    if !yes && !json && !confirm("Proceed?") {
        println!("Cancelled.");
        return Ok(());
    }

    if !json {
        println!("Executing swap...");
    }
    let swap = SwapParams { input_mint: &gm_mint, output_mint: jupiter::USDC_MINT, raw_amount: &raw_gm, taker: &taker, slippage_bps: slippage };
    let result = execute_with_retry(&w, &order, json, &swap).await?;

    let final_out = result.output_amount_result.as_deref()
        .map(|r| jupiter::format_amount(r, jupiter::USDC_DECIMALS))
        .unwrap_or(out_fmt);
    let sig = result.signature.as_deref().unwrap_or("unknown");

    if json {
        return json_out(&TradeJson {
            status: "success",
            amount: sell_str,
            token: sym,
            counter_amount: final_out,
            counter_token: "USDC",
            tx: format!("https://solscan.io/tx/{}", sig),
            slippage_pct,
            price_impact_pct: order.price_impact,
            fee_bps: order.fee_bps,
            gasless: order.gasless,
            router: order.router.clone(),
        });
    }

    println!("\nSwap successful!");
    println!("  Sold:      {} {}", sell_str, sym);
    println!("  Received:  {} USDC", final_out);
    println!("  Tx:        https://solscan.io/tx/{}", sig);
    Ok(())
}

// ── Close All ──────────────────────────────────────────────

/// Parse close-all percentage argument (e.g. "50%") or default to 100%.
fn parse_sell_pct(amount: Option<&str>) -> Result<f64> {
    let Some(s) = amount else { return Ok(100.0) };
    let s = s.trim();
    let pct_str = s.strip_suffix('%')
        .ok_or_else(|| eyre::eyre!("close-all amount must be a percentage (e.g. 10%, 50%)"))?;
    let pct: f64 = pct_str.parse()
        .map_err(|_| eyre::eyre!("Invalid percentage: {s}"))?;
    if !(0.0..=100.0).contains(&pct) {
        return Err(eyre::eyre!("Percentage must be 0–100, got {pct}"));
    }
    Ok(pct)
}

/// Check if a position should be skipped during close-all.
/// Returns `Some(skip_reason)` if it should be skipped, `None` otherwise.
fn should_skip_position(
    tb: &solana::SolanaTokenBalance,
    est_value: f64,
    tradable_set: &std::collections::HashSet<String>,
) -> Option<CloseSkipJson> {
    if est_value > 0.0 && est_value < MIN_SELL_VALUE_USD {
        return Some(CloseSkipJson {
            token: tb.symbol.clone(),
            estimated_usd: est_value,
            reason: "below $1.50 minimum",
        });
    }
    if !tradable_set.is_empty() && !tradable_set.contains(&tb.symbol.to_uppercase()) {
        return Some(CloseSkipJson {
            token: tb.symbol.clone(),
            estimated_usd: est_value,
            reason: "not tradable in current session",
        });
    }
    None
}

pub async fn close_all(amount: Option<&str>, yes: bool, dry_run: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    check_trading_hours()?;
    let sell_pct = parse_sell_pct(amount)?;

    let tokens = token_list::get_token_list();
    let w = load_wallet()?;
    let taker = w.pubkey();

    let (balances_res, assets, tradable_set) = tokio::join!(
        solana::get_all_balances(&taker, tokens, rpc_url),
        api::fetch_assets(),
        fetch_tradable_set()
    );
    let balances = balances_res?;
    let assets = assets.unwrap_or_default();
    if balances.is_empty() {
        if json {
            return json_out(&CloseAllResultJson {
                status: "success", sold: vec![], failed: vec![], skipped: vec![],
                total_usdc: "0".to_string(),
            });
        }
        println!("No GM positions to close.");
        return Ok(());
    }

    let pct_label = if sell_pct < 100.0 { format!(" ({}%)", sell_pct) } else { String::new() };
    if !json {
        println!("Positions to close{}:", pct_label);
        for b in &balances {
            if sell_pct < 100.0 {
                println!("  {} — {:.4} of {} tokens", b.symbol, b.balance * sell_pct / 100.0, b.balance);
            } else {
                println!("  {} — {} tokens", b.symbol, b.balance);
            }
        }
        println!();
    }

    if !dry_run {
        let prompt = if sell_pct < 100.0 { format!("Sell {}% of all positions?", sell_pct) } else { "Sell all positions?".to_string() };
        if !yes && !json && !confirm(&prompt) {
            println!("Cancelled.");
            return Ok(());
        }
        preflight_sell()?;
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
            let raw: u128 = tb.raw_amount.parse()
                .map_err(|_| eyre::eyre!("Invalid on-chain amount for {}: {}", tb.symbol, tb.raw_amount))?;
            let partial = pct_of_u128(raw, sell_pct);
            if partial == 0 { continue; }
            partial.to_string()
        } else {
            tb.raw_amount.clone()
        };

        let sell_balance = if sell_pct < 100.0 { tb.balance * sell_pct / 100.0 } else { tb.balance };
        let price = api::find_asset(&tb.symbol, &assets)
            .and_then(|a| a.primary_market.as_ref())
            .map(|pm| api::parse_price(&pm.price))
            .unwrap_or(0.0);
        let est_value = sell_balance * price;

        if let Some(skip) = should_skip_position(tb, est_value, &tradable_set) {
            if !json { eprintln!("  Skipping {} — {}", skip.token, skip.reason); }
            skipped.push(skip);
            continue;
        }

        let sell_display = jupiter::format_amount(&sell_raw, jupiter::GM_SOL_DECIMALS);

        if dry_run {
            if !json { println!("  [DRY RUN] Would sell {} {}", sell_display, tb.symbol); }
            sold.push(CloseItemJson { token: tb.symbol.clone(), amount: sell_display, usdc: String::new(), tx: String::new() });
            continue;
        }

        if !json { println!("Selling {} {} ...", sell_display, tb.symbol); }

        match sell_one_position(&w, &tb.mint, &sell_raw, &taker, json).await {
            Ok((usdc_str, tx)) => {
                let usdc_f: f64 = usdc_str.parse().unwrap_or(0.0);
                total_usdc += usdc_f;
                if !json { println!("  ✓ {} {} → {} USDC  tx: {}", sell_display, tb.symbol, usdc_str, tx); }
                sold.push(CloseItemJson { token: tb.symbol.clone(), amount: sell_display, usdc: usdc_str, tx });
            }
            Err(e) => {
                if !json { eprintln!("  ✗ {} — {}", tb.symbol, e); }
                failed.push(CloseFailJson { token: tb.symbol.clone(), error: e.to_string() });
            }
        }
    }

    if json {
        return json_out(&CloseAllResultJson {
            status: if dry_run { "dry_run" } else { "success" },
            sold, failed, skipped, total_usdc: format!("{total_usdc:.2}"),
        });
    }

    let label = if dry_run { "[DRY RUN] Would close-all" } else { "Close-all" };
    println!("\n{}{} complete:", label, pct_label);
    println!("  Sold:    {} positions → {:.2} USDC", sold.len(), total_usdc);
    if !skipped.is_empty() {
        let names: Vec<&str> = skipped.iter().map(|s| s.token.as_str()).collect();
        println!("  Skipped: {} (below ${:.2}: {})", skipped.len(), MIN_SELL_VALUE_USD, names.join(", "));
    }
    if !failed.is_empty() { println!("  Failed:  {} positions", failed.len()); }
    Ok(())
}

async fn sell_one_position(
    w: &wallet::Wallet,
    mint: &str,
    raw_amount: &str,
    taker: &str,
    json: bool,
) -> Result<(String, String)> {
    let (order, _) = get_order_checked(mint, jupiter::USDC_MINT, raw_amount, taker, Some(DEFAULT_SLIPPAGE_BPS), json).await?;
    let swap = SwapParams { input_mint: mint, output_mint: jupiter::USDC_MINT, raw_amount, taker, slippage_bps: Some(DEFAULT_SLIPPAGE_BPS) };
    let result = execute_with_retry(w, &order, json, &swap).await?;

    let usdc_out = result.output_amount_result.as_deref()
        .map(|r| jupiter::format_amount(r, jupiter::USDC_DECIMALS))
        .unwrap_or_else(|| jupiter::format_amount(&order.out_amount, jupiter::USDC_DECIMALS));
    let sig = result.signature.as_deref().unwrap_or("unknown");
    let tx = format!("https://solscan.io/tx/{sig}");
    Ok((usdc_out, tx))
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
        let filter_mint = tokens.iter()
            .find(|t| {
                t.symbol.eq_ignore_ascii_case(&filter_upper)
                    || t.symbol.strip_suffix("on").unwrap_or(t.symbol).eq_ignore_ascii_case(&filter_upper)
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
        println!("Found {} empty token account(s) — ~{:.6} SOL reclaimable", empty.len(), sol_estimate);
    }

    let found_count = empty.len();
    let (signatures, reclaimed_lamports) = solana::close_empty_accounts(&w, &empty, rpc_url).await?;

    let sol_reclaimed = reclaimed_lamports as f64 / 1_000_000_000.0;

    if json {
        let status = if signatures.is_empty() && found_count > 0 { "error" } else { "success" };
        return json_out(&ReclaimJson {
            status,
            accounts_closed: signatures.len(),
            sol_reclaimed: format!("{sol_reclaimed:.9}"),
            signatures,
        });
    }

    if signatures.is_empty() && found_count > 0 {
        println!("Found {} empty account(s) but all close attempts failed.", found_count);
    } else {
        println!("Closed {} account(s), reclaimed {:.6} SOL", signatures.len(), sol_reclaimed);
    }
    for sig in &signatures {
        println!("  Tx: https://solscan.io/tx/{sig}");
    }
    Ok(())
}
