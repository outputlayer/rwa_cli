use eyre::{Result, eyre};
use rwa_ondo::{amounts, jupiter, usecases};

use super::*;

/// Parse alternating ["AAPL", "4", "TSLA", "3"] into [("AAPL","4"), ("TSLA","3")].
/// Returns Err if the list is odd-length or any amount is not parseable as a number/pct/all.
fn parse_basket_pairs(tokens: &[String]) -> eyre::Result<Vec<(String, String)>> {
    if !tokens.len().is_multiple_of(2) {
        return Err(eyre!(
            "Expected alternating SYMBOL AMOUNT pairs (e.g. AAPL 4 TSLA 3), got {} token(s)",
            tokens.len()
        ));
    }
    let mut pairs = Vec::with_capacity(tokens.len() / 2);
    let mut i = 0;
    while i < tokens.len() {
        let sym = tokens[i].clone();
        let amt = tokens[i + 1].clone();
        // Basic sanity: amount must look like a number, percentage, or "all"
        let trimmed = amt.trim();
        if !trimmed.eq_ignore_ascii_case("all")
            && !trimmed.ends_with('%')
            && trimmed.parse::<f64>().is_err()
        {
            return Err(eyre!(
                "Invalid amount '{amt}' for token '{sym}'. Use a number (e.g. 10), percentage (e.g. 50%), or 'all'."
            ));
        }
        pairs.push((sym, amt));
        i += 2;
    }
    Ok(pairs)
}

// ── Buy basket ─────────────────────────────────────────────

pub async fn buy_basket(
    tokens: &[String],
    yes: bool,
    dry_run: bool,
    parallel: bool,
    json: bool,
    rpc_url: Option<&str>,
) -> Result<()> {
    let pairs = parse_basket_pairs(tokens)?;
    usecases::gm::ensure_trading_open()?;

    let w = load_wallet()?;
    let taker = w.pubkey();

    // Compute total USDC needed and validate each amount is parseable as USDC
    let mut raw_amounts: Vec<String> = Vec::with_capacity(pairs.len());
    let mut total_raw: u128 = 0;
    for (sym, amt) in &pairs {
        let raw = amounts::token_to_raw(amt, jupiter::USDC_DECIMALS)
            .map_err(|e| eyre!("Invalid amount '{amt}' for {sym}: {e}"))?;
        let raw_u: u128 = raw.parse().map_err(|_| eyre!("Invalid USDC amount for {sym}"))?;
        total_raw = total_raw.saturating_add(raw_u);
        raw_amounts.push(raw);
    }
    usecases::gm::preflight_basket_buy(&taker, total_raw, rpc_url).await?;

    if !json {
        let mode = if parallel { " (parallel)" } else { "" };
        let total_display = amounts::format_amount(&total_raw.to_string(), jupiter::USDC_DECIMALS);
        println!("Buying {} tokens = {} USDC total{}", pairs.len(), total_display, mode);
        for (sym, amt) in &pairs {
            println!("  {sym}: {amt} USDC");
        }
        println!();
    }

    if !dry_run {
        let total_display = amounts::format_amount(&total_raw.to_string(), jupiter::USDC_DECIMALS);
        let prompt = format!("Buy {} tokens for {} USDC total?", pairs.len(), total_display);
        if !yes && !json && !confirm(&prompt) {
            println!("Cancelled.");
            return Ok(());
        }
        usecases::gm::ensure_trading_open()?;
    }

    let symbol_raw: Vec<(String, String)> = pairs
        .iter()
        .zip(raw_amounts.iter())
        .map(|((sym, _), raw)| (sym.clone(), raw.clone()))
        .collect();

    if parallel || dry_run {
        return buy_basket_parallel(w, taker, &symbol_raw, dry_run, json).await;
    }

    // ── Sequential path ──
    let mut bought: Vec<BuyBasketItemJson> = Vec::new();
    let mut failed: Vec<CloseFailJson> = Vec::new();
    let mut total_usdc_spent: f64 = 0.0;

    for (i, (sym, raw)) in symbol_raw.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        let usdc_display = amounts::format_amount(raw, jupiter::USDC_DECIMALS);
        if !json {
            println!("Buying {} {} USDC ...", sym, usdc_display);
        }
        match usecases::gm::fetch_buy_order(sym, raw, &taker, json).await {
            Err(e) => {
                if !json { eprintln!("  ✗ {} — {}", sym, e); }
                failed.push(CloseFailJson { token: sym.clone(), error: e.to_string() });
            }
            Ok(order) => {
                let slip = order.slippage_pct;
                // raw was already validated as u128 upstream when computing total_raw
                let raw_u128: u128 = raw.parse().expect("raw validated as u128 upstream");
                let raw_u: f64 = raw_u128 as f64 / 10f64.powi(jupiter::USDC_DECIMALS as i32);
                match usecases::gm::execute_buy_from_order(&w, order, json).await {
                    Ok(exec) => {
                        total_usdc_spent += raw_u;
                        let tx = solscan_tx_url(&exec.signature);
                        if !json {
                            print!("  ✓ {} {} USDC → {} {}", sym, usdc_display, exec.output_amount, sym);
                            if let Some(s) = slip { print!("  (spread {s:.2}%)"); }
                            println!("  tx: {tx}");
                        }
                        bought.push(BuyBasketItemJson {
                            token: sym.clone(),
                            received: exec.output_amount,
                            usdc: usdc_display,
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
    symbol_raw: &[(String, String)],
    dry_run: bool,
    json: bool,
) -> Result<()> {
    use std::sync::Arc;
    use tokio::task::JoinSet;

    if !json {
        println!("Processing {} buy orders in parallel...", symbol_raw.len());
    }

    let wallet_arc = Arc::new(w);
    let mut failed: Vec<CloseFailJson> = Vec::new();

    if dry_run {
        // ── Dry-run: fetch-only (no execution to consume orders) ──
        let mut order_set: JoinSet<(String, Result<usecases::gm::BuyOrderReady>)> = JoinSet::new();
        for (sym, raw) in symbol_raw {
            let sym = sym.clone();
            let taker = taker.clone();
            let raw = raw.clone();
            order_set.spawn(async move {
                let sym_copy = sym.clone();
                let result = usecases::gm::fetch_buy_order(&sym, &raw, &taker, json).await;
                (sym_copy, result)
            });
        }

        let mut ready_orders: Vec<usecases::gm::BuyOrderReady> = Vec::new();
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

        if json {
            let preview: Vec<BuyBasketItemJson> = ready_orders
                .iter()
                .map(|o| BuyBasketItemJson {
                    token: o.symbol.clone(),
                    received: amounts::format_amount(&o.order.out_amount, jupiter::GM_SOL_DECIMALS),
                    usdc: o.usdc_display.clone(),
                    tx: String::new(),
                    slippage_pct: o.slippage_pct,
                })
                .collect();
            return json_out(&BuyBasketResultJson {
                status: "dry_run",
                bought: preview,
                failed,
                skipped: vec![],
                total_usdc_spent: "0".to_string(),
            });
        }
        println!("\n[DRY RUN] Would buy:");
        for o in &ready_orders {
            let recv = amounts::format_amount(&o.order.out_amount, jupiter::GM_SOL_DECIMALS);
            println!("  {} USDC → {} {}  (spread {:.2}%)",
                o.usdc_display, recv, o.symbol, o.slippage_pct.unwrap_or(0.0));
        }
        if !failed.is_empty() {
            println!("  {} tokens would fail (see above)", failed.len());
        }
        return Ok(());
    }

    // ── Real execution: pipeline fetch→execute per token ──
    // Each task fetches the order and immediately executes it, so pending
    // orders at the market maker stay low (bounded by ORDER_SEMAPHORE).
    let mut pipeline: JoinSet<(String, String, Option<f64>, Result<usecases::gm::SwapExecution>)> = JoinSet::new();
    for (sym, raw) in symbol_raw {
        let sym = sym.clone();
        let raw = raw.clone();
        let taker = taker.clone();
        let w2 = wallet_arc.clone();
        pipeline.spawn(async move {
            let sym_copy = sym.clone();
            let order = match usecases::gm::fetch_buy_order(&sym, &raw, &taker, json).await {
                Ok(o) => o,
                Err(e) => return (sym_copy, raw, None, Err(e)),
            };
            let usdc_disp = order.usdc_display.clone();
            let slip = order.slippage_pct;
            let result = usecases::gm::execute_buy_from_order(&w2, order, json).await;
            (sym_copy, usdc_disp, slip, result)
        });
    }

    let mut bought: Vec<BuyBasketItemJson> = Vec::new();
    let mut total_usdc_spent: f64 = 0.0;

    while let Some(res) = pipeline.join_next().await {
        match res {
            Ok((sym, usdc_disp, slip, Ok(exec))) => {
                // usdc_disp is produced by amounts::format_amount and is always a valid f64
                let usdc_f: f64 = usdc_disp.parse().expect("format_amount produces valid f64");
                total_usdc_spent += usdc_f;
                let tx = solscan_tx_url(&exec.signature);
                if !json {
                    print!("  ✓ {} USDC → {} {}  tx: {}", usdc_disp, exec.output_amount, sym, tx);
                    if let Some(s) = slip { print!("  (spread {s:.2}%)"); }
                    println!();
                }
                bought.push(BuyBasketItemJson {
                    token: sym,
                    received: exec.output_amount,
                    usdc: usdc_disp,
                    tx,
                    slippage_pct: slip,
                });
            }
            Ok((sym, _, _, Err(e))) => {
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

// ── Sell basket ────────────────────────────────────────────

pub async fn sell_basket(
    tokens: &[String],
    yes: bool,
    dry_run: bool,
    parallel: bool,
    json: bool,
    rpc_url: Option<&str>,
) -> Result<()> {
    let pairs = parse_basket_pairs(tokens)?;
    usecases::gm::ensure_trading_open()?;

    let w = load_wallet()?;
    let taker = w.pubkey();

    if !json {
        let mode = if parallel { " (parallel)" } else { "" };
        println!("Selling {} tokens{}", pairs.len(), mode);
        for (sym, amt) in &pairs {
            println!("  {sym}: {amt}");
        }
        println!();
    }

    if !dry_run {
        let prompt = format!("Sell {} tokens?", pairs.len());
        if !yes && !json && !confirm(&prompt) {
            println!("Cancelled.");
            return Ok(());
        }
        usecases::gm::ensure_trading_open()?;
    }

    if parallel || dry_run {
        return sell_basket_parallel(w, taker, &pairs, dry_run, json, rpc_url).await;
    }

    // ── Sequential path ──
    let mut sold: Vec<SellBasketItemJson> = Vec::new();
    let mut failed: Vec<CloseFailJson> = Vec::new();
    let mut total_usdc: f64 = 0.0;

    for (i, (sym, amt)) in pairs.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        if !json { println!("Selling {} {} ...", sym, amt); }
        match usecases::gm::fetch_sell_order_by_symbol(sym, amt, &taker, json, rpc_url).await {
            Err(e) => {
                if !json { eprintln!("  ✗ {} — {}", sym, e); }
                failed.push(CloseFailJson { token: sym.clone(), error: e.to_string() });
            }
            Ok(order) => {
                let slip = order.slippage_pct;
                match usecases::gm::execute_sell_from_order(&w, order, json).await {
                    Ok(exec) => {
                        // output_amount is produced by amounts::format_amount and is always a valid f64
                        let usdc_out: f64 = exec.output_amount.parse().expect("format_amount produces valid f64");
                        total_usdc += usdc_out;
                        let tx = solscan_tx_url(&exec.signature);
                        if !json {
                            print!("  ✓ {} {} → {} USDC", sym, amt, exec.output_amount);
                            if let Some(s) = slip { print!("  (spread {s:.2}%)"); }
                            println!("  tx: {tx}");
                        }
                        sold.push(SellBasketItemJson {
                            token: sym.clone(),
                            amount: amt.clone(),
                            usdc: exec.output_amount,
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

    let total_str = format!("{total_usdc:.2}");
    if json {
        return json_out(&SellBasketResultJson {
            status: "success",
            sold,
            failed,
            skipped: vec![],
            total_usdc_received: total_str,
        });
    }
    println!("\nSell-basket complete:");
    println!("  Sold:    {} tokens → {:.2} USDC", sold.len(), total_usdc);
    if !failed.is_empty() {
        println!("  Failed:  {} tokens", failed.len());
    }
    Ok(())
}

async fn sell_basket_parallel(
    w: rwa_ondo::wallet::Wallet,
    taker: String,
    pairs: &[(String, String)],
    dry_run: bool,
    json: bool,
    rpc_url: Option<&str>,
) -> Result<()> {
    use std::sync::Arc;
    use tokio::task::JoinSet;

    if !json {
        println!("Processing {} sell orders in parallel...", pairs.len());
    }

    let wallet_arc = Arc::new(w);
    let mut failed: Vec<CloseFailJson> = Vec::new();

    if dry_run {
        // ── Dry-run: fetch-only ──
        let mut order_set: JoinSet<(String, Result<usecases::gm::SellOrderReady>)> = JoinSet::new();
        for (sym, amt) in pairs {
            let sym = sym.clone();
            let amt = amt.clone();
            let taker = taker.clone();
            let rpc = rpc_url.map(str::to_string);
            order_set.spawn(async move {
                let sym_copy = sym.clone();
                let result = usecases::gm::fetch_sell_order_by_symbol(&sym, &amt, &taker, json, rpc.as_deref()).await;
                (sym_copy, result)
            });
        }

        let mut ready_orders: Vec<usecases::gm::SellOrderReady> = Vec::new();
        while let Some(res) = order_set.join_next().await {
            match res {
                Ok((sym, Ok(order))) => {
                    if !json {
                        println!(
                            "  ✓ {} — ~{} USDC (spread {:.2}%)",
                            sym,
                            amounts::format_amount(&order.order.out_amount, jupiter::USDC_DECIMALS),
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

        if json {
            let preview: Vec<SellBasketItemJson> = ready_orders
                .iter()
                .map(|o| SellBasketItemJson {
                    token: o.symbol.clone(),
                    amount: o.display_amount.clone(),
                    usdc: amounts::format_amount(&o.order.out_amount, jupiter::USDC_DECIMALS),
                    tx: String::new(),
                    slippage_pct: o.slippage_pct,
                })
                .collect();
            return json_out(&SellBasketResultJson {
                status: "dry_run",
                sold: preview,
                failed,
                skipped: vec![],
                total_usdc_received: "0".to_string(),
            });
        }
        println!("\n[DRY RUN] Would sell:");
        for o in &ready_orders {
            let usdc_out = amounts::format_amount(&o.order.out_amount, jupiter::USDC_DECIMALS);
            println!("  {} {} → {} USDC  (spread {:.2}%)",
                o.symbol, o.display_amount, usdc_out, o.slippage_pct.unwrap_or(0.0));
        }
        if !failed.is_empty() {
            println!("  {} tokens would fail (see above)", failed.len());
        }
        return Ok(());
    }

    // ── Real execution: pipeline fetch→execute per token ──
    let mut pipeline: JoinSet<(String, String, Option<f64>, Result<usecases::gm::SwapExecution>)> = JoinSet::new();
    for (sym, amt) in pairs {
        let sym = sym.clone();
        let amt = amt.clone();
        let taker = taker.clone();
        let rpc = rpc_url.map(str::to_string);
        let w2 = wallet_arc.clone();
        pipeline.spawn(async move {
            let sym_copy = sym.clone();
            let order = match usecases::gm::fetch_sell_order_by_symbol(&sym, &amt, &taker, json, rpc.as_deref()).await {
                Ok(o) => o,
                Err(e) => return (sym_copy, String::new(), None, Err(e)),
            };
            let disp = order.display_amount.clone();
            let slip = order.slippage_pct;
            let result = usecases::gm::execute_sell_from_order(&w2, order, json).await;
            (sym_copy, disp, slip, result)
        });
    }

    let mut sold: Vec<SellBasketItemJson> = Vec::new();
    let mut total_usdc: f64 = 0.0;

    while let Some(res) = pipeline.join_next().await {
        match res {
            Ok((sym, amt_disp, slip, Ok(exec))) => {
                // output_amount is produced by amounts::format_amount and is always a valid f64
                let usdc_out: f64 = exec.output_amount.parse().expect("format_amount produces valid f64");
                total_usdc += usdc_out;
                let tx = solscan_tx_url(&exec.signature);
                if !json {
                    print!("  ✓ {} {} → {} USDC  tx: {}", sym, amt_disp, exec.output_amount, tx);
                    if let Some(s) = slip { print!("  (spread {s:.2}%)"); }
                    println!();
                }
                sold.push(SellBasketItemJson {
                    token: sym,
                    amount: amt_disp,
                    usdc: exec.output_amount,
                    tx,
                    slippage_pct: slip,
                });
            }
            Ok((sym, _, _, Err(e))) => {
                if !json { eprintln!("  ✗ {} — {}", sym, e); }
                failed.push(CloseFailJson { token: sym, error: e.to_string() });
            }
            Err(e) => {
                if !json { eprintln!("  ✗ join error: {}", e); }
            }
        }
    }

    let total_str = format!("{total_usdc:.2}");
    if json {
        return json_out(&SellBasketResultJson {
            status: "success",
            sold,
            failed,
            skipped: vec![],
            total_usdc_received: total_str,
        });
    }
    println!("\nSell-basket (parallel) complete:");
    println!("  Sold:    {} tokens → {:.2} USDC", sold.len(), total_usdc);
    if !failed.is_empty() {
        println!("  Failed:  {} tokens", failed.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // parse_basket_pairs: valid even list of symbol+amount pairs.
    #[test]
    fn parse_basket_pairs_valid() {
        let tokens = vec![
            "AAPL".to_string(), "4".to_string(),
            "TSLA".to_string(), "3".to_string(),
            "NVDA".to_string(), "5.50".to_string(),
        ];
        let pairs = parse_basket_pairs(&tokens).unwrap();
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[0], ("AAPL".to_string(), "4".to_string()));
        assert_eq!(pairs[1], ("TSLA".to_string(), "3".to_string()));
        assert_eq!(pairs[2], ("NVDA".to_string(), "5.50".to_string()));
    }

    // parse_basket_pairs: odd count is an error.
    #[test]
    fn parse_basket_pairs_odd_count_is_err() {
        let tokens = vec!["AAPL".to_string(), "4".to_string(), "TSLA".to_string()];
        assert!(parse_basket_pairs(&tokens).is_err());
    }

    // parse_basket_pairs: invalid amount string is an error.
    #[test]
    fn parse_basket_pairs_invalid_amount_is_err() {
        let tokens = vec!["AAPL".to_string(), "xyz".to_string()];
        assert!(parse_basket_pairs(&tokens).is_err());
    }

    // parse_basket_pairs: "all" and "50%" are valid amounts.
    #[test]
    fn parse_basket_pairs_supports_all_and_pct() {
        let tokens = vec![
            "SPY".to_string(), "all".to_string(),
            "TSLA".to_string(), "50%".to_string(),
        ];
        let pairs = parse_basket_pairs(&tokens).unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].1, "all");
        assert_eq!(pairs[1].1, "50%");
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

    // sell-basket result JSON: total_usdc_received and sold[]/failed[] structure.
    #[test]
    fn sell_basket_result_json_has_correct_shape() {
        let result = SellBasketResultJson {
            status: "success",
            sold: vec![SellBasketItemJson {
                token: "SPYon".to_string(),
                amount: "2.000000".to_string(),
                usdc: "531.25".to_string(),
                tx: "https://solscan.io/tx/abc123".to_string(),
                slippage_pct: Some(-0.12),
            }],
            failed: vec![CloseFailJson {
                token: "TSLAon".to_string(),
                error: "not tradable".to_string(),
            }],
            skipped: vec![],
            total_usdc_received: "531.25".to_string(),
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["status"], "success");
        assert_eq!(json["total_usdc_received"], "531.25");
        assert_eq!(json["sold"][0]["token"], "SPYon");
        assert_eq!(json["sold"][0]["usdc"], "531.25");
        assert_eq!(json["failed"][0]["token"], "TSLAon");
        assert!(json.get("skipped").is_none()); // skip_serializing_if = empty
    }

    // sell-basket dry-run: tx is empty string, total_usdc_received is "0".
    #[test]
    fn sell_basket_dry_run_has_zero_total_and_empty_tx() {
        let result = SellBasketResultJson {
            status: "dry_run",
            sold: vec![SellBasketItemJson {
                token: "AAPLon".to_string(),
                amount: "1.000000".to_string(),
                usdc: "213.40".to_string(),
                tx: String::new(),
                slippage_pct: None,
            }],
            failed: vec![],
            skipped: vec![],
            total_usdc_received: "0".to_string(),
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["status"], "dry_run");
        assert_eq!(json["total_usdc_received"], "0");
        assert_eq!(json["sold"][0]["tx"], "");
        assert!(json.get("skipped").is_none());
        assert!(json["sold"][0].get("slippage_pct").is_none());
    }
}
