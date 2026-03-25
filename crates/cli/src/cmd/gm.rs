use chrono::Datelike;
use clap::Subcommand;
use eyre::Result;
use rwa_ondo::{api, gm, jupiter, solana, token_list, wallet};
use serde::Serialize;
use std::io::{self, Write};

// ── Subcommand enum ────────────────────────────────────────

#[derive(Subcommand, Debug)]
pub enum GmAction {
    /// Check if Ondo GM market is open (24/5: Sun 8pm -- Fri 8pm ET)
    Hours,

    /// Get swap quote for a GM token via Jupiter
    Quote {
        /// Token symbol (e.g. TSLA, TSLAon)
        symbol: String,
        /// USDC amount to spend (or token amount with --sell)
        amount: String,
        /// Quote selling token for USDC instead of buying
        #[arg(long)]
        sell: bool,
    },

    /// Buy GM token with USDC via Jupiter (Solana)
    Buy {
        /// Token symbol (e.g. TSLA, TSLAon)
        symbol: String,
        /// USDC amount, percentage of balance, or "all" (e.g. 100, 50%, all)
        amount: String,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },

    /// Sell GM token for USDC via Jupiter (Solana)
    Sell {
        /// Token symbol (e.g. TSLA, TSLAon)
        symbol: String,
        /// Token amount, percentage of holdings, or "all" (e.g. 5, 50%, all)
        amount: String,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },

    /// Portfolio positions and P&L (Solana)
    Portfolio {
        /// Wallet address (default: local wallet)
        wallet: Option<String>,
    },

    /// Price history for a GM token (1D, 1W, 1M, 3M, 1Y, ALL)
    History {
        /// Token symbol (e.g. TSLA, TSLAon)
        symbol: String,
        /// Time range: 1D, 1W, 1M, 3M, 1Y, ALL (default: 1M)
        #[arg(short, long, default_value = "1M")]
        range: String,
    },

    /// List all available GM tokens
    List,

    /// Send SOL, USDC, or GM tokens to another wallet
    Send {
        /// What to send: SOL, USDC, or token symbol (e.g. TSLA)
        token: String,
        /// Amount to send (e.g. 100, 50%, all)
        amount: String,
        /// Recipient Solana address
        to: String,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },
}

pub async fn execute(action: GmAction, json: bool, rpc_url: Option<&str>) -> Result<()> {
    match action {
        GmAction::Hours => hours(json),
        GmAction::Quote { symbol, amount, sell } => quote(&symbol, &amount, sell, json, rpc_url).await,
        GmAction::Buy { symbol, amount, yes } => buy(&symbol, &amount, yes, json, rpc_url).await,
        GmAction::Sell { symbol, amount, yes } => sell(&symbol, &amount, yes, json, rpc_url).await,
        GmAction::Portfolio { wallet } => portfolio(wallet.as_deref(), json, rpc_url).await,
        GmAction::History { symbol, range } => history(&symbol, &range, json).await,
        GmAction::List => list(json).await,
        GmAction::Send { token, amount, to, yes } => send(&token, &amount, &to, yes, json, rpc_url).await,
    }
}

// ── JSON output types ──────────────────────────────────────

#[derive(Serialize)]
struct HoursJson {
    status: &'static str,
    now: String,
    countdown: String,
}

#[derive(Serialize)]
struct QuoteJson {
    input: String,
    input_token: String,
    output: String,
    output_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    slippage_pct: Option<f64>,
}

#[derive(Serialize)]
struct TradeJson {
    status: &'static str,
    amount: String,
    token: String,
    counter_amount: String,
    counter_token: &'static str,
    tx: String,
}

#[derive(Serialize)]
struct PositionJson {
    token: String,
    balance: f64,
    price: f64,
    value_usd: f64,
    alloc_pct: f64,
    change_pct_24h: f64,
}

#[derive(Serialize)]
struct PortfolioJson {
    wallet: String,
    sol: f64,
    usdc: f64,
    positions: Vec<PositionJson>,
    total_value_usd: f64,
    change_24h_usd: f64,
    change_24h_pct: f64,
}

#[derive(Serialize)]
struct HistoryJson {
    symbol: String,
    range: String,
    candles: usize,
    first: HistoryCandleJson,
    last: HistoryCandleJson,
    high: f64,
    low: f64,
    change_pct: f64,
}

#[derive(Serialize)]
struct HistoryCandleJson {
    timestamp: u64,
    price: f64,
}

#[derive(Serialize)]
struct ListItemJson<'a> {
    symbol: &'a str,
    name: &'a str,
    mint: &'a str,
}

fn json_out(v: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(v)?);
    Ok(())
}

// ── Public command handlers ─────────────────────────────────

pub fn hours(json: bool) -> Result<()> {
    use chrono::Timelike;
    use chrono_tz::US::Eastern;

    let now = chrono::Utc::now().with_timezone(&Eastern);
    let wd = now.weekday();
    let hour = now.hour();
    let min = now.minute();

    let closed = !is_market_open();

    let time_str = now.format("%A %I:%M %p ET").to_string();

    let (status, countdown) = if closed {
        let days_until_sun: u32 = match wd {
            chrono::Weekday::Fri => 2,
            chrono::Weekday::Sat => 1,
            chrono::Weekday::Sun => 0,
            _ => 0,
        };
        let mins_left = if wd == chrono::Weekday::Sun {
            (20 - hour) * 60 - min
        } else {
            let to_midnight = (24 - hour) * 60 - min;
            let full_days = days_until_sun.saturating_sub(1);
            to_midnight + full_days * 24 * 60 + 20 * 60
        };
        ("closed", format!("opens in {}h {}m", mins_left / 60, mins_left % 60))
    } else {
        let days_until_fri = match wd {
            chrono::Weekday::Sun => 5,
            chrono::Weekday::Mon => 4,
            chrono::Weekday::Tue => 3,
            chrono::Weekday::Wed => 2,
            chrono::Weekday::Thu => 1,
            chrono::Weekday::Fri => 0,
            chrono::Weekday::Sat => 6,
        };
        let mins_left = if days_until_fri == 0 {
            (20 - hour) * 60 - min
        } else {
            let to_midnight = (24 - hour) * 60 - min;
            to_midnight + (days_until_fri - 1) * 24 * 60 + 20 * 60
        };
        ("open", format!("closes in {}h {}m", mins_left / 60, mins_left % 60))
    };

    if json {
        return json_out(&HoursJson { status, now: time_str, countdown });
    }

    let label = if closed { "CLOSED" } else { "OPEN" };
    println!("{}", label);
    println!("  Now:     {}", time_str);
    println!("  Hours:   Sunday 8:00 PM -- Friday 8:00 PM ET");
    let cap = if closed { "Opens" } else { "Closes" };
    let cd = countdown.trim_start_matches("opens in ").trim_start_matches("closes in ");
    println!("  {} in: {}", cap, cd);
    Ok(())
}

pub async fn portfolio(wallet: Option<&str>, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let tokens = token_list::get_token_list().await;
    let pubkey = match wallet {
        Some(w) => {
            // Basic validation: base58, reasonable length for Solana address
            if w.len() < 32 || w.len() > 44 || w.chars().any(|c| c.is_whitespace()) {
                return Err(eyre::eyre!("Invalid Solana address: {w}"));
            }
            w.to_string()
        }
        None => load_wallet()?.pubkey(),
    };

    let (portfolio_bal, assets) = tokio::join!(
        solana::get_portfolio_balances(&pubkey, &tokens, rpc_url),
        api::fetch_assets()
    );
    let portfolio_bal = portfolio_bal?;
    let assets = assets?;
    let sol_bal = portfolio_bal.sol;
    let usdc_bal = portfolio_bal.usdc;
    let balances = portfolio_bal.gm_tokens;

    let mut positions = Vec::new();
    let mut total_value = 0.0;
    let mut total_prev_value = 0.0;

    for tb in &balances {
        let asset = api::find_asset(&tb.symbol, &assets);
        let (price, pct_24h) = match asset.and_then(|a| a.primary_market.as_ref()) {
            Some(pm) => {
                let p = api::parse_price(&pm.price);
                let pct = pm.price_change_pct_24h.as_deref().map(api::parse_price).unwrap_or(0.0);
                (p, pct)
            }
            None => (0.0, 0.0),
        };
        let value = tb.balance * price;
        let prev_value = if pct_24h.abs() > f64::EPSILON {
            value / (1.0 + pct_24h / 100.0)
        } else {
            value
        };
        total_value += value;
        total_prev_value += prev_value;
        positions.push(PositionJson {
            token: tb.symbol.clone(),
            balance: tb.balance,
            price,
            value_usd: value,
            alloc_pct: 0.0,
            change_pct_24h: pct_24h,
        });
    }

    // Compute allocation % for each position
    if total_value.abs() > f64::EPSILON {
        for p in &mut positions {
            p.alloc_pct = (p.value_usd / total_value) * 100.0;
        }
    }

    // Sort by value descending
    positions.sort_by(|a, b| b.value_usd.partial_cmp(&a.value_usd).unwrap_or(std::cmp::Ordering::Equal));

    let total_change = total_value - total_prev_value;
    let total_pct = if total_prev_value.abs() > f64::EPSILON {
        (total_change / total_prev_value) * 100.0
    } else {
        0.0
    };

    if json {
        return json_out(&PortfolioJson {
            wallet: pubkey.clone(),
            sol: sol_bal,
            usdc: usdc_bal,
            positions,
            total_value_usd: total_value,
            change_24h_usd: total_change,
            change_24h_pct: total_pct,
        });
    }

    println!("Portfolio for {pubkey}\n");
    println!("  SOL:   {:.6}", sol_bal);
    println!("  USDC:  {:.2}", usdc_bal);

    if positions.is_empty() {
        println!("\nNo GM token positions.");
        return Ok(());
    }

    println!();
    println!(
        "{:<10} {:>12} {:>10} {:>12} {:>8} {:>8}",
        "TOKEN", "BALANCE", "PRICE", "VALUE", "ALLOC", "24h"
    );
    println!("{}", "-".repeat(64));

    for p in &positions {
        println!(
            "{:<10} {:>12.4} {:>10.2} {:>11.2} {:>7.1}% {:>+7.2}%",
            p.token, p.balance, p.price, p.value_usd, p.alloc_pct, p.change_pct_24h
        );
    }

    println!("{}", "-".repeat(64));
    println!(
        "{:<10} {:>12} {:>10} {:>11.2} {:>7} {:>+7.2}%",
        "TOTAL", "", "", total_value, "", total_pct
    );

    Ok(())
}

// ── Helpers ────────────────────────────────────────────────

fn resolve_gm_mint(symbol: &str, tokens: &[token_list::GmTokenEntry]) -> Result<(String, String)> {
    let entry = gm::resolve_token(symbol, tokens)?;
    let mint = entry.solana_address.as_deref()
        .ok_or_else(|| eyre::eyre!("No Solana address for {}", entry.symbol))?;
    Ok((entry.symbol.clone(), mint.to_string()))
}

fn load_wallet() -> Result<wallet::Wallet> {
    wallet::Wallet::load_default().map_err(|_| {
        eyre::eyre!(
            "No wallet found.\n\n\
             Create or import one first:\n  \
             rwa keys generate                          Create a new wallet\n  \
             rwa keys import --seed-phrase \"word1 ...\"   Import from seed phrase\n  \
             rwa keys import --private-key <BASE58>     Import from private key\n  \
             rwa keys import --file <PATH>              Import from key file"
        )
    })
}

/// Minimum SOL needed for transaction fees (~0.005 SOL).
const MIN_SOL_FOR_GAS: f64 = 0.005;
/// Minimum buy/sell amount in USDC.
const MIN_USDC_AMOUNT: f64 = 1.0;

/// Resolve percentage or "all" amounts into absolute numbers.
///  - "100" → 100.0 (passthrough)
///  - "50%" → 50% of balance
///  - "all" → 100% of balance////// `balance_fn` is called lazily only when needed.
async fn resolve_percent_amount<F, Fut>(raw: &str, balance_fn: F) -> Result<f64>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<f64>>,
{
    let s = raw.trim();
    if s.eq_ignore_ascii_case("all") {
        let bal = balance_fn().await?;
        if bal <= 0.0 {
            return Err(eyre::eyre!("Balance is 0 — nothing to trade"));
        }
        return Ok(bal);
    }
    if let Some(pct_str) = s.strip_suffix('%') {
        let pct: f64 = pct_str.parse().map_err(|_| eyre::eyre!("Invalid percentage: {s}"))?;
        if !(0.0..=100.0).contains(&pct) {
            return Err(eyre::eyre!("Percentage must be 0–100, got {pct}"));
        }
        let bal = balance_fn().await?;
        if bal <= 0.0 {
            return Err(eyre::eyre!("Balance is 0 — nothing to trade"));
        }
        return Ok(bal * pct / 100.0);
    }
    s.parse::<f64>().map_err(|_| eyre::eyre!("Invalid amount: {s}"))
}

/// Check if Ondo GM trading is currently open.
/// Trading window: Sunday 8 PM ET → Friday 8 PM ET (24/5).
fn is_market_open() -> bool {
    use chrono::Timelike;
    use chrono_tz::US::Eastern;

    let now = chrono::Utc::now().with_timezone(&Eastern);
    let wd = now.weekday();
    let hour = now.hour();

    !(matches!(wd, chrono::Weekday::Sat)
        || (wd == chrono::Weekday::Sun && hour < 20)
        || (wd == chrono::Weekday::Fri && hour >= 20))
}

fn check_trading_hours() -> Result<()> {
    if !is_market_open() {
        use chrono_tz::US::Eastern;
        let now = chrono::Utc::now().with_timezone(&Eastern);
        return Err(eyre::eyre!(
            "Ondo GM market is closed right now.\n  \
             Trading hours: Sunday 8 PM — Friday 8 PM ET (24/5)\n  \
             Current time:  {} ET",
            now.format("%A %I:%M %p")
        ));
    }
    Ok(())
}

async fn preflight_buy(pubkey: &str, usdc_amount: f64, rpc_url: Option<&str>) -> Result<()> {
    check_trading_hours()?;

    if usdc_amount < MIN_USDC_AMOUNT {
        return Err(eyre::eyre!("Minimum buy amount is {MIN_USDC_AMOUNT} USDC"));
    }

    let sol = solana::get_sol_balance(pubkey, rpc_url).await?;
    let usdc = solana::get_usdc_balance(pubkey, rpc_url).await?;

    let mut issues = Vec::new();
    if sol < MIN_SOL_FOR_GAS {
        issues.push(format!(
            "Insufficient SOL for gas: {sol:.6} SOL (need ≥{MIN_SOL_FOR_GAS})"
        ));
    }
    if usdc < usdc_amount {
        issues.push(format!(
            "Insufficient USDC: {usdc:.2} USDC (need {usdc_amount:.2})"
        ));
    }
    if !issues.is_empty() {
        let mut msg = issues.join("\n  ");
        msg.push_str(&format!("\n  Fund wallet: {pubkey}"));
        return Err(eyre::eyre!(msg));
    }
    Ok(())
}

async fn preflight_sell(pubkey: &str, rpc_url: Option<&str>) -> Result<()> {
    check_trading_hours()?;

    let sol = solana::get_sol_balance(pubkey, rpc_url).await?;
    if sol < MIN_SOL_FOR_GAS {
        return Err(eyre::eyre!(
            "Insufficient SOL for gas: {sol:.6} SOL (need ≥{MIN_SOL_FOR_GAS})\n  \
             Fund wallet: {pubkey}"
        ));
    }
    Ok(())
}

pub async fn quote(symbol: &str, amount: &str, is_sell: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let tokens = token_list::get_token_list().await;
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
        });
    }

    println!("Quote: {} {} -> {} {}", in_fmt, in_label, out_fmt, out_label);
    if let (Some(usd_in), Some(usd_out), Some(slip)) = (input_usd, output_usd, slippage) {
        println!("  Input value:   ${:.2}", usd_in);
        println!("  Output value:  ${:.2}", usd_out);
        println!("  Slippage:      {:.2}%", slip);
    }

    Ok(())
}

fn confirm(msg: &str) -> bool {
    print!("{msg} [y/N] ");
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

pub async fn buy(symbol: &str, amount: &str, yes: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let tokens = token_list::get_token_list().await;
    let (sym, gm_mint) = resolve_gm_mint(symbol, &tokens)?;
    let w = load_wallet()?;
    let taker = w.pubkey();

    let usdc_f = resolve_percent_amount(amount, || {
        let t = taker.clone();
        let rpc = rpc_url.map(str::to_string);
        async move { solana::get_usdc_balance(&t, rpc.as_deref()).await }
    }).await?;
    let usdc_str = format!("{usdc_f:.2}");

    preflight_buy(&taker, usdc_f, rpc_url).await?;

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
    if let (Some(usd_in), Some(usd_out)) = (order.in_usd_value, order.out_usd_value) {
        if usd_in > 0.0 {
            let slippage = (usd_out - usd_in) / usd_in * 100.0;
            if slippage < -1.0 {
                println!("Warning: high slippage: {:.2}%", slippage);
            }
        }
    }

    if !yes && !json && !confirm("Proceed?") {
        println!("Cancelled.");
        return Ok(());
    }

    if !json {
        println!("Executing swap...");
    }
    let result = jupiter::execute_order(&w, &order).await?;

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
        });
    }

    println!("\nSwap successful!");
    println!("  Bought:    {} {}", final_out, sym);
    println!("  Spent:     {} USDC", usdc_str);
    println!("  Tx:        https://solscan.io/tx/{}", sig);

    Ok(())
}

pub async fn sell(symbol: &str, amount: &str, yes: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let tokens = token_list::get_token_list().await;
    let (sym, gm_mint) = resolve_gm_mint(symbol, &tokens)?;
    let w = load_wallet()?;
    let taker = w.pubkey();

    preflight_sell(&taker, rpc_url).await?;

    let gm_dec = jupiter::GM_SOL_DECIMALS;

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

    let raw_gm = jupiter::token_to_raw(&sell_str, gm_dec)?;

    if !json {
        println!("Getting quote for {} {} -> USDC ...", sell_str, sym);
    }
    let order = jupiter::get_order(&gm_mint, jupiter::USDC_MINT, &raw_gm, &taker).await?;

    let out_fmt = jupiter::format_amount(&order.out_amount, jupiter::USDC_DECIMALS);
    if !json {
        println!("You will receive ~{} USDC", out_fmt);
    }
    if let (Some(usd_in), Some(usd_out)) = (order.in_usd_value, order.out_usd_value) {
        if usd_in > 0.0 {
            let slippage = (usd_out - usd_in) / usd_in * 100.0;
            if slippage < -1.0 {
                println!("Warning: high slippage: {:.2}%", slippage);
            }
        }
    }

    if !yes && !json && !confirm("Proceed?") {
        println!("Cancelled.");
        return Ok(());
    }

    if !json {
        println!("Executing swap...");
    }
    let result = jupiter::execute_order(&w, &order).await?;

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
        });
    }

    println!("\nSwap successful!");
    println!("  Sold:      {} {}", sell_str, sym);
    println!("  Received:  {} USDC", final_out);
    println!("  Tx:        https://solscan.io/tx/{}", sig);

    Ok(())
}

async fn history(symbol: &str, range: &str, json: bool) -> Result<()> {
    let candles = api::fetch_history(symbol, range).await?;
    if candles.is_empty() {
        return Err(eyre::eyre!("No price history for {} (range: {})", symbol, range));
    }

    let sym = symbol.to_uppercase();
    let sym = if sym.ends_with("ON") { sym } else { format!("{sym}ON") };

    let first = candles.first().unwrap();
    let last = candles.last().unwrap();
    let high = candles.iter().map(|c| c.high).fold(f64::NEG_INFINITY, f64::max);
    let low = candles.iter().map(|c| c.low).fold(f64::INFINITY, f64::min);
    let change_pct = if first.open > 0.0 {
        (last.close - first.open) / first.open * 100.0
    } else {
        0.0
    };

    if json {
        return json_out(&HistoryJson {
            symbol: sym,
            range: range.to_uppercase(),
            candles: candles.len(),
            first: HistoryCandleJson { timestamp: first.timestamp, price: first.open },
            last: HistoryCandleJson { timestamp: last.timestamp, price: last.close },
            high,
            low,
            change_pct,
        });
    }

    println!("{} Price History ({})", sym, range.to_uppercase());
    println!("{}", "-".repeat(50));
    println!("  Period:    {} candles", candles.len());
    println!("  Open:      ${:.2}", first.open);
    println!("  Close:     ${:.2}", last.close);
    println!("  High:      ${:.2}", high);
    println!("  Low:       ${:.2}", low);
    println!("  Change:    {:+.2}%", change_pct);

    Ok(())
}

async fn list(json: bool) -> Result<()> {
    let tokens = token_list::get_token_list().await;

    if json {
        let items: Vec<_> = tokens.iter().map(|t| ListItemJson {
            symbol: &t.symbol,
            name: &t.name,
            mint: t.solana_address.as_deref().unwrap_or(""),
        }).collect();
        return json_out(&items);
    }

    println!("{} GM tokens available\n", tokens.len());
    for t in &tokens {
        if t.name.is_empty() {
            println!("  {}", t.symbol);
        } else {
            println!("  {:<12} {}", t.symbol, t.name);
        }
    }
    Ok(())
}

// ── Send ───────────────────────────────────────────────────

#[derive(Serialize)]
struct SendJson {
    status: &'static str,
    token: String,
    amount: String,
    recipient: String,
    tx: String,
}

async fn send(token: &str, amount: &str, to: &str, yes: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let w = load_wallet()?;
    let pubkey = w.pubkey();

    // Validate recipient address (base58, 32-44 chars typical for Solana)
    if to.len() < 32 || to.len() > 44 {
        return Err(eyre::eyre!("Invalid recipient address: {to}"));
    }
    if to == pubkey {
        return Err(eyre::eyre!("Cannot send to yourself"));
    }

    let token_upper = token.to_uppercase();

    match token_upper.as_str() {
        "SOL" => send_sol(&w, amount, to, yes, json, rpc_url).await,
        "USDC" => send_usdc(&w, amount, to, yes, json, rpc_url).await,
        _ => send_gm_token(&w, &token_upper, amount, to, yes, json, rpc_url).await,
    }
}

async fn send_sol(w: &wallet::Wallet, amount: &str, to: &str, yes: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let pubkey = w.pubkey();

    let sol_f = resolve_percent_amount(amount, || {
        let pk = pubkey.clone();
        let rpc = rpc_url.map(String::from);
        async move { solana::get_sol_balance(&pk, rpc.as_deref()).await }
    }).await?;

    if sol_f < MIN_SOL_FOR_GAS {
        return Err(eyre::eyre!("Amount too small — need to keep ≥{MIN_SOL_FOR_GAS} SOL for gas"));
    }

    // Keep enough for gas if sending "all"
    let send_amount = if amount == "all" || amount == "100%" {
        let bal = solana::get_sol_balance(&pubkey, rpc_url).await?;
        let keep = MIN_SOL_FOR_GAS;
        if bal <= keep {
            return Err(eyre::eyre!("Balance too low — need ≥{keep} SOL for gas"));
        }
        bal - keep
    } else {
        sol_f
    };

    if !json {
        println!("Send {:.6} SOL → {to}", send_amount);
    }
    if !yes && !json && !confirm("Proceed?") {
        return Err(eyre::eyre!("Cancelled"));
    }

    let sig = solana::transfer_sol(w, to, send_amount, rpc_url).await?;

    if json {
        return json_out(&SendJson {
            status: "success",
            token: "SOL".into(),
            amount: format!("{send_amount:.6}"),
            recipient: to.into(),
            tx: format!("https://solscan.io/tx/{sig}"),
        });
    }
    println!("✓ Sent {:.6} SOL → {to}", send_amount);
    println!("  https://solscan.io/tx/{sig}");
    Ok(())
}

async fn send_usdc(w: &wallet::Wallet, amount: &str, to: &str, yes: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let pubkey = w.pubkey();

    // Check SOL for gas
    let sol = solana::get_sol_balance(&pubkey, rpc_url).await?;
    if sol < MIN_SOL_FOR_GAS {
        return Err(eyre::eyre!("Insufficient SOL for gas (have {sol:.4}, need ≥{MIN_SOL_FOR_GAS})"));
    }

    let usdc_f = resolve_percent_amount(amount, || {
        let pk = pubkey.clone();
        let rpc = rpc_url.map(String::from);
        async move { solana::get_usdc_balance(&pk, rpc.as_deref()).await }
    }).await?;

    if usdc_f <= 0.0 {
        return Err(eyre::eyre!("USDC balance is 0"));
    }

    let raw = (usdc_f * 1_000_000.0) as u64;

    if !json {
        println!("Send {usdc_f:.2} USDC → {to}");
    }
    if !yes && !json && !confirm("Proceed?") {
        return Err(eyre::eyre!("Cancelled"));
    }

    let sig = solana::transfer_spl(
        w, to, solana::USDC_MINT, raw, 6, false, rpc_url
    ).await?;

    if json {
        return json_out(&SendJson {
            status: "success",
            token: "USDC".into(),
            amount: format!("{usdc_f:.2}"),
            recipient: to.into(),
            tx: format!("https://solscan.io/tx/{sig}"),
        });
    }
    println!("✓ Sent {usdc_f:.2} USDC → {to}");
    println!("  https://solscan.io/tx/{sig}");
    Ok(())
}

async fn send_gm_token(w: &wallet::Wallet, symbol: &str, amount: &str, to: &str, yes: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let pubkey = w.pubkey();
    let tokens = token_list::get_token_list().await;
    let (sym, gm_mint) = resolve_gm_mint(symbol, &tokens)?;

    // Check SOL for gas
    let sol = solana::get_sol_balance(&pubkey, rpc_url).await?;
    if sol < MIN_SOL_FOR_GAS {
        return Err(eyre::eyre!("Insufficient SOL for gas (have {sol:.4}, need ≥{MIN_SOL_FOR_GAS})"));
    }

    let token_f = resolve_percent_amount(amount, || {
        let pk = pubkey.clone();
        let mint = gm_mint.clone();
        let rpc = rpc_url.map(String::from);
        async move {
            let b = solana::get_balance(&pk, &mint, rpc.as_deref()).await?;
            Ok(b.balance)
        }
    }).await?;

    if token_f <= 0.0 {
        return Err(eyre::eyre!("Balance is 0 — nothing to send"));
    }

    // GM tokens use 9 decimals (Token-2022)
    let raw = (token_f * 1_000_000_000.0) as u64;

    if !json {
        println!("Send {token_f:.6} {sym} → {to}");
    }
    if !yes && !json && !confirm("Proceed?") {
        return Err(eyre::eyre!("Cancelled"));
    }

    let sig = solana::transfer_spl(
        w, to, &gm_mint, raw, 9, true, rpc_url  // true = Token-2022
    ).await?;

    if json {
        return json_out(&SendJson {
            status: "success",
            token: sym.clone(),
            amount: format!("{token_f:.6}"),
            recipient: to.into(),
            tx: format!("https://solscan.io/tx/{sig}"),
        });
    }
    println!("✓ Sent {token_f:.6} {sym} → {to}");
    println!("  https://solscan.io/tx/{sig}");
    Ok(())
}
