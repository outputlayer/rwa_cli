use alloy_primitives::{Address, U256};
use clap::Subcommand;
use eyre::Result;
use rwa_ondo::{api, gm, oracle, provider, token_list};

#[derive(Subcommand, Debug)]
pub enum GmAction {
    /// List all available GM tokens
    List,

    /// <SYMBOL|all>         Get token USD price and 24h change
    Price {
        /// Token symbol (e.g. TSLA, TSLAon) — or "all" for all tokens
        symbol: String,
    },

    /// <WALLET> [-t TOKEN]  Check GM token balances on BNB Chain
    Balance {
        /// Wallet address (0x...)
        wallet: String,

        /// Filter by token symbol (optional)
        #[arg(short, long)]
        token: Option<String>,
    },

    /// <SYMBOL>             Detailed token info (price, sValue, holders, 52w)
    Info {
        /// Token symbol (e.g. TSLA, TSLAon)
        symbol: String,
    },

    /// <WALLET>             Portfolio with USD values and 24h P&L
    Portfolio {
        /// Wallet address (0x...)
        wallet: String,
    },
}

pub async fn execute(action: GmAction, rpc_url: &str) -> Result<()> {
    // Price only needs the Ondo API — skip token list fetch (~170ms saved)
    if let GmAction::Price { ref symbol } = action {
        return price(symbol).await;
    }

    let tokens = token_list::get_token_list().await;

    match action {
        GmAction::List => list_tokens(&tokens),
        GmAction::Price { .. } => unreachable!(),
        GmAction::Balance { wallet, token } => balance(rpc_url, &wallet, token.as_deref(), &tokens).await,
        GmAction::Info { symbol } => info(rpc_url, &symbol, &tokens).await,
        GmAction::Portfolio { wallet } => portfolio(rpc_url, &wallet, &tokens).await,
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
        println!("{:<12} {:>12} {:>10} {:>8} {:>10}", "TOKEN", "PRICE (USD)", "24h CHG", "24h %", "sVALUE");
        println!("{}", "-".repeat(60));
        for asset in &assets {
            if let Some(pm) = &asset.primary_market {
                let price = api::parse_price(&pm.price);
                let chg = pm.price_change_24h.as_deref().map(api::parse_price).unwrap_or(0.0);
                let pct = pm.price_change_pct_24h.as_deref().map(api::parse_price).unwrap_or(0.0);
                let sv = pm.shares_multiplier.as_deref().unwrap_or("-");
                println!(
                    "{:<12} {:>12.2} {:>+10.2} {:>7.2}% {:>10}",
                    asset.symbol, price, chg, pct, sv
                );
            }
        }
        println!("\nTotal: {} tokens", assets.len());
    } else {
        let asset = api::find_asset(symbol, &assets)
            .ok_or_else(|| eyre::eyre!("Token {} not found in Ondo API", symbol))?;
        let pm = asset.primary_market.as_ref()
            .ok_or_else(|| eyre::eyre!("No market data for {}", asset.symbol))?;

        let price = api::parse_price(&pm.price);
        let chg = pm.price_change_24h.as_deref().map(api::parse_price).unwrap_or(0.0);
        let pct = pm.price_change_pct_24h.as_deref().map(api::parse_price).unwrap_or(0.0);

        println!("{} ({}):", asset.symbol, asset.asset_name);
        println!("  Price (USD):       ${:.2}", price);
        println!("  24h Change:        {:+.2} ({:+.2}%)", chg, pct);
        if let Some(sv) = &pm.shares_multiplier {
            println!("  sValue:            {}", sv);
        }
        if let Some(holders) = pm.total_holders {
            println!("  Holders:           {}", holders);
        }
        if !pm.tradable_sessions.is_empty() {
            println!("  Trading:           {}", pm.tradable_sessions.join(", "));
        }

        if let Some(um) = &asset.underlying_market {
            println!("  ─── Underlying: {} ───", um.name);
            if let (Some(hi), Some(lo)) = (&um.price_high_52w, &um.price_low_52w) {
                println!("  52w Range:         ${} — ${}", lo, hi);
            }
            if let Some(vol) = &um.volume {
                println!("  Volume:            {}", format_compact_usd(vol));
            }
            if let Some(cap) = &um.market_cap {
                println!("  Market Cap:        {}", format_compact_usd(cap));
            }
        }
    }

    Ok(())
}

async fn balance(rpc_url: &str, wallet: &str, token: Option<&str>, tokens: &[token_list::GmTokenEntry]) -> Result<()> {
    let wallet_addr: Address = wallet.parse()?;
    let provider = provider::create_provider(rpc_url).await?;

    if let Some(sym) = token {
        let entry = gm::resolve_token(sym, tokens)?;
        let token_addr = entry.bsc_address
            .ok_or_else(|| eyre::eyre!("No BSC address for {}", entry.symbol))?;
        let bal = gm::get_balance(&provider, token_addr, wallet_addr).await?;
        println!("{}: {}", entry.symbol, format_u256_decimals(bal, 18));
    } else {
        let balances = gm::get_all_balances(&provider, wallet_addr, tokens).await?;
        if balances.is_empty() {
            println!("No GM token balances found for {wallet}");
        } else {
            println!("{:<12} BALANCE", "TOKEN");
            println!("{}", "-".repeat(40));
            for tb in &balances {
                println!("{:<12} {}", tb.token.symbol, format_u256_decimals(tb.balance, 18));
            }
        }
    }

    Ok(())
}

async fn info(rpc_url: &str, symbol: &str, tokens: &[token_list::GmTokenEntry]) -> Result<()> {
    let entry = gm::resolve_token(symbol, tokens)?;
    let address = entry.bsc_address
        .ok_or_else(|| eyre::eyre!("No BSC address for {}", entry.symbol))?;
    let provider = provider::create_provider(rpc_url).await?;

    let token_info = gm::get_token_info(&provider, address).await?;
    let oracle_data = oracle::get_oracle_data(&provider, address).await?;

    println!("Token:           {}", token_info.name);
    println!("Symbol:          {}", token_info.symbol);
    println!("BSC Address:     {}", address);
    if let Some(eth) = entry.eth_address {
        println!("ETH Address:     {}", eth);
    }
    println!("Decimals:        {}", token_info.decimals);
    println!("sValue:          {:.6}", oracle_data.value);
    println!("sValue (raw):    {}", format_u256_decimals(oracle_data.raw, 18));
    println!("Explorer:        {}", rwa_core::chain::Chain::BnbMainnet.explorer_url(address));

    Ok(())
}

async fn portfolio(rpc_url: &str, wallet: &str, tokens: &[token_list::GmTokenEntry]) -> Result<()> {
    let wallet_addr: Address = wallet.parse()?;
    let provider = provider::create_provider(rpc_url).await?;

    // Fetch balances and prices in parallel
    let (balances, assets) = tokio::join!(
        gm::get_all_balances(&provider, wallet_addr, tokens),
        api::fetch_assets()
    );
    let balances = balances?;
    let assets = assets?;

    if balances.is_empty() {
        println!("No GM token balances found for {wallet}");
        return Ok(());
    }

    println!("Portfolio for {wallet}\n");
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
        // previous value = current_value / (1 + pct/100)
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
