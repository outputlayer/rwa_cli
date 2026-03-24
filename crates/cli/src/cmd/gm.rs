use alloy_primitives::{Address, U256};
use chrono::Datelike;
use clap::Subcommand;
use eyre::Result;
use rwa_core::chain::Chain;
use rwa_ondo::{api, gm, jupiter, oracle, provider, solana, token_list, wallet};
use std::io::{self, Write};

fn parse_chain(s: &str) -> Result<Chain> {
    match s.to_lowercase().as_str() {
        "bsc" | "bnb" => Ok(Chain::BnbMainnet),
        "eth" | "ethereum" => Ok(Chain::EthereumMainnet),
        "sol" | "solana" => Ok(Chain::SolanaMainnet),
        _ => Err(eyre::eyre!("Unknown chain: {s}. Use: bsc, eth, or solana")),
    }
}

#[derive(Subcommand, Debug)]
pub enum GmAction {
    /// List all available GM tokens
    List,

    /// <SYMBOL|all>         Token price
    Price {
        /// Token symbol (e.g. TSLA, TSLAon) — or "all" for all tokens
        symbol: String,
    },

    /// <SYMBOL>             Detailed token info (price, 24h, sValue, holders, 52w)
    Info {
        /// Token symbol (e.g. TSLA, TSLAon)
        symbol: String,
    },

    /// Show top gainers and losers (24h)
    Top {
        /// Number of tokens to show per side (default: 10)
        #[arg(short, long, default_value = "10")]
        n: usize,
    },

    /// <QUERY>              Search tokens by name or symbol
    Search {
        /// Search query (e.g. "Tesla", "tech", "APL")
        query: String,
    },

    /// Show trading hours status and countdown
    Hours,

    /// <WALLET>             Portfolio with USD values and 24h P&L
    Portfolio {
        /// Wallet address (0x... for EVM, base58 for Solana)
        wallet: String,

        /// Chain: solana (default), bsc, or eth
        #[arg(short, long, default_value = "solana")]
        chain: String,
    },

    /// <SYMBOL> --amount <USDC>  Buy GM token with USDC via Jupiter (Solana)
    ///
    /// Liquidity: Ondo JIT mint/redeem, 24/5 (Sun 8pm — Fri 8pm ET)
    Buy {
        /// Token symbol (e.g. TSLA, TSLAon)
        symbol: String,

        /// USDC amount, percentage of balance, or "all" (e.g. 100, 50%, all)
        #[arg(short, long)]
        amount: String,

        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },

    /// <SYMBOL> --amount <N>     Sell GM token for USDC via Jupiter (Solana)
    ///
    /// Liquidity: Ondo JIT mint/redeem, 24/5 (Sun 8pm — Fri 8pm ET)
    Sell {
        /// Token symbol (e.g. TSLA, TSLAon)
        symbol: String,

        /// Token amount, percentage of holdings, or "all" (e.g. 5, 50%, all)
        #[arg(short, long)]
        amount: String,

        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },

    /// <SYMBOL> --amount <USDC>  Get swap quote without executing (Solana)
    Quote {
        /// Token symbol (e.g. TSLA, TSLAon)
        symbol: String,

        /// Amount, percentage, or "all" (e.g. 100, 50%, all)
        #[arg(short, long)]
        amount: String,

        /// Quote selling token for USDC instead of buying
        #[arg(long)]
        sell: bool,
    },

    /// Execute multiple trades sequentially
    ///
    /// Each order: "buy SYMBOL USDC_AMOUNT" or "sell SYMBOL TOKEN_AMOUNT"
    ///
    /// Example: rwa gm batch "buy TSLAon 100" "buy AAPLon 200" "sell NVDAon 5"
    ///
    /// Liquidity: Ondo JIT mint/redeem, 24/5 (Sun 8pm — Fri 8pm ET)
    Batch {
        /// Trade orders: "buy SYMBOL AMOUNT" or "sell SYMBOL AMOUNT"
        #[arg(required = true, num_args = 1..)]
        orders: Vec<String>,

        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },
}

pub async fn execute(action: GmAction, rpc_url_override: Option<&str>) -> Result<()> {
    // These commands only need the Ondo API — skip token list fetch
    match &action {
        GmAction::Price { ref symbol } => return price(symbol).await,
        GmAction::Top { n } => return top(*n).await,
        GmAction::Hours => return hours(),
        _ => {}
    }

    let tokens = token_list::get_token_list().await;

    match action {
        GmAction::List => list_tokens(&tokens),
        GmAction::Price { .. } | GmAction::Top { .. } | GmAction::Hours => unreachable!(),
        GmAction::Search { query } => search(&query, &tokens).await,
        GmAction::Info { symbol } => {
            let rpc = rpc_url_override.unwrap_or(Chain::BnbMainnet.default_rpc_url());
            info(rpc, &symbol, &tokens).await
        }
        GmAction::Portfolio { wallet, chain } => {
            let chain = parse_chain(&chain)?;
            if chain == Chain::SolanaMainnet {
                portfolio_solana(&wallet, &tokens).await
            } else {
                let rpc = rpc_url_override.unwrap_or(chain.default_rpc_url());
                portfolio_evm(rpc, &wallet, &tokens, chain).await
            }
        }
        GmAction::Buy { symbol, amount, yes } => buy(&symbol, &amount, yes, &tokens).await,
        GmAction::Sell { symbol, amount, yes } => sell(&symbol, &amount, yes, &tokens).await,
        GmAction::Quote { symbol, amount, sell } => quote(&symbol, &amount, sell, &tokens).await,
        GmAction::Batch { orders, yes } => batch(&orders, yes, &tokens).await,
    }
}

fn list_tokens(tokens: &[token_list::GmTokenEntry]) -> Result<()> {
    println!("{:<12} {}", "SYMBOL", "NAME");
    println!("{}", "-".repeat(60));
    for t in tokens {
        let name = t.name.trim_end_matches(" (Ondo Tokenized)");
        println!("{:<12} {}", t.symbol, name);
    }
    println!("\nTotal: {} tokens", tokens.len());
    Ok(())
}

async fn price(symbol: &str) -> Result<()> {
    let assets = api::fetch_assets().await?;

    if symbol.eq_ignore_ascii_case("all") {
        println!("{:<12} {:>12} {:>9}", "TOKEN", "PRICE", "24h %");
        println!("{}", "-".repeat(36));
        for asset in &assets {
            if let Some(pm) = &asset.primary_market {
                let price = api::parse_price(&pm.price);
                let pct = pm.price_change_pct_24h.as_deref().map(api::parse_price).unwrap_or(0.0);
                println!("{:<12} {:>11.2} {:>+8.2}%", asset.symbol, price, pct);
            }
        }
        println!("\nTotal: {} tokens", assets.len());
    } else {
        let asset = api::find_asset(symbol, &assets)
            .ok_or_else(|| eyre::eyre!("Token {} not found", symbol))?;
        let pm = asset.primary_market.as_ref()
            .ok_or_else(|| eyre::eyre!("No market data for {}", asset.symbol))?;

        let price = api::parse_price(&pm.price);
        let pct = pm.price_change_pct_24h.as_deref().map(api::parse_price).unwrap_or(0.0);
        println!("{}: ${:.2} ({:+.2}%)", asset.symbol, price, pct);
    }
    Ok(())
}

async fn top(n: usize) -> Result<()> {
    let assets = api::fetch_assets().await?;

    let mut priced: Vec<(&str, f64, f64)> = assets.iter()
        .filter_map(|a| {
            let pm = a.primary_market.as_ref()?;
            let price = api::parse_price(&pm.price);
            let pct = pm.price_change_pct_24h.as_deref().map(api::parse_price).unwrap_or(0.0);
            Some((a.symbol.as_str(), price, pct))
        })
        .collect();

    // Gainers (highest % first)
    priced.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    println!("🟢 Top {} Gainers (24h)", n);
    println!("{:<12} {:>12} {:>9}", "TOKEN", "PRICE", "24h %");
    println!("{}", "-".repeat(36));
    for (sym, price, pct) in priced.iter().take(n) {
        println!("{:<12} {:>11.2} {:>+8.2}%", sym, price, pct);
    }

    // Losers (lowest % first)
    println!("\n🔴 Top {} Losers (24h)", n);
    println!("{:<12} {:>12} {:>9}", "TOKEN", "PRICE", "24h %");
    println!("{}", "-".repeat(36));
    for (sym, price, pct) in priced.iter().rev().take(n) {
        println!("{:<12} {:>11.2} {:>+8.2}%", sym, price, pct);
    }

    Ok(())
}

async fn search(query: &str, tokens: &[token_list::GmTokenEntry]) -> Result<()> {
    let q = query.to_lowercase();
    let results: Vec<&token_list::GmTokenEntry> = tokens.iter()
        .filter(|t| {
            t.symbol.to_lowercase().contains(&q)
                || t.name.to_lowercase().contains(&q)
        })
        .collect();

    if results.is_empty() {
        println!("No tokens matching \"{}\"", query);
        return Ok(());
    }

    println!("{:<12} {}", "SYMBOL", "NAME");
    println!("{}", "-".repeat(60));
    for t in &results {
        let name = t.name.trim_end_matches(" (Ondo Tokenized)");
        println!("{:<12} {}", t.symbol, name);
    }
    println!("\n{} result(s)", results.len());
    Ok(())
}

fn hours() -> Result<()> {
    use chrono::Timelike;
    use chrono_tz::US::Eastern;

    let now = chrono::Utc::now().with_timezone(&Eastern);
    let wd = now.weekday();
    let hour = now.hour();
    let min = now.minute();

    let closed = matches!(wd, chrono::Weekday::Sat)
        || (wd == chrono::Weekday::Sun && hour < 20)
        || (wd == chrono::Weekday::Fri && hour >= 20);

    let time_str = now.format("%A %I:%M %p ET").to_string();

    if closed {
        println!("🔴 CLOSED");
        println!("  Now:     {}", time_str);
        println!("  Hours:   Sunday 8:00 PM — Friday 8:00 PM ET");

        // Calculate time until open (Sunday 8 PM ET)
        let days_until_sun = match wd {
            chrono::Weekday::Fri => 2, // Fri 8pm+ → Sun
            chrono::Weekday::Sat => 1, // Sat → Sun
            chrono::Weekday::Sun => 0, // Sun <8pm → same day
            _ => 0,
        };
        let mins_today_left = if wd == chrono::Weekday::Sun {
            (20 - hour) * 60 - min
        } else {
            // For Fri/Sat, calculate to end of day + remaining days + 20h on Sunday
            let to_midnight = (24 - hour) * 60 - min;
            let full_days = if days_until_sun > 1 { days_until_sun - 1 } else { 0 };
            to_midnight + full_days * 24 * 60 + 20 * 60
        };
        let h = mins_today_left / 60;
        let m = mins_today_left % 60;
        println!("  Opens in: {}h {}m", h, m);
    } else {
        println!("🟢 OPEN");
        println!("  Now:     {}", time_str);
        println!("  Hours:   Sunday 8:00 PM — Friday 8:00 PM ET");

        // Calculate time until close (Friday 8 PM ET)
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
        let h = mins_left / 60;
        let m = mins_left % 60;
        println!("  Closes in: {}h {}m", h, m);
    }
    Ok(())
}

async fn info(rpc_url: &str, symbol: &str, tokens: &[token_list::GmTokenEntry]) -> Result<()> {
    let entry = gm::resolve_token(symbol, tokens)?;
    let address = entry.bsc_address
        .ok_or_else(|| eyre::eyre!("No BSC address for {}", entry.symbol))?;

    // Fetch on-chain data and Ondo API in parallel
    let (provider_res, assets_res) = tokio::join!(
        provider::create_provider(rpc_url),
        api::fetch_assets()
    );
    let provider = provider_res?;
    let assets = assets_res?;

    let (token_info, oracle_data) = tokio::join!(
        gm::get_token_info(&provider, address),
        oracle::get_oracle_data(&provider, address)
    );
    let token_info = token_info?;
    let oracle_data = oracle_data?;

    let asset = api::find_asset(&entry.symbol, &assets);

    println!("{} ({})", token_info.symbol, token_info.name);
    println!("{}", "─".repeat(50));

    // Price and 24h from Ondo API
    if let Some(pm) = asset.and_then(|a| a.primary_market.as_ref()) {
        let price = api::parse_price(&pm.price);
        let chg = pm.price_change_24h.as_deref().map(api::parse_price).unwrap_or(0.0);
        let pct = pm.price_change_pct_24h.as_deref().map(api::parse_price).unwrap_or(0.0);
        println!("  Price:         ${:.2}", price);
        println!("  24h Change:    {:+.2} ({:+.2}%)", chg, pct);
        if let Some(sv) = &pm.shares_multiplier {
            println!("  Shares/Token:  {}", sv);
        }
        if let Some(holders) = pm.total_holders {
            println!("  Holders:       {}", holders);
        }
    }

    // On-chain oracle
    println!("  sValue:        {:.6}", oracle_data.value);
    println!("  Decimals:      {}", token_info.decimals);

    // Addresses
    println!("  BSC:           {}", address);
    if let Some(eth) = entry.eth_address {
        println!("  ETH:           {}", eth);
    }
    if let Some(sol) = &entry.solana_address {
        println!("  Solana:        {}", sol);
    }

    // Underlying market
    if let Some(um) = asset.and_then(|a| a.underlying_market.as_ref()) {
        println!("\n  ─── {} ───", um.name);
        if let (Some(hi), Some(lo)) = (&um.price_high_52w, &um.price_low_52w) {
            println!("  52w Range:     ${} — ${}", lo, hi);
        }
        if let Some(vol) = &um.volume {
            println!("  Volume:        {}", format_compact_usd(vol));
        }
        if let Some(cap) = &um.market_cap {
            println!("  Market Cap:    {}", format_compact_usd(cap));
        }
    }

    println!("\n  Liquidity:     Ondo JIT mint/redeem, 24/5 (Sun 8pm — Fri 8pm ET)");
    println!("  Explorer:      {}", rwa_core::chain::Chain::BnbMainnet.explorer_url(address));

    Ok(())
}

async fn portfolio_evm(rpc_url: &str, wallet: &str, tokens: &[token_list::GmTokenEntry], chain: Chain) -> Result<()> {
    let wallet_addr: Address = wallet.parse()?;
    let provider = provider::create_provider(rpc_url).await?;

    // Fetch balances and prices in parallel
    let (balances, assets) = tokio::join!(
        gm::get_all_balances(&provider, wallet_addr, tokens, chain),
        api::fetch_assets()
    );
    let balances = balances?;
    let assets = assets?;

    if balances.is_empty() {
        println!("No GM token balances found on {chain} for {wallet}");
        return Ok(());
    }

    println!("{chain} Portfolio for {wallet}\n");
    println!(
        "{:<12} {:>14} {:>12} {:>16} {:>9}",
        "TOKEN", "BALANCE", "PRICE", "VALUE (USD)", "24h %"
    );
    println!("{}", "─".repeat(67));

    let mut total_value = 0.0;
    let mut total_prev_value = 0.0;

    for tb in &balances {
        let bal_f64 = u256_to_f64(tb.balance, 18);
        let asset = api::find_asset(&tb.token.symbol, &assets);

        let (price, pct_24h) = match asset.and_then(|a| a.primary_market.as_ref()) {
            Some(pm) => {
                let p = api::parse_price(&pm.price);
                let pct = pm.price_change_pct_24h.as_deref().map(api::parse_price).unwrap_or(0.0);
                (p, pct)
            }
            None => (0.0, 0.0),
        };

        let value = bal_f64 * price;
        let prev_value = if pct_24h.abs() > f64::EPSILON {
            value / (1.0 + pct_24h / 100.0)
        } else {
            value
        };

        total_value += value;
        total_prev_value += prev_value;

        println!(
            "{:<12} {:>14.4} {:>11.2} {:>15.2} {:>+8.2}%",
            tb.token.symbol, bal_f64, price, value, pct_24h
        );
    }

    println!("{}", "─".repeat(67));

    let total_change = total_value - total_prev_value;
    let total_pct = if total_prev_value.abs() > f64::EPSILON {
        (total_change / total_prev_value) * 100.0
    } else {
        0.0
    };

    println!(
        "{:<12} {:>14} {:>12} {:>15.2}",
        "TOTAL", "", "", total_value
    );
    println!(
        "\n24h Change: {:+.2} ({:+.2}%)",
        total_change, total_pct
    );

    Ok(())
}

async fn portfolio_solana(wallet: &str, tokens: &[token_list::GmTokenEntry]) -> Result<()> {
    let (balances, assets, sol_bal, usdc_bal) = tokio::join!(
        solana::get_all_balances(wallet, tokens, None),
        api::fetch_assets(),
        solana::get_sol_balance(wallet, None),
        solana::get_usdc_balance(wallet, None)
    );
    let balances = balances?;
    let assets = assets?;
    let sol_bal = sol_bal.unwrap_or(0.0);
    let usdc_bal = usdc_bal.unwrap_or(0.0);

    println!("Solana Portfolio for {wallet}\n");
    println!("  SOL:   {:.6}", sol_bal);
    println!("  USDC:  {:.2}", usdc_bal);

    if balances.is_empty() {
        println!("\nNo GM token positions.");
        return Ok(());
    }

    println!();
    println!(
        "{:<12} {:>14} {:>12} {:>16} {:>9}",
        "TOKEN", "BALANCE", "PRICE", "VALUE (USD)", "24h %"
    );
    println!("{}", "─".repeat(67));

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

        println!(
            "{:<12} {:>14.4} {:>11.2} {:>15.2} {:>+8.2}%",
            tb.symbol, tb.balance, price, value, pct_24h
        );
    }

    println!("{}", "─".repeat(67));

    let total_change = total_value - total_prev_value;
    let total_pct = if total_prev_value.abs() > f64::EPSILON {
        (total_change / total_prev_value) * 100.0
    } else {
        0.0
    };

    println!(
        "{:<12} {:>14} {:>12} {:>15.2}",
        "TOTAL", "", "", total_value
    );
    println!(
        "\n24h Change: {:+.2} ({:+.2}%)",
        total_change, total_pct
    );

    Ok(())
}

fn format_compact_usd(v: &str) -> String {
    let n: f64 = v.parse().unwrap_or(0.0);
    if n >= 1e12 {
        format!("${:.2}T", n / 1e12)
    } else if n >= 1e9 {
        format!("${:.2}B", n / 1e9)
    } else if n >= 1e6 {
        format!("${:.2}M", n / 1e6)
    } else if n >= 1e3 {
        format!("${:.1}K", n / 1e3)
    } else {
        format!("${v}")
    }
}

/// Format a U256 with 18 decimals into a human-readable string.
fn format_u256_decimals(value: U256, decimals: u8) -> String {
    let s = value.to_string();
    let d = decimals as usize;
    if s.len() <= d {
        let zeros = d - s.len();
        format!("0.{}{}", "0".repeat(zeros), s.trim_end_matches('0'))
    } else {
        let (integer, frac) = s.split_at(s.len() - d);
        let frac = frac.trim_end_matches('0');
        if frac.is_empty() {
            integer.to_string()
        } else {
            format!("{integer}.{frac}")
        }
    }
}

/// Convert U256 with `decimals` to f64.
fn u256_to_f64(value: U256, decimals: u8) -> f64 {
    let s = format_u256_decimals(value, decimals);
    s.parse::<f64>().unwrap_or(0.0)
}

// ── Jupiter swap commands ──────────────────────────────────

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
///  - "all" → 100% of balance
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

/// Check if Ondo GM trading is currently open.
/// Trading window: Sunday 8 PM ET → Friday 8 PM ET (24/5).
fn check_trading_hours() -> Result<()> {
    use chrono::Timelike;
    use chrono_tz::US::Eastern;

    let now = chrono::Utc::now().with_timezone(&Eastern);
    let wd = now.weekday();
    let hour = now.hour();

    let closed = matches!(wd, chrono::Weekday::Sat)
        || (wd == chrono::Weekday::Sun && hour < 20)
        || (wd == chrono::Weekday::Fri && hour >= 20);

    if closed {
        return Err(eyre::eyre!(
            "Ondo GM market is closed right now.\n  \
             Trading hours: Sunday 8 PM — Friday 8 PM ET (24/5)\n  \
             Current time:  {} ET",
            now.format("%A %I:%M %p")
        ));
    }
    Ok(())
}

async fn preflight_buy(pubkey: &str, usdc_amount: f64) -> Result<()> {
    check_trading_hours()?;

    if usdc_amount < MIN_USDC_AMOUNT {
        return Err(eyre::eyre!("Minimum buy amount is {MIN_USDC_AMOUNT} USDC"));
    }

    let (sol, usdc) = tokio::join!(
        solana::get_sol_balance(pubkey, None),
        solana::get_usdc_balance(pubkey, None)
    );
    let sol = sol?;
    let usdc = usdc?;

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

async fn preflight_sell(pubkey: &str) -> Result<()> {
    check_trading_hours()?;

    let sol = solana::get_sol_balance(pubkey, None).await?;
    if sol < MIN_SOL_FOR_GAS {
        return Err(eyre::eyre!(
            "Insufficient SOL for gas: {sol:.6} SOL (need ≥{MIN_SOL_FOR_GAS})\n  \
             Fund wallet: {pubkey}"
        ));
    }
    Ok(())
}

async fn quote(symbol: &str, amount: &str, is_sell: bool, tokens: &[token_list::GmTokenEntry]) -> Result<()> {
    check_trading_hours()?;
    let (sym, gm_mint) = resolve_gm_mint(symbol, tokens)?;
    let w = load_wallet()?;
    let taker = w.pubkey();

    let gm_dec = jupiter::GM_SOL_DECIMALS;
    let usdc_dec = jupiter::USDC_DECIMALS;

    let (input_mint, output_mint, raw_amount, direction) = if is_sell {
        let sell_f = resolve_percent_amount(amount, || {
            let t = taker.clone();
            let m = gm_mint.clone();
            async move {
                let bal = solana::get_balance(&t, &m, None).await?;
                Ok(bal.balance)
            }
        }).await?;
        let sell_str = format!("{:.prec$}", sell_f, prec = gm_dec as usize);
        let raw = jupiter::token_to_raw(&sell_str, gm_dec)?;
        (gm_mint.as_str().to_string(), jupiter::USDC_MINT.to_string(), raw, "sell")
    } else {
        let buy_f = resolve_percent_amount(amount, || {
            let t = taker.clone();
            async move { solana::get_usdc_balance(&t, None).await }
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

    println!("Quote: {} {} → {} {}", in_fmt, in_label, out_fmt, out_label);
    if let (Some(usd_in), Some(usd_out)) = (order.in_usd_value, order.out_usd_value) {
        let slippage = if usd_in > 0.0 { (usd_out - usd_in) / usd_in * 100.0 } else { 0.0 };
        println!("  Input value:   ${:.2}", usd_in);
        println!("  Output value:  ${:.2}", usd_out);
        println!("  Slippage:      {:.2}%", slippage);
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

async fn buy(symbol: &str, amount: &str, yes: bool, tokens: &[token_list::GmTokenEntry]) -> Result<()> {
    let (sym, gm_mint) = resolve_gm_mint(symbol, tokens)?;
    let w = load_wallet()?;
    let taker = w.pubkey();

    let usdc_f = resolve_percent_amount(amount, || {
        let t = taker.clone();
        async move { solana::get_usdc_balance(&t, None).await }
    }).await?;
    let usdc_str = format!("{usdc_f:.2}");

    preflight_buy(&taker, usdc_f).await?;

    let raw_usdc = jupiter::usdc_to_raw(&usdc_str)?;
    let gm_dec = jupiter::GM_SOL_DECIMALS;

    println!("Getting quote for {} USDC → {} ...", usdc_str, sym);
    let order = jupiter::get_order(jupiter::USDC_MINT, &gm_mint, &raw_usdc, &taker).await?;

    let out_fmt = jupiter::format_amount(&order.out_amount, gm_dec);
    println!("You will receive ~{} {}", out_fmt, sym);
    if let (Some(usd_in), Some(usd_out)) = (order.in_usd_value, order.out_usd_value) {
        if usd_in > 0.0 {
            let slippage = (usd_out - usd_in) / usd_in * 100.0;
            if slippage < -1.0 {
                println!("⚠ High slippage: {:.2}%", slippage);
            }
        }
    }

    if !yes && !confirm("Proceed?") {
        println!("Cancelled.");
        return Ok(());
    }

    println!("Executing swap...");
    let result = jupiter::execute_order(&w, &order).await?;

    let final_out = result.output_amount_result.as_deref()
        .map(|r| jupiter::format_amount(r, gm_dec))
        .unwrap_or(out_fmt);

    let sig = result.signature.as_deref().unwrap_or("unknown");
    println!("\nSwap successful!");
    println!("  Bought:    {} {}", final_out, sym);
    println!("  Spent:     {} USDC", usdc_str);
    println!("  Tx:        https://solscan.io/tx/{}", sig);

    Ok(())
}

async fn sell(symbol: &str, amount: &str, yes: bool, tokens: &[token_list::GmTokenEntry]) -> Result<()> {
    let (sym, gm_mint) = resolve_gm_mint(symbol, tokens)?;
    let w = load_wallet()?;
    let taker = w.pubkey();

    preflight_sell(&taker).await?;

    let gm_dec = jupiter::GM_SOL_DECIMALS;

    let sell_f = resolve_percent_amount(amount, || {
        let t = taker.clone();
        let m = gm_mint.clone();
        async move {
            let bal = solana::get_balance(&t, &m, None).await?;
            Ok(bal.balance)
        }
    }).await?;
    let sell_str = format!("{:.prec$}", sell_f, prec = gm_dec as usize);

    let raw_gm = jupiter::token_to_raw(&sell_str, gm_dec)?;

    println!("Getting quote for {} {} → USDC ...", sell_str, sym);
    let order = jupiter::get_order(&gm_mint, jupiter::USDC_MINT, &raw_gm, &taker).await?;

    let out_fmt = jupiter::format_amount(&order.out_amount, jupiter::USDC_DECIMALS);
    println!("You will receive ~{} USDC", out_fmt);
    if let (Some(usd_in), Some(usd_out)) = (order.in_usd_value, order.out_usd_value) {
        if usd_in > 0.0 {
            let slippage = (usd_out - usd_in) / usd_in * 100.0;
            if slippage < -1.0 {
                println!("⚠ High slippage: {:.2}%", slippage);
            }
        }
    }

    if !yes && !confirm("Proceed?") {
        println!("Cancelled.");
        return Ok(());
    }

    println!("Executing swap...");
    let result = jupiter::execute_order(&w, &order).await?;

    let final_out = result.output_amount_result.as_deref()
        .map(|r| jupiter::format_amount(r, jupiter::USDC_DECIMALS))
        .unwrap_or(out_fmt);

    let sig = result.signature.as_deref().unwrap_or("unknown");
    println!("\nSwap successful!");
    println!("  Sold:      {} {}", amount, sym);
    println!("  Received:  {} USDC", final_out);
    println!("  Tx:        https://solscan.io/tx/{}", sig);

    Ok(())
}

/// Parse an order string like "buy TSLAon 100" or "sell AAPLon 5".
fn parse_order(s: &str) -> Result<(bool, String, String)> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() != 3 {
        return Err(eyre::eyre!(
            "Invalid order: \"{s}\"\n  Expected format: \"buy SYMBOL AMOUNT\" or \"sell SYMBOL AMOUNT\""
        ));
    }
    let is_buy = match parts[0].to_lowercase().as_str() {
        "buy" => true,
        "sell" => false,
        other => return Err(eyre::eyre!(
            "Invalid action \"{other}\" in order \"{s}\"\n  Use \"buy\" or \"sell\""
        )),
    };
    Ok((is_buy, parts[1].to_string(), parts[2].to_string()))
}

async fn batch(orders: &[String], yes: bool, tokens: &[token_list::GmTokenEntry]) -> Result<()> {
    // Parse all orders first to fail fast
    let parsed: Vec<(bool, String, String)> = orders
        .iter()
        .map(|o| parse_order(o))
        .collect::<Result<Vec<_>>>()?;

    check_trading_hours()?;

    let w = load_wallet()?;
    let taker = w.pubkey();

    // Calculate total USDC needed for all buys
    let total_usdc: f64 = parsed
        .iter()
        .filter(|(is_buy, _, _)| *is_buy)
        .map(|(_, _, amt)| amt.parse::<f64>().unwrap_or(0.0))
        .sum();

    if total_usdc > 0.0 {
        preflight_buy(&taker, total_usdc).await?;
    } else {
        preflight_sell(&taker).await?;
    }

    // Resolve all mints upfront
    let resolved: Vec<(bool, String, String, String)> = parsed
        .into_iter()
        .map(|(is_buy, sym, amt)| {
            let (sym, mint) = resolve_gm_mint(&sym, tokens)?;
            Ok((is_buy, sym.to_string(), mint, amt))
        })
        .collect::<Result<Vec<_>>>()?;

    println!("{} orders:", resolved.len());
    for (is_buy, sym, _, amt) in &resolved {
        let label = if *is_buy { "BUY" } else { "SELL" };
        println!("  {} {} {}", label, amt, sym);
    }
    if !yes && !confirm("Execute all?") {
        println!("Cancelled.");
        return Ok(());
    }

    println!();

    let gm_dec = jupiter::GM_SOL_DECIMALS;
    let mut succeeded = Vec::new();
    let mut failed = Vec::new();

    for (i, (is_buy, sym, gm_mint, amount)) in resolved.iter().enumerate() {
        let label = if *is_buy { "BUY" } else { "SELL" };
        println!("[{}/{}] {} {} {} ...", i + 1, resolved.len(), label, amount, sym);

        let result = if *is_buy {
            execute_single_buy(&w, &taker, sym, gm_mint, amount, gm_dec).await
        } else {
            execute_single_sell(&w, &taker, sym, gm_mint, amount, gm_dec).await
        };

        match result {
            Ok((filled, sig)) => {
                println!("  ✓ {}", filled);
                if let Some(s) = &sig {
                    println!("    https://solscan.io/tx/{s}");
                }
                succeeded.push(format!("{label} {amount} {sym}"));
            }
            Err(e) => {
                println!("  ✗ {e}");
                failed.push(format!("{label} {amount} {sym}: {e}"));
            }
        }
        println!();
    }

    // Summary
    println!("─── Batch Summary ───");
    println!("  Succeeded: {}/{}", succeeded.len(), resolved.len());
    if !failed.is_empty() {
        println!("  Failed:    {}/{}", failed.len(), resolved.len());
        for f in &failed {
            println!("    - {f}");
        }
    }

    if !failed.is_empty() {
        return Err(eyre::eyre!("{} of {} orders failed", failed.len(), resolved.len()));
    }
    Ok(())
}

/// Execute a single buy, returns (description, optional tx signature).
async fn execute_single_buy(
    w: &wallet::Wallet,
    taker: &str,
    sym: &str,
    gm_mint: &str,
    amount: &str,
    gm_dec: u8,
) -> Result<(String, Option<String>)> {
    let usdc_f = resolve_percent_amount(amount, || {
        let t = taker.to_string();
        async move { solana::get_usdc_balance(&t, None).await }
    }).await?;
    let usdc_str = format!("{usdc_f:.2}");
    let raw = jupiter::usdc_to_raw(&usdc_str)?;
    let order = jupiter::get_order(jupiter::USDC_MINT, gm_mint, &raw, taker).await?;
    let out_fmt = jupiter::format_amount(&order.out_amount, gm_dec);
    let result = jupiter::execute_order(w, &order).await?;
    let final_out = result
        .output_amount_result
        .as_deref()
        .map(|r| jupiter::format_amount(r, gm_dec))
        .unwrap_or(out_fmt);
    let desc = format!("{} {} for {} USDC", final_out, sym, usdc_str);
    Ok((desc, result.signature))
}

/// Execute a single sell, returns (description, optional tx signature).
async fn execute_single_sell(
    w: &wallet::Wallet,
    taker: &str,
    sym: &str,
    gm_mint: &str,
    amount: &str,
    gm_dec: u8,
) -> Result<(String, Option<String>)> {
    let sell_f = resolve_percent_amount(amount, || {
        let t = taker.to_string();
        let m = gm_mint.to_string();
        async move {
            let bal = solana::get_balance(&t, &m, None).await?;
            Ok(bal.balance)
        }
    }).await?;
    let sell_str = format!("{:.prec$}", sell_f, prec = gm_dec as usize);
    let raw = jupiter::token_to_raw(&sell_str, gm_dec)?;
    let order = jupiter::get_order(gm_mint, jupiter::USDC_MINT, &raw, taker).await?;
    let out_fmt = jupiter::format_amount(&order.out_amount, jupiter::USDC_DECIMALS);
    let result = jupiter::execute_order(w, &order).await?;
    let final_out = result
        .output_amount_result
        .as_deref()
        .map(|r| jupiter::format_amount(r, jupiter::USDC_DECIMALS))
        .unwrap_or(out_fmt);
    let desc = format!("{} {} → {} USDC", sell_str, sym, final_out);
    Ok((desc, result.signature))
}
