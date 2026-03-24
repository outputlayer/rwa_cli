use alloy_primitives::{Address, U256};
use clap::Subcommand;
use eyre::Result;
use rwa_ondo::{gm, oracle, provider, token_list};

#[derive(Subcommand, Debug)]
pub enum GmAction {
    /// List all available GM tokens
    List,

    /// Get token price from Chainlink Tokenized Equity Feed (ETH Mainnet)
    Price {
        /// Token symbol (e.g. TSLA, TSLAon, SPY, QQQ, CRCL)
        symbol: String,

        /// Custom Chainlink feed address (overrides built-in registry)
        #[arg(short, long)]
        feed: Option<String>,

        /// Ethereum RPC URL (default: public ETH RPC)
        #[arg(long, env = "RWA_ETH_RPC_URL")]
        eth_rpc_url: Option<String>,
    },

    /// Check GM token balances for a wallet (BSC)
    Balance {
        /// Wallet address to check
        wallet: String,

        /// Specific token symbol (optional, shows all if omitted)
        #[arg(short, long)]
        token: Option<String>,
    },

    /// Show detailed info about a GM token (BSC)
    Info {
        /// Token symbol (e.g. TSLA, TSLAon)
        symbol: String,
    },
}

pub async fn execute(action: GmAction, rpc_url: &str) -> Result<()> {
    let tokens = token_list::get_token_list().await;

    match action {
        GmAction::List => list_tokens(&tokens),
        GmAction::Price { symbol, feed, eth_rpc_url } => {
            let eth_rpc = eth_rpc_url
                .unwrap_or_else(|| rwa_core::chain::Chain::EthereumMainnet.default_rpc_url().to_string());
            price(&eth_rpc, &symbol, feed.as_deref(), &tokens).await
        }
        GmAction::Balance { wallet, token } => balance(rpc_url, &wallet, token.as_deref(), &tokens).await,
        GmAction::Info { symbol } => info(rpc_url, &symbol, &tokens).await,
    }
}

fn list_tokens(tokens: &[token_list::GmTokenEntry]) -> Result<()> {
    println!("{:<12} {:<44} {}", "SYMBOL", "BSC ADDRESS", "ETH ADDRESS");
    println!("{}", "-".repeat(100));
    for t in tokens {
        let bsc = t.bsc_address.map(|a| format!("{a}")).unwrap_or_else(|| "-".into());
        let eth = t.eth_address.map(|a| format!("{a}")).unwrap_or_else(|| "-".into());
        println!("{:<12} {:<44} {}", t.symbol, bsc, eth);
    }
    println!("\nTotal: {} tokens", tokens.len());
    Ok(())
}

async fn price(eth_rpc_url: &str, symbol: &str, feed: Option<&str>, tokens: &[token_list::GmTokenEntry]) -> Result<()> {
    let entry = gm::resolve_token(symbol, tokens)?;
    let eth_provider = provider::create_provider(eth_rpc_url).await?;

    let feed_override: Option<Address> = feed.map(|f| f.parse()).transpose()?;

    let tp = oracle::get_token_price(&eth_provider, &entry.symbol, feed_override).await?;

    println!("{}:", entry.symbol);
    println!("  Token price (USD): ${:.2}  ({})", tp.token_price_usd, tp.feed_description);
    println!("  Feed updated:      {}", tp.price_updated_at);

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
            println!("{:<12} {}", "TOKEN", "BALANCE");
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
    println!("Shares/Token:    {}", format_u256_decimals(oracle_data.shares_per_token, 18));
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
