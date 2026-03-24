use alloy_primitives::{Address, U256};
use clap::Subcommand;
use eyre::Result;
use rwa_ondo::{api, gm, oracle, provider, token_list};

#[derive(Subcommand, Debug)]
pub enum GmAction {
    /// List all available GM tokens
    List,

    /// Get GM token price (USD) from official Ondo API
    Price {
        /// Token symbol (e.g. TSLA, TSLAon) — or "all" for all tokens
        symbol: String,
    },

    /// Check GM token balances for a wallet (BSC)
    Balance {
        /// Wallet address to check
        wallet: String,

        /// Specific token symbol (optional, shows all if omitted)
        #[arg(short, long)]
        token: Option<String>,
    },

    /// Show detailed info about a GM token
    Info {
        /// Token symbol (e.g. TSLA, TSLAon)
        symbol: String,
    },
}

pub async fn execute(action: GmAction, rpc_url: &str) -> Result<()> {
    let tokens = token_list::get_token_list().await;

    match action {
        GmAction::List => list_tokens(&tokens),
        GmAction::Price { symbol } => price(&symbol).await,
        GmAction::Balance { wallet, token } => balance(rpc_url, &wallet, token.as_deref(), &tokens).await,
        GmAction::Info { symbol } => info(rpc_url, &symbol, &tokens).await,
    }
}

fn list_tokens(tokens: &[token_list::GmTokenEntry]) -> Result<()> {
    println!("{:<12} {:<44} ETH ADDRESS", "SYMBOL", "BSC ADDRESS");
    println!("{}", "-".repeat(100));
    for t in tokens {
        let bsc = t.bsc_address.map(|a| format!("{a}")).unwrap_or_else(|| "-".into());
        let eth = t.eth_address.map(|a| format!("{a}")).unwrap_or_else(|| "-".into());
        println!("{:<12} {:<44} {}", t.symbol, bsc, eth);
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
                println!("  Volume:            {}", format_volume(vol));
            }
            if let Some(cap) = &um.market_cap {
                println!("  Market Cap:        {}", format_market_cap(cap));
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

fn format_volume(v: &str) -> String {
    let n: f64 = v.parse().unwrap_or(0.0);
    if n >= 1_000_000_000.0 {
        format!("${:.2}B", n / 1_000_000_000.0)
    } else if n >= 1_000_000.0 {
        format!("${:.2}M", n / 1_000_000.0)
    } else if n >= 1_000.0 {
        format!("${:.1}K", n / 1_000.0)
    } else {
        v.to_string()
    }
}

fn format_market_cap(v: &str) -> String {
    let n: f64 = v.parse().unwrap_or(0.0);
    if n >= 1_000_000_000_000.0 {
        format!("${:.2}T", n / 1_000_000_000_000.0)
    } else if n >= 1_000_000_000.0 {
        format!("${:.2}B", n / 1_000_000_000.0)
    } else if n >= 1_000_000.0 {
        format!("${:.2}M", n / 1_000_000.0)
    } else {
        format!("${}", v)
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
