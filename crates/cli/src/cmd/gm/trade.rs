use eyre::Result;
use rwa_ondo::{api, jupiter, solana, token_list};

use super::*;

pub async fn quote(symbol: &str, amount: &str, is_sell: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let tokens = token_list::get_token_list();
    check_trading_hours()?;
    let (sym, gm_mint) = resolve_gm_mint(symbol, &tokens)?;
    let w = load_wallet()?;
    let taker = w.pubkey();

    let gm_dec = jupiter::GM_SOL_DECIMALS;
    let usdc_dec = jupiter::USDC_DECIMALS;

    let (input_mint, output_mint, raw_amount, direction) = if is_sell {
        let sell_f = resolve_percent_amount(amount, || {
            let t = taker.clone();
            let m = gm_mint.clone();
            let rpc = rpc_url.map(str::to_string);
            async move {
                let bal = solana::get_balance(&t, &m, rpc.as_deref()).await?;
                Ok(bal.balance)
            }
        }).await?;
        let sell_str = format!("{:.prec$}", sell_f, prec = gm_dec as usize);
        let raw = jupiter::token_to_raw(&sell_str, gm_dec)?;
        (gm_mint.as_str().to_string(), jupiter::USDC_MINT.to_string(), raw, "sell")
    } else {
        let buy_f = resolve_percent_amount(amount, || {
            let t = taker.clone();
            let rpc = rpc_url.map(str::to_string);
            async move { solana::get_usdc_balance(&t, rpc.as_deref()).await }
        }).await?;
        let buy_str = format!("{buy_f:.2}");
        let raw = jupiter::usdc_to_raw(&buy_str)?;
        (jupiter::USDC_MINT.to_string(), gm_mint.clone(), raw, "buy")
    };

    let order = jupiter::get_order(&input_mint, &output_mint, &raw_amount, &taker).await?;

    let (in_label, out_label, in_dec, out_dec) = if direction == "buy" {
        ("USDC", sym.as_str(), usdc_dec, gm_dec)
    } else {
        (sym.as_str(), "USDC", gm_dec, usdc_dec)
    };

    let in_fmt = jupiter::format_amount(&order.in_amount, in_dec);
    let out_fmt = jupiter::format_amount(&order.out_amount, out_dec);

    let (input_usd, output_usd, slippage) = match (order.in_usd_value, order.out_usd_value) {
        (Some(usd_in), Some(usd_out)) => {
            let slip = if usd_in > 0.0 { (usd_out - usd_in) / usd_in * 100.0 } else { 0.0 };
            (Some(usd_in), Some(usd_out), Some(slip))
        }
        _ => (None, None, None),
    };

    if json {
        return json_out(&QuoteJson {
            input: in_fmt,
            input_token: in_label.to_string(),
            output: out_fmt,
            output_token: out_label.to_string(),
            input_usd,
            output_usd,
            slippage_pct: slippage,
            price_impact_pct: order.price_impact,
            fee_bps: order.fee_bps,
        });
    }

    println!("Quote: {} {} -> {} {}", in_fmt, in_label, out_fmt, out_label);
    if let (Some(usd_in), Some(usd_out), Some(slip)) = (input_usd, output_usd, slippage) {
        println!("  Input value:   ${:.2}", usd_in);
        println!("  Output value:  ${:.2}", usd_out);
        println!("  Slippage:      {:.2}%", slip);
    }
    if let Some(pi) = order.price_impact {
        println!("  Price impact:  {:.4}%", pi);
    }
    Ok(())
}

pub async fn buy(symbol: &str, amount: &str, yes: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let tokens = token_list::get_token_list();
    let (sym, gm_mint) = resolve_gm_mint(symbol, &tokens)?;
    let w = load_wallet()?;
    let taker = w.pubkey();

    let usdc_f = resolve_percent_amount(amount, || {
        let t = taker.clone();
        let rpc = rpc_url.map(str::to_string);
        async move { solana::get_usdc_balance(&t, rpc.as_deref()).await }
    }).await?;
    let usdc_str = format!("{usdc_f:.2}");

    preflight_buy(&taker, usdc_f, &w, json, rpc_url).await?;

    let raw_usdc = jupiter::usdc_to_raw(&usdc_str)?;
    let gm_dec = jupiter::GM_SOL_DECIMALS;

    if !json {
        println!("Getting quote for {} USDC -> {} ...", usdc_str, sym);
    }
    let order = jupiter::get_order(jupiter::USDC_MINT, &gm_mint, &raw_usdc, &taker).await?;

    let out_fmt = jupiter::format_amount(&order.out_amount, gm_dec);
    if !json {
        println!("You will receive ~{} {}", out_fmt, sym);
    }
    let slippage = check_slippage(&order, json)?;

    if !yes && !json && !confirm("Proceed?") {
        println!("Cancelled.");
        return Ok(());
    }

    if !json {
        println!("Executing swap...");
    }
    let result = execute_with_retry(&w, &order, json, jupiter::USDC_MINT, &gm_mint, &raw_usdc, &taker).await?;

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
            slippage_pct: slippage,
        });
    }

    println!("\nSwap successful!");
    println!("  Bought:    {} {}", final_out, sym);
    println!("  Spent:     {} USDC", usdc_str);
    println!("  Tx:        https://solscan.io/tx/{}", sig);
    Ok(())
}

pub async fn sell(symbol: &str, amount: &str, yes: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let tokens = token_list::get_token_list();
    let (sym, gm_mint) = resolve_gm_mint(symbol, &tokens)?;
    let w = load_wallet()?;
    let taker = w.pubkey();

    preflight_sell(&taker, &w, json, rpc_url).await?;

    let gm_dec = jupiter::GM_SOL_DECIMALS;
    let is_all = amount.trim().eq_ignore_ascii_case("all");
    let is_pct = amount.trim().ends_with('%');

    let (sell_str, raw_gm) = if is_all || is_pct {
        let bal = solana::get_balance(&taker, &gm_mint, rpc_url).await?;
        if bal.balance <= 0.0 {
            return Err(eyre::eyre!("Balance is 0 — nothing to trade"));
        }
        if is_all {
            let sell_display = jupiter::format_amount(&bal.raw_amount, gm_dec);
            (sell_display, bal.raw_amount)
        } else {
            let pct_str = amount.trim().strip_suffix('%').unwrap();
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
        let sell_f: f64 = amount.parse().map_err(|_| eyre::eyre!("Invalid amount: {amount}"))?;
        let sell_display = format!("{:.prec$}", sell_f, prec = gm_dec as usize);
        let raw = jupiter::token_to_raw(&sell_display, gm_dec)?;
        (sell_display, raw)
    };

    if !json {
        println!("Getting quote for {} {} -> USDC ...", sell_str, sym);
    }
    let order = jupiter::get_order(&gm_mint, jupiter::USDC_MINT, &raw_gm, &taker).await?;

    let out_fmt = jupiter::format_amount(&order.out_amount, jupiter::USDC_DECIMALS);
    if !json {
        println!("You will receive ~{} USDC", out_fmt);
    }
    let slippage = check_slippage(&order, json)?;

    if !yes && !json && !confirm("Proceed?") {
        println!("Cancelled.");
        return Ok(());
    }

    if !json {
        println!("Executing swap...");
    }
    let result = execute_with_retry(&w, &order, json, &gm_mint, jupiter::USDC_MINT, &raw_gm, &taker).await?;

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
            slippage_pct: slippage,
        });
    }

    println!("\nSwap successful!");
    println!("  Sold:      {} {}", sell_str, sym);
    println!("  Received:  {} USDC", final_out);
    println!("  Tx:        https://solscan.io/tx/{}", sig);
    Ok(())
}

// ── Close All ──────────────────────────────────────────────

pub async fn close_all(amount: Option<&str>, yes: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    check_trading_hours()?;

    let sell_pct: f64 = match amount {
        Some(s) => {
            let s = s.trim();
            let pct_str = s.strip_suffix('%')
                .ok_or_else(|| eyre::eyre!("close-all amount must be a percentage (e.g. 10%, 50%)"))?;
            let pct: f64 = pct_str.parse()
                .map_err(|_| eyre::eyre!("Invalid percentage: {s}"))?;
            if !(0.0..=100.0).contains(&pct) {
                return Err(eyre::eyre!("Percentage must be 0–100, got {pct}"));
            }
            pct
        }
        None => 100.0,
    };

    let tokens = token_list::get_token_list();
    let w = load_wallet()?;
    let taker = w.pubkey();

    let (balances_res, assets) = tokio::join!(
        solana::get_all_balances(&taker, &tokens, rpc_url),
        api::fetch_assets()
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

    let pct_label = if sell_pct < 100.0 { format!(" ({}%)", sell_pct) } else { String::new() };
    if !json {
        println!("Positions to close{}:", pct_label);
        for b in &balances {
            if sell_pct < 100.0 {
                println!("  {} — {} of {} tokens", b.symbol, format_args!("{:.4}", b.balance * sell_pct / 100.0), b.balance);
            } else {
                println!("  {} — {} tokens", b.symbol, b.balance);
            }
        }
        println!();
    }

    let prompt = if sell_pct < 100.0 {
        format!("Sell {}% of all positions?", sell_pct)
    } else {
        "Sell all positions?".to_string()
    };
    if !yes && !json && !confirm(&prompt) {
        println!("Cancelled.");
        return Ok(());
    }

    preflight_sell(&taker, &w, json, rpc_url).await?;

    let mut sold = Vec::new();
    let mut failed = Vec::new();
    let mut skipped = Vec::new();
    let mut total_usdc: f64 = 0.0;

    for (i, tb) in balances.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }

        let sell_raw = if sell_pct < 100.0 {
            let raw: u128 = tb.raw_amount.parse().unwrap_or(0);
            let partial = pct_of_u128(raw, sell_pct);
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
        let price = api::find_asset(&tb.symbol, &assets)
            .and_then(|a| a.primary_market.as_ref())
            .map(|pm| api::parse_price(&pm.price))
            .unwrap_or(0.0);
        let est_value = sell_balance * price;
        if est_value > 0.0 && est_value < MIN_SELL_VALUE_USD {
            if !json {
                eprintln!("  Skipping {} — est. ${:.2} below ${:.0} minimum", tb.symbol, est_value, MIN_SELL_VALUE_USD);
            }
            skipped.push(CloseSkipJson {
                token: tb.symbol.clone(),
                estimated_usd: est_value,
                reason: "below $1 minimum",
            });
            continue;
        }

        let sell_display = jupiter::format_amount(&sell_raw, jupiter::GM_SOL_DECIMALS);
        if !json {
            println!("Selling {} {} ...", sell_display, tb.symbol);
        }

        match sell_one_position(&w, &tb.mint, &sell_raw, &taker, json).await {
            Ok((usdc_str, tx)) => {
                let usdc_f: f64 = usdc_str.parse().unwrap_or(0.0);
                total_usdc += usdc_f;
                if !json {
                    println!("  ✓ {} {} → {} USDC  tx: {}", sell_display, tb.symbol, usdc_str, tx);
                }
                sold.push(CloseItemJson {
                    token: tb.symbol.clone(),
                    amount: sell_display,
                    usdc: usdc_str,
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
            status: "success",
            sold,
            failed,
            skipped,
            total_usdc: format!("{total_usdc:.2}"),
        });
    }

    println!("\nClose-all{} complete:", pct_label);
    println!("  Sold:    {} positions → {:.2} USDC", sold.len(), total_usdc);
    if !skipped.is_empty() {
        let names: Vec<&str> = skipped.iter().map(|s| s.token.as_str()).collect();
        println!("  Skipped: {} (below $1: {})", skipped.len(), names.join(", "));
    }
    if !failed.is_empty() {
        println!("  Failed:  {} positions", failed.len());
    }
    Ok(())
}

async fn sell_one_position(
    w: &wallet::Wallet,
    mint: &str,
    raw_amount: &str,
    taker: &str,
    json: bool,
) -> Result<(String, String)> {
    let order = jupiter::get_order(mint, jupiter::USDC_MINT, raw_amount, taker).await?;
    check_slippage(&order, json)?;
    let result = execute_with_retry(w, &order, json, mint, jupiter::USDC_MINT, raw_amount, taker).await?;

    let usdc_out = result.output_amount_result.as_deref()
        .map(|r| jupiter::format_amount(r, jupiter::USDC_DECIMALS))
        .unwrap_or_else(|| jupiter::format_amount(&order.out_amount, jupiter::USDC_DECIMALS));
    let sig = result.signature.as_deref().unwrap_or("unknown");
    let tx = format!("https://solscan.io/tx/{sig}");
    Ok((usdc_out, tx))
}
