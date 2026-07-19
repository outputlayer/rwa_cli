use eyre::{Result, eyre};
use rwa_ondo::{amounts, jupiter, usecases};
use std::sync::Arc;

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

/// Resolve the basket token source: positional argv XOR `--from-file <path>`
/// (`-` = stdin). Files/stdin are tokenized on whitespace after dropping blank
/// lines and `#` comment lines. Exactly one source must be present.
fn read_basket_tokens(argv: &[String], from_file: Option<&str>) -> eyre::Result<Vec<String>> {
    let invalid = |msg: String| {
        eyre::Report::from(usecases::gm::GmTradeError::new(
            usecases::gm::GmTradeErrorKind::InvalidAmount,
            msg,
        ))
    };
    match (argv.is_empty(), from_file) {
        (false, Some(_)) => Err(invalid(
            "pass tokens as arguments OR --from-file, not both".to_string(),
        )),
        (true, None) => Err(invalid(
            "no tokens given: pass SYMBOL AMOUNT pairs (or bare symbols with --equal), or --from-file <path>".to_string(),
        )),
        (false, None) => Ok(argv.to_vec()),
        (true, Some(path)) => {
            let raw = if path == "-" {
                use std::io::Read as _;
                let mut s = String::new();
                std::io::stdin()
                    .read_to_string(&mut s)
                    .map_err(|e| invalid(format!("failed to read tokens from stdin: {e}")))?;
                s
            } else {
                std::fs::read_to_string(path)
                    .map_err(|e| invalid(format!("failed to read --from-file '{path}': {e}")))?
            };
            let toks: Vec<String> = raw
                .lines()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .flat_map(|l| l.split_whitespace())
                .map(str::to_string)
                .collect();
            if toks.is_empty() {
                let src = if path == "-" { "stdin".to_string() } else { format!("--from-file '{path}'") };
                return Err(invalid(format!("no tokens read from {src}")));
            }
            Ok(toks)
        }
    }
}

/// Strip the trailing `%` (and any surrounding whitespace) from a `--total`
/// weight for the JSON `allocation.weights` echo, e.g. `"50 %"` -> `"50"`.
fn echo_weight(amt: &str) -> String {
    amt.trim().trim_end_matches('%').trim().to_string()
}

/// Resolve basket pairs into per-item raw USDC: split `--total` across
/// percent weights, or parse absolute amounts (the historical path). Pure —
/// runs before wallet load so bad input never touches the network.
fn resolve_buy_items(
    pairs: &[(String, String)],
    total: Option<&str>,
) -> eyre::Result<Vec<(String, u128)>> {
    if let Some(total) = total {
        return usecases::gm::split_total_by_weights(total, pairs);
    }
    let mut items = Vec::with_capacity(pairs.len());
    for (sym, amt) in pairs {
        if amt.trim().ends_with('%') {
            return Err(usecases::gm::GmTradeError::new(
                usecases::gm::GmTradeErrorKind::InvalidAmount,
                format!("Percentage amounts in buy-basket require --total (e.g. {sym} {amt} --total 1000)"),
            )
            .into());
        }
        let raw = amounts::token_to_raw(amt, jupiter::USDC_DECIMALS)
            .map_err(|e| eyre!("Invalid amount '{amt}' for {sym}: {e}"))?;
        let raw_u: u128 = raw.parse().map_err(|_| eyre!("Invalid USDC amount for {sym}"))?;
        items.push((sym.clone(), raw_u));
    }
    Ok(items)
}

/// Resolve bare symbols into per-item raw USDC by splitting `--total` evenly
/// (the `--equal` mode). Rejects a missing `--total` and any token that carries
/// an amount/percent (with `--equal` the tokens must be bare symbols). Pure —
/// runs before wallet load.
fn resolve_equal_items(symbols: &[String], total: Option<&str>) -> eyre::Result<Vec<(String, u128)>> {
    let total = total.ok_or_else(|| {
        eyre::Report::from(usecases::gm::GmTradeError::new(
            usecases::gm::GmTradeErrorKind::InvalidAmount,
            "--equal requires --total (e.g. --total 1000 --equal TSLA NVDA SPY)".to_string(),
        ))
    })?;
    for s in symbols {
        let t = s.trim();
        if t.eq_ignore_ascii_case("all") || t.ends_with('%') || t.parse::<f64>().is_ok() {
            return Err(usecases::gm::GmTradeError::new(
                usecases::gm::GmTradeErrorKind::InvalidAmount,
                format!("--equal takes bare symbols, not amounts; got '{s}'"),
            )
            .into());
        }
    }
    usecases::gm::split_total_equal(total, symbols)
}

// ── Per-item processors ─────────────────────────────────────
//
// One processor per operation. Each does fetch + execute and returns either
// the JSON entry + USDC delta on success, or a CloseFailJson on failure.
// Used by both the sequential and parallel orchestrators below.

#[allow(clippy::too_many_arguments)]
async fn process_buy_item(
    wallet: Arc<rwa_ondo::wallet::Wallet>,
    taker: String,
    sym: String,
    raw: String,
    json: bool,
    slippage: Option<u32>,
    max_bps: Option<u32>,
    sol_lamports: u64,
) -> std::result::Result<(BuyBasketItemJson, f64), CloseFailJson> {
    let order = match usecases::gm::fetch_buy_order(&sym, &raw, &taker, json, slippage, max_bps, Some(sol_lamports)).await {
        Ok(o) => o,
        Err(e) => {
            if !json {
                eprintln!("  ✗ {} — {}", sym, e);
            }
            return Err(fail_json(sym, &e));
        }
    };
    let usdc_display = order.usdc_display.clone();
    let slip = order.slippage_pct;

    match usecases::gm::execute_buy_from_order(&wallet, order, json).await {
        Ok(exec) => {
            // raw was validated as u128 upstream; the value only feeds the
            // display total, so degrade to 0 rather than panic — a panic HERE
            // fires after an on-chain trade succeeded, killing the JoinSet and
            // losing the whole JSON report (tx URLs included).
            let raw_u128: u128 = raw.parse().unwrap_or(0);
            let raw_u: f64 = raw_u128 as f64 / 10f64.powi(jupiter::USDC_DECIMALS as i32);
            let tx = solscan_tx_url(&exec.signature);
            if !json {
                print!("  ✓ {} {} USDC → {} {}", sym, usdc_display, exec.output_amount, sym);
                if let Some(s) = slip { print!("  (spread {s:.2}%)"); }
                println!("  tx: {tx}");
            }
            Ok((
                BuyBasketItemJson {
                    token: sym,
                    received: exec.output_amount,
                    usdc: usdc_display,
                    tx,
                    slippage_pct: slip,
                },
                raw_u,
            ))
        }
        Err(e) => {
            if !json {
                eprintln!("  ✗ {} — {}", sym, e);
            }
            Err(fail_json(sym, &e))
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn process_sell_item(
    wallet: Arc<rwa_ondo::wallet::Wallet>,
    taker: String,
    sym: String,
    amt: String,
    json: bool,
    rpc_url: Option<String>,
    slippage: Option<u32>,
    max_bps: Option<u32>,
) -> std::result::Result<(SellBasketItemJson, f64), CloseFailJson> {
    let order = match usecases::gm::fetch_sell_order_by_symbol(&sym, &amt, &taker, json, rpc_url.as_deref(), slippage, max_bps).await {
        Ok(o) => o,
        Err(e) => {
            if !json {
                eprintln!("  ✗ {} — {}", sym, e);
            }
            return Err(fail_json(sym, &e));
        }
    };
    let display_amt = order.display_amount.clone();
    let slip = order.slippage_pct;

    match usecases::gm::execute_sell_from_order(&wallet, order, json).await {
        Ok(exec) => {
            // output_amount comes from amounts::format_amount (valid f64); it
            // only feeds the display total — degrade to 0 rather than panic
            // after a swap that already landed on-chain.
            let usdc_out: f64 = exec.output_amount.parse().unwrap_or(0.0);
            let tx = solscan_tx_url(&exec.signature);
            if !json {
                print!("  ✓ {} {} → {} USDC", sym, display_amt, exec.output_amount);
                if let Some(s) = slip { print!("  (spread {s:.2}%)"); }
                println!("  tx: {tx}");
            }
            Ok((
                SellBasketItemJson {
                    token: sym,
                    amount: display_amt,
                    usdc: exec.output_amount,
                    tx,
                    slippage_pct: slip,
                },
                usdc_out,
            ))
        }
        Err(e) => {
            if !json {
                eprintln!("  ✗ {} — {}", sym, e);
            }
            Err(fail_json(sym, &e))
        }
    }
}

// ── Buy basket ─────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub async fn buy_basket(
    tokens: &[String],
    total: Option<&str>,
    equal: bool,
    from_file: Option<&str>,
    opts: ExecOpts,
    parallel: bool,
    tuning: TradeTuning,
    rpc_url: Option<&str>,
    selected: Option<&str>,
) -> Result<()> {
    let ExecOpts { yes, dry_run, json } = opts;
    let TradeTuning { slippage, max_bps } = tuning;
    let tokens: Vec<String> = read_basket_tokens(tokens, from_file)?;

    // Resolve items + the JSON `allocation` echo per mode.
    let (items, allocation): (Vec<(String, u128)>, Option<AllocationJson>) = if equal {
        let items = resolve_equal_items(&tokens, total)?;
        let n = items.len();
        let each_pct = if n > 0 { 100.0 / n as f64 } else { 0.0 };
        let weights = items
            .iter()
            .map(|(sym, _)| (sym.clone(), format!("{each_pct:.4}")))
            .collect();
        let alloc = total.map(|t| AllocationJson { total: t.to_string(), weights });
        (items, alloc)
    } else {
        let pairs = parse_basket_pairs(&tokens)?;
        let items = resolve_buy_items(&pairs, total)?;
        let alloc = total.map(|t| AllocationJson {
            total: t.to_string(),
            weights: pairs.iter().map(|(sym, amt)| (sym.clone(), echo_weight(amt))).collect(),
        });
        (items, alloc)
    };
    let total_raw: u128 = items.iter().map(|(_, r)| r).sum();

    let w = load_wallet(selected)?;
    let taker = w.pubkey();

    // Auto-refuel SOL from USDC before a real basket buy; the reserve keeps
    // the refuel from eating the basket's own USDC.
    let (gas_refuel, balances) = if dry_run {
        (None, None)
    } else {
        auto_gas(&w, rpc_url, yes, json, total_raw).await?
    };
    let sol_lamports = usecases::gm::preflight_basket_buy(&taker, &items, total_raw, rpc_url, balances).await?;

    if !json {
        let mode = if parallel { " (parallel)" } else { "" };
        let total_display = amounts::format_amount(&total_raw.to_string(), jupiter::USDC_DECIMALS);
        println!("Buying {} tokens = {} USDC total{}", items.len(), total_display, mode);
        for (sym, raw) in &items {
            println!("  {sym}: {} USDC", amounts::format_amount(&raw.to_string(), jupiter::USDC_DECIMALS));
        }
        println!();
    }

    if !dry_run {
        let total_display = amounts::format_amount(&total_raw.to_string(), jupiter::USDC_DECIMALS);
        let prompt = format!("Buy {} tokens for {} USDC total?", items.len(), total_display);
        if !require_execution_consent(yes, json, &prompt)? {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let symbol_raw: Vec<(String, String)> = items
        .iter()
        .map(|(sym, raw)| (sym.clone(), raw.to_string()))
        .collect();

    if dry_run {
        return buy_basket_dry_run(&taker, &symbol_raw, allocation, parallel, json, slippage, max_bps).await;
    }

    let wallet_arc = Arc::new(w);
    let (bought, failed, total_usdc_spent) = run_swap_items(
        symbol_raw,
        parallel,
        json,
        "buy orders",
        jupiter::order_retry_count,
        |(sym, raw)| {
            format!("Buying {} {} USDC ...", sym, amounts::format_amount(raw, jupiter::USDC_DECIMALS))
        },
        |(sym, raw)| process_buy_item(wallet_arc.clone(), taker.clone(), sym, raw, json, slippage, max_bps, sol_lamports),
    )
    .await;

    let total_str = format!("{total_usdc_spent:.2}");
    let status = multi_status(bought.len(), failed.len());
    if json {
        json_out(&BuyBasketResultJson {
            gas_refuel,
            status,
            allocation,
            bought,
            failed,
            skipped: vec![],
            total_usdc_spent: total_str,
        })?;
        // `wallet_arc` wraps an ed25519 SigningKey that zeroizes its secret on
        // Drop; all run_swap_items tasks are joined by now, so this Arc is the
        // sole strong ref — drop it explicitly to zeroize the key before the
        // possible exit(1) below (see `exit_if_all_failed`'s doc comment:
        // `process::exit` runs no destructors, so this MUST happen first).
        if status == "error" {
            drop(wallet_arc);
        }
        exit_if_all_failed(status);
        return Ok(());
    }
    let label = if parallel { "Buy-basket (parallel)" } else { "Buy-basket" };
    println!("\n{} complete:", label);
    println!("  Bought:  {} tokens → {} USDC", bought.len(), total_usdc_spent);
    if !failed.is_empty() {
        println!("  Failed:  {} tokens", failed.len());
    }
    if status == "error" {
        return Err(eyre!("buy-basket: all {} order(s) failed", failed.len()));
    }
    Ok(())
}

async fn buy_basket_dry_run(
    taker: &str,
    symbol_raw: &[(String, String)],
    allocation: Option<AllocationJson>,
    parallel: bool,
    json: bool,
    slippage: Option<u32>,
    max_bps: Option<u32>,
) -> Result<()> {
    let taker = taker.to_string();
    let describe_ok = |order: &usecases::gm::BuyOrderReady| {
        format!(
            "~{} {} (spread {:.2}%)",
            amounts::format_amount(&order.order.out_amount, jupiter::GM_SOL_DECIMALS),
            order.symbol,
            order.slippage_pct.unwrap_or(0.0)
        )
    };
    let fetch = |(sym, raw): (String, String)| {
        let taker = taker.clone();
        async move {
            let result = usecases::gm::fetch_buy_order(&sym, &raw, &taker, json, slippage, max_bps, None).await;
            (sym, result)
        }
    };
    let (ready_orders, failed) = if parallel {
        fetch_orders_parallel(symbol_raw.to_vec(), json, "buy orders", jupiter::order_retry_count, describe_ok, fetch).await
    } else {
        fetch_orders_sequential(symbol_raw.to_vec(), json, describe_ok, fetch).await
    };

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
            gas_refuel: None,
            status: "dry_run",
            allocation,
            bought: preview,
            failed,
            skipped: vec![],
            total_usdc_spent: "0".to_string(),
        });
    }
    println!("\n[DRY RUN] Would buy:");
    for o in &ready_orders {
        let recv = amounts::format_amount(&o.order.out_amount, jupiter::GM_SOL_DECIMALS);
        println!(
            "  {} USDC → {} {}  (spread {:.2}%)",
            o.usdc_display,
            recv,
            o.symbol,
            o.slippage_pct.unwrap_or(0.0)
        );
    }
    if !failed.is_empty() {
        println!("  {} tokens would fail (see above)", failed.len());
    }
    Ok(())
}

// ── Sell basket ────────────────────────────────────────────

pub async fn sell_basket(
    tokens: &[String],
    opts: ExecOpts,
    parallel: bool,
    tuning: TradeTuning,
    rpc_url: Option<&str>,
    selected: Option<&str>,
) -> Result<()> {
    let ExecOpts { yes, dry_run, json } = opts;
    let TradeTuning { slippage, max_bps } = tuning;
    let pairs = parse_basket_pairs(tokens)?;

    let w = load_wallet(selected)?;
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
        if !require_execution_consent(yes, json, &prompt)? {
            println!("Cancelled.");
            return Ok(());
        }
    }

    if dry_run {
        return sell_basket_dry_run(&taker, &pairs, parallel, json, rpc_url, slippage, max_bps).await;
    }

    let wallet_arc = Arc::new(w);
    let rpc = rpc_url.map(str::to_string);
    let (sold, failed, total_usdc) = run_swap_items(
        pairs.clone(),
        parallel,
        json,
        "sell orders",
        jupiter::order_retry_count,
        |(sym, amt)| format!("Selling {} {} ...", sym, amt),
        |(sym, amt)| process_sell_item(wallet_arc.clone(), taker.clone(), sym, amt, json, rpc.clone(), slippage, max_bps),
    )
    .await;

    let total_str = format!("{total_usdc:.2}");
    let status = multi_status(sold.len(), failed.len());
    if json {
        json_out(&SellBasketResultJson {
            status,
            sold,
            failed,
            skipped: vec![],
            total_usdc_received: total_str,
        })?;
        // `wallet_arc` wraps an ed25519 SigningKey that zeroizes its secret on
        // Drop; all run_swap_items tasks are joined by now, so this Arc is the
        // sole strong ref — drop it explicitly to zeroize the key before the
        // possible exit(1) below (see `exit_if_all_failed`'s doc comment:
        // `process::exit` runs no destructors, so this MUST happen first).
        if status == "error" {
            drop(wallet_arc);
        }
        exit_if_all_failed(status);
        return Ok(());
    }
    let label = if parallel { "Sell-basket (parallel)" } else { "Sell-basket" };
    println!("\n{} complete:", label);
    println!("  Sold:    {} tokens → {:.2} USDC", sold.len(), total_usdc);
    if !failed.is_empty() {
        println!("  Failed:  {} tokens", failed.len());
    }
    if status == "error" {
        return Err(eyre!("sell-basket: all {} order(s) failed", failed.len()));
    }
    Ok(())
}

async fn sell_basket_dry_run(
    taker: &str,
    pairs: &[(String, String)],
    parallel: bool,
    json: bool,
    rpc_url: Option<&str>,
    slippage: Option<u32>,
    max_bps: Option<u32>,
) -> Result<()> {
    let taker = taker.to_string();
    let rpc = rpc_url.map(str::to_string);
    let describe_ok = |order: &usecases::gm::SellOrderReady| {
        format!(
            "~{} USDC (spread {:.2}%)",
            amounts::format_amount(&order.order.out_amount, jupiter::USDC_DECIMALS),
            order.slippage_pct.unwrap_or(0.0)
        )
    };
    let fetch = |(sym, amt): (String, String)| {
        let taker = taker.clone();
        let rpc = rpc.clone();
        async move {
            let result =
                usecases::gm::fetch_sell_order_by_symbol(&sym, &amt, &taker, json, rpc.as_deref(), slippage, max_bps).await;
            (sym, result)
        }
    };
    let (ready_orders, failed) = if parallel {
        fetch_orders_parallel(pairs.to_vec(), json, "sell orders", jupiter::order_retry_count, describe_ok, fetch).await
    } else {
        fetch_orders_sequential(pairs.to_vec(), json, describe_ok, fetch).await
    };

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
        println!(
            "  {} {} → {} USDC  (spread {:.2}%)",
            o.symbol,
            o.display_amount,
            usdc_out,
            o.slippage_pct.unwrap_or(0.0)
        );
    }
    if !failed.is_empty() {
        println!("  {} tokens would fail (see above)", failed.len());
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
            gas_refuel: None,
            status: "success",
            allocation: None,
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
                error_kind: Some("swap_rejected"),
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
        assert_eq!(json["failed"][0]["error_kind"], "swap_rejected");
        assert_eq!(json["bought"][0]["token"], "JNJon");
        assert_eq!(json["total_usdc_spent"], "15.00");
    }

    // buy-basket dry-run: tx is empty string, total_usdc_spent is "0".
    #[test]
    fn buy_basket_dry_run_has_zero_total_and_empty_tx() {
        let result = BuyBasketResultJson {
            gas_refuel: None,
            status: "dry_run",
            allocation: None,
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
        assert!(json.get("allocation").is_none(), "absent without --total");
    }

    // --total runs echo the allocation; plain runs omit the field entirely.
    #[test]
    fn buy_basket_json_allocation_echo() {
        let mut weights = std::collections::BTreeMap::new();
        weights.insert("TSLA".to_string(), "50".to_string());
        weights.insert("NVDA".to_string(), "50".to_string());
        let result = BuyBasketResultJson {
            gas_refuel: None,
            status: "dry_run",
            allocation: Some(AllocationJson { total: "1000".to_string(), weights }),
            bought: vec![],
            failed: vec![],
            skipped: vec![],
            total_usdc_spent: "0".to_string(),
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["allocation"]["total"], "1000");
        assert_eq!(json["allocation"]["weights"]["TSLA"], "50");
        assert_eq!(json["allocation"]["weights"]["NVDA"], "50");
    }

    // A weight with a space before the '%' (e.g. "50 %") must echo as "50",
    // not "50 " — trim_end_matches('%') alone leaves the inner space behind.
    #[test]
    fn allocation_weight_echo_trims_trailing_space_around_percent_sign() {
        assert_eq!(echo_weight("50 %"), "50");
        assert_eq!(echo_weight("50%"), "50");
        assert_eq!(echo_weight(" 12.5 % "), "12.5");
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
                error_kind: Some("not_tradable"),
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
        assert_eq!(json["failed"][0]["error_kind"], "not_tradable");
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

    // resolve_buy_items: --total splits percent weights via the ondo helper.
    #[test]
    fn resolve_buy_items_splits_total() {
        let pairs = vec![
            ("TSLA".to_string(), "50%".to_string()),
            ("NVDA".to_string(), "50%".to_string()),
        ];
        let items = resolve_buy_items(&pairs, Some("100")).unwrap();
        assert_eq!(items[0], ("TSLA".to_string(), 50_000_000u128));
        assert_eq!(items[1], ("NVDA".to_string(), 50_000_000u128));
    }

    // resolve_buy_items: percent amounts WITHOUT --total are a typed error
    // pointing at the flag (previously an opaque parse error).
    #[test]
    fn resolve_buy_items_percent_without_total_is_typed_err() {
        let pairs = vec![("TSLA".to_string(), "50%".to_string())];
        let err = resolve_buy_items(&pairs, None).unwrap_err();
        assert_eq!(usecases::gm::classify_error(&err), Some("invalid_amount"));
        assert!(err.to_string().contains("--total"), "{err}");
    }

    // resolve_buy_items: absolute amounts without --total keep working.
    #[test]
    fn resolve_buy_items_absolute_amounts_unchanged() {
        let pairs = vec![
            ("AAPL".to_string(), "4".to_string()),
            ("TSLA".to_string(), "3.50".to_string()),
        ];
        let items = resolve_buy_items(&pairs, None).unwrap();
        assert_eq!(items[0].1, 4_000_000u128);
        assert_eq!(items[1].1, 3_500_000u128);
    }

    #[test]
    fn resolve_equal_items_splits_total_evenly() {
        let out = resolve_equal_items(
            &["TSLA".to_string(), "NVDA".to_string(), "SPY".to_string()],
            Some("999"),
        )
        .unwrap();
        assert_eq!(out.iter().map(|(_, r)| *r).sum::<u128>(), 999_000_000);
        assert_eq!(out[0].0, "TSLA");
    }

    #[test]
    fn resolve_equal_items_requires_total() {
        let err = resolve_equal_items(&["TSLA".to_string()], None).unwrap_err();
        assert_eq!(usecases::gm::classify_error(&err), Some("invalid_amount"));
        assert!(err.to_string().contains("--total"), "{err}");
    }

    #[test]
    fn resolve_equal_items_rejects_amounts_in_tokens() {
        // With --equal the tokens are bare symbols; a "50%" or number is an error.
        let err = resolve_equal_items(&["TSLA".to_string(), "50%".to_string()], Some("100")).unwrap_err();
        assert_eq!(usecases::gm::classify_error(&err), Some("invalid_amount"));
    }

    #[test]
    fn resolve_equal_items_rejects_all_keyword() {
        // "all" is reserved amount syntax, not a bare symbol; reject pre-network.
        let err = resolve_equal_items(&["TSLA".to_string(), "all".to_string()], Some("12")).unwrap_err();
        assert_eq!(usecases::gm::classify_error(&err), Some("invalid_amount"));
    }

    #[test]
    fn read_tokens_from_file_strips_comments_and_blanks() {
        let dir = std::env::temp_dir().join(format!("rwa-basket-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("basket.txt");
        std::fs::write(&p, "# my basket\nTSLA 50%\n\n  NVDA 50%  \n# trailing comment\n").unwrap();
        let toks = read_basket_tokens(&[], Some(p.to_str().unwrap())).unwrap();
        assert_eq!(toks, vec!["TSLA", "50%", "NVDA", "50%"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_tokens_rejects_both_sources() {
        let err = read_basket_tokens(&["TSLA".to_string(), "10".to_string()], Some("basket.txt")).unwrap_err();
        assert_eq!(usecases::gm::classify_error(&err), Some("invalid_amount"));
        assert!(err.to_string().contains("--from-file"), "{err}");
    }

    #[test]
    fn read_tokens_empty_file_is_error() {
        let dir = std::env::temp_dir().join(format!("rwa-basket-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("empty.txt");
        std::fs::write(&p, "# only a comment\n\n").unwrap();
        let err = read_basket_tokens(&[], Some(p.to_str().unwrap())).unwrap_err();
        assert_eq!(usecases::gm::classify_error(&err), Some("invalid_amount"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_tokens_argv_passthrough() {
        let toks = read_basket_tokens(&["AAPL".to_string(), "10".to_string()], None).unwrap();
        assert_eq!(toks, vec!["AAPL", "10"]);
    }
}
