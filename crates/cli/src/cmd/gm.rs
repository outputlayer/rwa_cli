use alloy_primitives::{Address, U256};
use clap::Subcommand;
use eyre::Result;
use rwa_ondo::{gm, oracle, provider, token_list::GM_TOKENS};

#[derive(Subcommand, Debug)]
pub enum GmAction {
    /// List all available GM tokens
    List,

    /// Get oracle price data for a GM token
    Price {
        /// Token symbol (e.g. TSLA, TSLAon, AAPL)
        symbol: String,
    },

    /// Check GM token balances for a wallet
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
    match action {
        GmAction::List => list_tokens(),
        GmAction::Price { symbol } => price(rpc_url, &symbol).await,
        GmAction::Balance { wallet, token } => balance(rpc_url, &wallet, token.as_deref()).await,
        GmAction::Info { symbol } => info(rpc_url, &symbol).await,
    }
}

fn list_tokens() -> Result<()> {
    println!("{:<12} {}", "SYMBOL", "ADDRESS");
    println!("{}", "-".repeat(56));
    for &(symbol, address) in GM_TOKENS.iter() {
        println!("{:<12} {}", symbol, address);
    }
    println!("\nTotal: {} tokens", GM_TOKENS.len());
    Ok(())
}

async fn price(rpc_url: &str, symbol: &str) -> Result<()> {
    let &(_sym, addr_str) = gm::resolve_token(symbol)?;
    let address: Address = addr_str.parse()?;
    let provider = provider::create_provider(rpc_url).await?;

    let data = oracle::get_oracle_data(&provider, address).await?;
    let shares = format_u256_decimals(data.shares_per_token, 18);

    println!("Oracle data for {_sym}:");
    println!("  Shares per token:      {shares}");
    println!("  Last updated:          {} (unix)", data.last_update_timestamp);
    println!("  Max diff (bps):        {}", data.max_absolute_diff_bps);
    println!("  Min update window:     {}s", data.min_price_update_window);

    Ok(())
}

async fn balance(rpc_url: &str, wallet: &str, token: Option<&str>) -> Result<()> {
    let wallet_addr: Address = wallet.parse()?;
    let provider = provider::create_provider(rpc_url).await?;

    if let Some(sym) = token {
        let &(_sym, addr_str) = gm::resolve_token(sym)?;
        let token_addr: Address = addr_str.parse()?;
        let bal = gm::get_balance(&provider, token_addr, wallet_addr).await?;
        println!("{}: {}", _sym, format_u256_decimals(bal, 18));
    } else {
        let balances = gm::get_all_balances(&provider, wallet_addr).await?;
        if balances.is_empty() {
            println!("No GM token balances found for {wallet}");
        } else {
            println!("{:<12} {}", "TOKEN", "BALANCE");
            println!("{}", "-".repeat(40));
            for tb in &balances {
                println!("{:<12} {}", tb.token.symbol, format_u256_decimals(tb.balance, 18));
            }
        }
    }

    Ok(())
}

async fn info(rpc_url: &str, symbol: &str) -> Result<()> {
    let &(_sym, addr_str) = gm::resolve_token(symbol)?;
    let address: Address = addr_str.parse()?;
    let provider = provider::create_provider(rpc_url).await?;

    let token_info = gm::get_token_info(&provider, address).await?;
    let oracle_data = oracle::get_oracle_data(&provider, address).await?;

    println!("Token:           {}", token_info.name);
    println!("Symbol:          {}", token_info.symbol);
    println!("Address:         {}", token_info.address);
    println!("Decimals:        {}", token_info.decimals);
    println!("Shares/Token:    {}", format_u256_decimals(oracle_data.shares_per_token, 18));
    println!("Oracle updated:  {} (unix)", oracle_data.last_update_timestamp);
    println!("Explorer:        {}", rwa_core::chain::Chain::BnbMainnet.explorer_url(address));

    Ok(())
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
