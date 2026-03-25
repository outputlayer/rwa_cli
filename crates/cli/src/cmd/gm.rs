use chrono::Datelike;
use clap::Subcommand;
use eyre::Result;
use rwa_ondo::{api, gm, jupiter, solana, token_list, wallet};
use serde::Serialize;
use std::io::{self, Write};

// ── Subcommand enum ────────────────────────────────────────

#[derive(Subcommand, Debug)]
pub enum GmAction {
    /// Check market status, current session, and tradable tokens
    Hours {
        /// Show full list of tradable tokens (omitted by default in --json)
        #[arg(long)]
        tradable: bool,
    },

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

    /// List available GM tokens (use --search to filter)
    List {
        /// Filter tokens by keyword (searches symbol and name)
        #[arg(short, long)]
        search: Option<String>,
    },

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

    /// Close all GM positions — sell every token for USDC sequentially
    CloseAll {
        /// Percentage of each position to sell (e.g. 10%, 50%). Sells 100% if omitted.
        amount: Option<String>,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },
}

pub async fn execute(action: GmAction, json: bool, rpc_url: Option<&str>) -> Result<()> {
    match action {
        GmAction::Hours { tradable } => hours(json, tradable).await,
        GmAction::Quote { symbol, amount, sell } => quote(&symbol, &amount, sell, json, rpc_url).await,
        GmAction::Buy { symbol, amount, yes } => buy(&symbol, &amount, yes, json, rpc_url).await,
        GmAction::Sell { symbol, amount, yes } => sell(&symbol, &amount, yes, json, rpc_url).await,
        GmAction::Portfolio { wallet } => portfolio(wallet.as_deref(), json, rpc_url).await,
        GmAction::History { symbol, range } => history(&symbol, &range, json).await,
        GmAction::List { search } => list(json, search.as_deref()).await,
        GmAction::Send { token, amount, to, yes } => send(&token, &amount, &to, yes, json, rpc_url).await,
        GmAction::CloseAll { amount, yes } => close_all(amount.as_deref(), yes, json, rpc_url).await,
    }
}

// ── JSON output types ──────────────────────────────────────

#[derive(Serialize)]
struct HoursJson {
    status: &'static str,
    session: &'static str,
    session_hours: &'static str,
    now: String,
    countdown: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tradable_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tradable: Option<Vec<String>>,
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
    /// Price impact from Jupiter API (negative = loss).
    #[serde(skip_serializing_if = "Option::is_none")]
    price_impact_pct: Option<f64>,
    /// Jupiter fee in basis points.
    #[serde(skip_serializing_if = "Option::is_none")]
    fee_bps: Option<u32>,
}

#[derive(Serialize)]
struct TradeJson {
    status: &'static str,
    amount: String,
    token: String,
    counter_amount: String,
    counter_token: &'static str,
    tx: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    slippage_pct: Option<f64>,
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
struct ListItemJson {
    symbol: String,
    name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    sector: Option<String>,
    /// Whether this token is tradable in the current session.
    tradable: bool,
}

fn json_out(v: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(v)?);
    Ok(())
}

// ── Public command handlers ─────────────────────────────────

pub async fn hours(json: bool, show_tradable: bool) -> Result<()> {
    use chrono::Timelike;
    use chrono_tz::US::Eastern;

    let now = chrono::Utc::now().with_timezone(&Eastern);
    let wd = now.weekday();
    let hour = now.hour();
    let min = now.minute();

    let session = api::current_session();
    let closed = session == api::Session::Closed;

    let time_str = now.format("%A %I:%M %p ET").to_string();

    // Countdown to next session boundary
    let countdown = if closed {
        let days_until_sun: u32 = match wd {
            chrono::Weekday::Fri => 2,
            chrono::Weekday::Sat => 1,
            chrono::Weekday::Sun => 0,
            _ => 0,
        };
        let mins_left = if wd == chrono::Weekday::Sun {
            (20u32).saturating_sub(hour) * 60 - min
        } else {
            let to_midnight = (24 - hour) * 60 - min;
            let full_days = days_until_sun.saturating_sub(1);
            to_midnight + full_days * 24 * 60 + 20 * 60
        };
        format!("opens in {}h {}m", mins_left / 60, mins_left % 60)
    } else {
        // Minutes until current session ends
        let session_end_hour: u32 = match session {
            api::Session::PreMarket => 9,    // ends at 9:30
            api::Session::Regular => 16,     // ends at 16:00
            api::Session::PostMarket => 20,  // ends at 20:00
            api::Session::Overnight => 4,    // ends at 4:00
            _ => 0,
        };
        let session_end_min: u32 = if session == api::Session::PreMarket { 30 } else { 0 };

        let mins_left = if session == api::Session::Overnight && hour >= 20 {
            // 20:00–23:59 → ends at 4:00 next day
            (24 - hour + 4) * 60 - min
        } else if session_end_hour > hour || (session_end_hour == hour && session_end_min > min) {
            (session_end_hour - hour) * 60 + session_end_min - min
        } else {
            0
        };
        format!("next session in {}h {}m", mins_left / 60, mins_left % 60)
    };

    // Fetch tradable tokens for current session (skip API call if not needed)
    let (tradable_count, tradable_list) = if !closed && (show_tradable || !json) {
        match api::fetch_session_limits().await {
            Ok(limits) => {
                let tradable: Vec<String> = limits.iter()
                    .filter(|l| l.is_tradable(session))
                    .map(|l| l.symbol.clone())
                    .collect();
                let count = tradable.len();
                if show_tradable {
                    (Some(count), Some(tradable))
                } else {
                    (Some(count), None)
                }
            }
            Err(_) => (None, None),
        }
    } else if !closed {
        // JSON without --tradable: still show count, skip list
        match api::fetch_session_limits().await {
            Ok(limits) => {
                let count = limits.iter()
                    .filter(|l| l.is_tradable(session))
                    .count();
                (Some(count), None)
            }
            Err(_) => (None, None),
        }
    } else {
        (None, None)
    };

    let status = if closed { "closed" } else { "open" };

    if json {
        return json_out(&HoursJson {
            status,
            session: session.label(),
            session_hours: session.hours(),
            now: time_str,
            countdown,
            tradable_count,
            tradable: tradable_list,
        });
    }

    let label = if closed { "CLOSED" } else { "OPEN" };
    println!("{} — {}", label, session.label());
    println!("  Now:         {}", time_str);
    println!("  Session:     {} ({})", session.label(), session.hours());
    println!();
    println!("  Sessions:");
    println!("    Pre-Market:     4:00 AM – 9:29 AM ET");
    println!("    Regular:        9:30 AM – 3:59 PM ET");
    println!("    Post-Market:    4:00 PM – 7:59 PM ET");
    println!("    Overnight:      8:00 PM – 3:59 AM ET");
    println!("    Closed:         Fri 8 PM – Sun 8 PM ET");
    println!();
    let cd = countdown.trim_start_matches("opens in ").trim_start_matches("next session in ");
    if closed {
        println!("  Opens in: {}", cd);
    } else {
        println!("  Next session in: {}", cd);
        if let Some(count) = tradable_count {
            println!("  Tradable now: {} tokens", count);
        }
    }
    Ok(())
}

pub async fn portfolio(wallet: Option<&str>, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let tokens = token_list::get_token_list();
    let pubkey = match wallet {
        Some(w) => {
            solana::validate_address(w)?;
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
    let mint = entry.solana_address
        .ok_or_else(|| eyre::eyre!("No Solana address for {}", entry.symbol))?;
    Ok((entry.symbol.to_string(), mint.to_string()))
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

/// Maximum allowed slippage before blocking the trade.
const MAX_SLIPPAGE_PCT: f64 = 10.0;

/// Get slippage: prefer Jupiter's `price_impact`, fall back to USD value diff.
fn calc_slippage(order: &jupiter::OrderResponse) -> Option<f64> {
    // Jupiter Ultra API returns price_impact directly (negative = loss).
    if let Some(pi) = order.price_impact {
        return Some(pi);
    }
    // Fallback: compute from USD values
    match (order.in_usd_value, order.out_usd_value) {
        (Some(usd_in), Some(usd_out)) if usd_in > 0.0 => {
            Some((usd_out - usd_in) / usd_in * 100.0)
        }
        _ => None,
    }
}

/// Check slippage and warn/block. Returns slippage for JSON output.
fn check_slippage(order: &jupiter::OrderResponse, json: bool) -> Result<Option<f64>> {
    let slip = calc_slippage(order);
    if let Some(s) = slip {
        if s < -MAX_SLIPPAGE_PCT {
            return Err(eyre::eyre!(
                "Slippage too high ({s:.2}%). Max allowed: -{MAX_SLIPPAGE_PCT:.0}%. \
                 Try a smaller amount or wait for better liquidity."
            ));
        }
        if s < -1.0 && !json {
            eprintln!("Warning: slippage {s:.2}%");
        }
    }
    Ok(slip)
}

/// Minimum SOL needed for transaction fees (~0.01 SOL covers tx + ATA creation).
const MIN_SOL_FOR_GAS: f64 = 0.01;
/// Minimum buy/sell amount in USDC. Jupiter rejects orders below ~$1.
const MIN_USDC_AMOUNT: f64 = 1.0;
/// Minimum sell value in USD. Jupiter market makers reject tiny orders.
const MIN_SELL_VALUE_USD: f64 = 1.0;
/// USDC amount to swap for SOL when gas is low (~$3 → ~0.02 SOL).
const TOPUP_USDC: f64 = 3.0;

/// Resolve percentage or "all" amounts into absolute numbers.
///  - "100" → 100.0 (passthrough)
///  - "50%" → 50% of balance
///  - "all" → 100% of balance
///
/// `balance_fn` is called lazily only when needed.
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

fn check_trading_hours() -> Result<()> {
    let session = api::current_session();
    if session == api::Session::Closed {
        use chrono_tz::US::Eastern;
        let now = chrono::Utc::now().with_timezone(&Eastern);
        return Err(eyre::eyre!(
            "Ondo GM market is closed right now.\n  \
             Trading resumes: Sunday 8:00 PM ET (Overnight session)\n  \
             Current time:    {} ET\n  \
             Run `rwa gm hours` for session details",
            now.format("%A %I:%M %p")
        ));
    }
    Ok(())
}

async fn preflight_buy(pubkey: &str, usdc_amount: f64, w: &wallet::Wallet, json: bool, rpc_url: Option<&str>) -> Result<()> {
    check_trading_hours()?;

    if usdc_amount < MIN_USDC_AMOUNT {
        return Err(eyre::eyre!("Minimum buy amount is {MIN_USDC_AMOUNT} USDC"));
    }

    let sol = solana::get_sol_balance(pubkey, rpc_url).await?;
    let usdc = solana::get_usdc_balance(pubkey, rpc_url).await?;

    if sol < MIN_SOL_FOR_GAS && usdc > usdc_amount + TOPUP_USDC {
        topup_sol(w, pubkey, json, rpc_url).await?;
        let usdc = usdc - TOPUP_USDC;
        if usdc < usdc_amount {
            return Err(eyre::eyre!(
                "Insufficient USDC: {usdc:.2} USDC (need {usdc_amount:.2})\n  \
                 Fund wallet: {pubkey}"
            ));
        }
        return Ok(());
    }

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

async fn preflight_sell(pubkey: &str, w: &wallet::Wallet, json: bool, rpc_url: Option<&str>) -> Result<()> {
    check_trading_hours()?;

    let sol = solana::get_sol_balance(pubkey, rpc_url).await?;
    if sol < MIN_SOL_FOR_GAS {
        let usdc = solana::get_usdc_balance(pubkey, rpc_url).await?;
        if usdc >= TOPUP_USDC {
            topup_sol(w, pubkey, json, rpc_url).await?;
            return Ok(());
        }
        return Err(eyre::eyre!(
            "Insufficient SOL for gas: {sol:.6} SOL (need ≥{MIN_SOL_FOR_GAS})\n  \
             Fund wallet: {pubkey}"
        ));
    }
    Ok(())
}

/// Auto-swap USDC → SOL for transaction fees.
async fn topup_sol(w: &wallet::Wallet, pubkey: &str, json: bool, rpc_url: Option<&str>) -> Result<()> {
    if !json {
        eprintln!("SOL too low for gas — swapping ${TOPUP_USDC:.0} USDC → SOL ...");
    }
    let raw_usdc = jupiter::usdc_to_raw(&format!("{TOPUP_USDC:.2}"))?;
    let order = jupiter::get_order(jupiter::USDC_MINT, jupiter::SOL_MINT, &raw_usdc, pubkey).await?;
    execute_with_retry(w, &order, json, jupiter::USDC_MINT, jupiter::SOL_MINT, &raw_usdc, pubkey).await?;
    // Wait for the SOL to arrive
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let new_sol = solana::get_sol_balance(pubkey, rpc_url).await?;
    if !json {
        eprintln!("SOL topped up: {new_sol:.6} SOL");
    }
    Ok(())
}

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

fn confirm(msg: &str) -> bool {
    print!("{msg} [y/N] ");
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Execute a swap with automatic retries (max 2) for transient Jupiter errors.
/// For expired/rejected quotes (-2003, -2004, -2005), fetches a fresh order before retrying.
const MAX_SWAP_RETRIES: u32 = 2;

async fn execute_with_retry(
    w: &wallet::Wallet,
    order: &jupiter::OrderResponse,
    json: bool,
    input_mint: &str,
    output_mint: &str,
    raw_amount: &str,
    taker: &str,
) -> Result<jupiter::ExecuteResponse> {
    let mut current_order_owned: Option<jupiter::OrderResponse> = None;
    let mut last_err = None;

    for attempt in 0..=MAX_SWAP_RETRIES {
        let ord = current_order_owned.as_ref().unwrap_or(order);
        match jupiter::execute_order(w, ord).await {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                let msg = e.to_string();
                let needs_new_order = msg.contains("code -2003")
                    || msg.contains("code -2004")
                    || msg.contains("code -2005");
                let retry_same = msg.contains("code -1000")
                    || msg.contains("code -2000")
                    || msg.contains("code -1)");

                if (!needs_new_order && !retry_same) || attempt == MAX_SWAP_RETRIES {
                    return Err(e);
                }
                if !json {
                    eprintln!("Transient error (attempt {}/{}), retrying in 3s...", attempt + 1, MAX_SWAP_RETRIES);
                }
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                if needs_new_order {
                    current_order_owned = Some(
                        jupiter::get_order(input_mint, output_mint, raw_amount, taker).await?
                    );
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| eyre::eyre!("Swap failed after retries")))
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

    // For "all" and "%", use the raw on-chain amount to avoid float precision loss
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
            let raw: u128 = bal.raw_amount.parse().unwrap_or(0);
            let pct_raw = (raw as f64 * pct / 100.0) as u128;
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

#[derive(Serialize)]
struct CloseAllResultJson {
    status: &'static str,
    sold: Vec<CloseItemJson>,
    failed: Vec<CloseFailJson>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    skipped: Vec<CloseSkipJson>,
    total_usdc: String,
}

#[derive(Serialize)]
struct CloseSkipJson {
    token: String,
    estimated_usd: f64,
    reason: &'static str,
}

#[derive(Serialize)]
struct CloseItemJson {
    token: String,
    amount: String,
    usdc: String,
    tx: String,
}

#[derive(Serialize)]
struct CloseFailJson {
    token: String,
    error: String,
}

pub async fn close_all(amount: Option<&str>, yes: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    check_trading_hours()?;

    // Parse optional percentage (e.g. "10%", "50%"). None = sell 100%.
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

    // Get balances and prices concurrently
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

    // Ensure SOL for gas
    preflight_sell(&taker, &w, json, rpc_url).await?;

    let mut sold = Vec::new();
    let mut failed = Vec::new();
    let mut skipped = Vec::new();
    let mut total_usdc: f64 = 0.0;

    for (i, tb) in balances.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }

        // Compute raw amount to sell
        let sell_raw = if sell_pct < 100.0 {
            let raw: u128 = tb.raw_amount.parse().unwrap_or(0);
            let partial = (raw as f64 * sell_pct / 100.0) as u128;
            if partial == 0 {
                continue; // skip dust
            }
            partial.to_string()
        } else {
            tb.raw_amount.clone()
        };

        // Skip positions below minimum sell value — Jupiter rejects tiny orders.
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

        match sell_one_position(&w, &tb.symbol, &tb.mint, &sell_raw, &taker, json).await {
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

/// Sell a single position (helper for close_all). Returns (usdc_received, tx_url).
async fn sell_one_position(
    w: &wallet::Wallet,
    _symbol: &str,
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

fn clean_name(name: &str) -> String {
    name.replace(" (Ondo Tokenized)", "")
}

fn token_type_from_name(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if n.contains("etf") || n.contains(" fund") || n.contains(" trust")
        || n.contains(" index") || n.contains(" shares")
    {
        "etf"
    } else {
        "stock"
    }
}

async fn list(json: bool, search: Option<&str>) -> Result<()> {
    let tokens = token_list::get_token_list();
    let assets = api::fetch_assets().await.unwrap_or_default();

    // Fetch session limits to determine tradability
    let session = api::current_session();
    let tradable_set: std::collections::HashSet<String> = api::fetch_session_limits().await
        .unwrap_or_default()
        .iter()
        .filter(|l| l.is_tradable(session))
        .map(|l| l.symbol.to_uppercase())
        .collect();

    // Build symbol → OndoAsset lookup
    let asset_map: std::collections::HashMap<String, &api::OndoAsset> = assets.iter()
        .map(|a| (a.symbol.to_uppercase(), a))
        .collect();

    let filtered: Vec<_> = match search {
        Some(q) => {
            let q = q.to_lowercase();
            tokens.iter().filter(|t| {
                let sym_match = t.symbol.to_lowercase().contains(&q);
                let name_match = asset_map.get(&t.symbol.to_uppercase())
                    .map(|a| a.asset_name.to_lowercase().contains(&q))
                    .unwrap_or(false);
                let sector_match = asset_map.get(&t.symbol.to_uppercase())
                    .and_then(|a| a.sector())
                    .map(|s| s.to_lowercase().contains(&q))
                    .unwrap_or(false);
                sym_match || name_match || sector_match
            }).collect()
        }
        None => tokens.iter().collect(),
    };

    if json {
        let items: Vec<_> = filtered.iter().map(|t| {
            let asset = asset_map.get(&t.symbol.to_uppercase());
            let name = asset.map(|a| clean_name(&a.asset_name)).unwrap_or_default();
            let kind = asset.and_then(|a| a.instrument_type())
                .unwrap_or_else(|| token_type_from_name(&name))
                .to_lowercase();
            let sector = asset.and_then(|a| a.sector()).map(String::from);
            let tradable = tradable_set.contains(&t.symbol.to_uppercase());
            ListItemJson {
                symbol: t.symbol.to_string(),
                name,
                kind,
                sector,
                tradable,
            }
        }).collect();
        return json_out(&items);
    }

    println!("{} GM tokens{}\n", filtered.len(),
        search.map(|s| format!(" matching '{}'", s)).unwrap_or_default());
    for t in &filtered {
        let asset = asset_map.get(&t.symbol.to_uppercase());
        let name = asset.map(|a| clean_name(&a.asset_name)).unwrap_or_default();
        let sector = asset.and_then(|a| a.sector()).unwrap_or("");
        let mark = if tradable_set.contains(&t.symbol.to_uppercase()) { "✓" } else { "✗" };
        if sector.is_empty() {
            println!("  {} {:<12} {}", mark, t.symbol, name);
        } else {
            println!("  {} {:<12} {:<30} {}", mark, t.symbol, name, sector);
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

    solana::validate_address(to)?;
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

    let sol_bal = solana::get_sol_balance(&pubkey, rpc_url).await?;
    let is_all = amount == "all" || amount == "100%";

    let sol_f = if is_all {
        sol_bal
    } else if let Some(pct_str) = amount.strip_suffix('%') {
        let pct: f64 = pct_str.parse().map_err(|_| eyre::eyre!("Invalid percentage: {amount}"))?;
        if !(0.0..=100.0).contains(&pct) {
            return Err(eyre::eyre!("Percentage must be 0–100, got {pct}"));
        }
        sol_bal * pct / 100.0
    } else {
        amount.parse::<f64>().map_err(|_| eyre::eyre!("Invalid amount: {amount}"))?
    };

    if sol_f < MIN_SOL_FOR_GAS {
        return Err(eyre::eyre!("Amount too small — need to keep ≥{MIN_SOL_FOR_GAS} SOL for gas"));
    }

    // Keep enough for gas if sending "all"
    let send_amount = if is_all {
        if sol_bal <= MIN_SOL_FOR_GAS {
            return Err(eyre::eyre!("Balance too low — need ≥{MIN_SOL_FOR_GAS} SOL for gas"));
        }
        sol_bal - MIN_SOL_FOR_GAS
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

    let raw_str = jupiter::usdc_to_raw(&format!("{usdc_f:.2}"))?;
    let raw: u64 = raw_str.parse().map_err(|_| eyre::eyre!("Invalid USDC amount"))?;

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
    let tokens = token_list::get_token_list();
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

    // GM tokens use 9 decimals (Token-2022) — use string math to avoid float precision loss
    let token_str = format!("{:.9}", token_f);
    let raw_str = jupiter::token_to_raw(&token_str, jupiter::GM_SOL_DECIMALS)?;
    let raw: u64 = raw_str.parse().map_err(|_| eyre::eyre!("Invalid token amount"))?;

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
