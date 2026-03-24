use alloy_primitives::{Address, U256};
use clap::Subcommand;
use eyre::Result;
use rwa_core::chain::Chain;
use rwa_ondo::{api, gm, jupiter, oracle, provider, solana, token_list, wallet};

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

    /// <SYMBOL|all>         Get token USD price and 24h change
    Price {
        /// Token symbol (e.g. TSLA, TSLAon) — or "all" for all tokens
        symbol: String,
    },

    /// <WALLET> [-t TOKEN]  Check GM token balances
    Balance {
        /// Wallet address (0x... for EVM, base58 for Solana)
        wallet: String,

        /// Filter by token symbol (optional)
        #[arg(short, long)]
        token: Option<String>,

        /// Chain: solana (default), bsc, or eth
        #[arg(short, long, default_value = "solana")]
        chain: String,
    },

    /// <SYMBOL>             Detailed token info (price, sValue, holders, 52w)
    Info {
        /// Token symbol (e.g. TSLA, TSLAon)
        symbol: String,
    },

    /// <WALLET>             Portfolio with USD values and 24h P&L
    Portfolio {
        /// Wallet address (0x... for EVM, base58 for Solana)
        wallet: String,

        /// Chain: solana (default), bsc, or eth
        #[arg(short, long, default_value = "solana")]
        chain: String,
    },

    /// <SYMBOL> --amount <USDC>  Buy GM token with USDC via Jupiter (Solana)
    Buy {
        /// Token symbol (e.g. TSLA, TSLAon)
        symbol: String,

        /// USDC amount to spend
        #[arg(short, long)]
        amount: String,
    },

    /// <SYMBOL> --amount <N>     Sell GM token for USDC via Jupiter (Solana)
    Sell {
        /// Token symbol (e.g. TSLA, TSLAon)
        symbol: String,

        /// Token amount to sell
        #[arg(short, long)]
        amount: String,
    },

    /// <SYMBOL> --amount <USDC>  Get swap quote without executing (Solana)
    Quote {
        /// Token symbol (e.g. TSLA, TSLAon)
        symbol: String,

        /// USDC amount (for buy quote) or token amount (with --sell)
        #[arg(short, long)]
        amount: String,

        /// Quote selling token for USDC instead of buying
        #[arg(long)]
        sell: bool,
    },
}

pub async fn execute(action: GmAction, rpc_url_override: Option<&str>) -> Result<()> {
    // Price only needs the Ondo API — skip token list fetch (~170ms saved)
    if let GmAction::Price { ref symbol } = action {
        return price(symbol).await;
    }

    let tokens = token_list::get_token_list().await;

    match action {
        GmAction::List => list_tokens(&tokens),
        GmAction::Price { .. } => unreachable!(),
        GmAction::Balance { wallet, token, chain } => {
            let chain = parse_chain(&chain)?;
            if chain == Chain::SolanaMainnet {
                balance_solana(&wallet, token.as_deref(), &tokens).await
            } else {
                let rpc = rpc_url_override.unwrap_or(chain.default_rpc_url());
                balance_evm(rpc, &wallet, token.as_deref(), &tokens, chain).await
            }
        }
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
        GmAction::Buy { symbol, amount } => buy(&symbol, &amount, &tokens).await,
        GmAction::Sell { symbol, amount } => sell(&symbol, &amount, &tokens).await,
        GmAction::Quote { symbol, amount, sell } => quote(&symbol, &amount, sell, &tokens).await,
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

async fn balance_evm(rpc_url: &str, wallet: &str, token: Option<&str>, tokens: &[token_list::GmTokenEntry], chain: Chain) -> Result<()> {
    let wallet_addr: Address = wallet.parse()?;
    let provider = provider::create_provider(rpc_url).await?;

    if let Some(sym) = token {
        let entry = gm::resolve_token(sym, tokens)?;
        let token_addr = gm::token_address_for_chain(entry, chain)
            .ok_or_else(|| eyre::eyre!("No {} address for {}", chain, entry.symbol))?;
        let bal = gm::get_balance(&provider, token_addr, wallet_addr).await?;
        println!("{}: {}", entry.symbol, format_u256_decimals(bal, 18));
    } else {
        let balances = gm::get_all_balances(&provider, wallet_addr, tokens, chain).await?;
        if balances.is_empty() {
            println!("No GM token balances found on {} for {wallet}", chain);
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
    if let Some(sol) = &entry.solana_address {
        println!("SOL Address:     {}", sol);
    }
    println!("Decimals:        {}", token_info.decimals);
    println!("sValue:          {:.6}", oracle_data.value);
    println!("sValue (raw):    {}", format_u256_decimals(oracle_data.raw, 18));
    println!("Explorer:        {}", rwa_core::chain::Chain::BnbMainnet.explorer_url(address));

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

async fn balance_solana(wallet: &str, token: Option<&str>, tokens: &[token_list::GmTokenEntry]) -> Result<()> {
    if let Some(sym) = token {
        let entry = gm::resolve_token(sym, tokens)?;
        let mint = entry.solana_address.as_deref()
            .ok_or_else(|| eyre::eyre!("No Solana address for {}", entry.symbol))?;
        let bal = solana::get_balance(wallet, mint, None).await?;
        println!("{}: {}", entry.symbol, bal.balance);
    } else {
        let balances = solana::get_all_balances(wallet, tokens, None).await?;
        if balances.is_empty() {
            println!("No GM token balances found on Solana for {wallet}");
        } else {
            println!("{:<12} BALANCE", "TOKEN");
            println!("{}", "-".repeat(40));
            for tb in &balances {
                println!("{:<12} {}", tb.symbol, tb.balance);
            }
        }
    }
    Ok(())
}

async fn portfolio_solana(wallet: &str, tokens: &[token_list::GmTokenEntry]) -> Result<()> {
    let (balances, assets) = tokio::join!(
        solana::get_all_balances(wallet, tokens, None),
        api::fetch_assets()
    );
    let balances = balances?;
    let assets = assets?;

    if balances.is_empty() {
        println!("No GM token balances found on Solana for {wallet}");
        return Ok(());
    }

    println!("Solana Portfolio for {wallet}\n");
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

async fn preflight_buy(pubkey: &str, usdc_amount: f64) -> Result<()> {
    if usdc_amount < MIN_USDC_AMOUNT {
        return Err(eyre::eyre!("Minimum buy amount is {MIN_USDC_AMOUNT} USDC"));
    }

    let (sol, usdc) = tokio::join!(
        solana::get_sol_balance(pubkey, None),
        solana::get_usdc_balance(pubkey, None)
    );
    let sol = sol?;
    let usdc = usdc?;

    if sol < MIN_SOL_FOR_GAS {
        return Err(eyre::eyre!(
            "Insufficient SOL for gas fees.\n  \
             Balance: {sol:.6} SOL (need ≥{MIN_SOL_FOR_GAS} SOL)\n  \
             Send SOL to: {pubkey}"
        ));
    }
    if usdc < usdc_amount {
        return Err(eyre::eyre!(
            "Insufficient USDC balance.\n  \
             Balance: {usdc:.2} USDC (need {usdc_amount:.2} USDC)\n  \
             Send USDC to: {pubkey}"
        ));
    }
    Ok(())
}

async fn preflight_sell(pubkey: &str) -> Result<()> {
    let sol = solana::get_sol_balance(pubkey, None).await?;
    if sol < MIN_SOL_FOR_GAS {
        return Err(eyre::eyre!(
            "Insufficient SOL for gas fees.\n  \
             Balance: {sol:.6} SOL (need ≥{MIN_SOL_FOR_GAS} SOL)\n  \
             Send SOL to: {pubkey}"
        ));
    }
    Ok(())
}

async fn quote(symbol: &str, amount: &str, is_sell: bool, tokens: &[token_list::GmTokenEntry]) -> Result<()> {
    let (sym, gm_mint) = resolve_gm_mint(symbol, tokens)?;
    let w = load_wallet()?;
    let taker = w.pubkey();

    let gm_dec = jupiter::GM_SOL_DECIMALS;
    let usdc_dec = jupiter::USDC_DECIMALS;

    let (input_mint, output_mint, raw_amount, direction) = if is_sell {
        let raw = jupiter::token_to_raw(amount, gm_dec)?;
        (gm_mint.as_str(), jupiter::USDC_MINT, raw, "sell")
    } else {
        let raw = jupiter::usdc_to_raw(amount)?;
        (jupiter::USDC_MINT, gm_mint.as_str(), raw, "buy")
    };

    let order = jupiter::get_order(input_mint, output_mint, &raw_amount, &taker).await?;

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

async fn buy(symbol: &str, amount: &str, tokens: &[token_list::GmTokenEntry]) -> Result<()> {
    let (sym, gm_mint) = resolve_gm_mint(symbol, tokens)?;
    let w = load_wallet()?;
    let taker = w.pubkey();

    let usdc_f: f64 = amount.parse().map_err(|_| eyre::eyre!("Invalid amount: {amount}"))?;
    preflight_buy(&taker, usdc_f).await?;

    let raw_usdc = jupiter::usdc_to_raw(amount)?;
    let gm_dec = jupiter::GM_SOL_DECIMALS;

    println!("Getting quote for {} USDC → {} ...", amount, sym);
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

    println!("Executing swap...");
    let result = jupiter::execute_order(&w, &order).await?;

    let final_out = result.output_amount_result.as_deref()
        .map(|r| jupiter::format_amount(r, gm_dec))
        .unwrap_or(out_fmt);

    let sig = result.signature.as_deref().unwrap_or("unknown");
    println!("\nSwap successful!");
    println!("  Bought:    {} {}", final_out, sym);
    println!("  Spent:     {} USDC", amount);
    println!("  Tx:        https://solscan.io/tx/{}", sig);

    Ok(())
}

async fn sell(symbol: &str, amount: &str, tokens: &[token_list::GmTokenEntry]) -> Result<()> {
    let (sym, gm_mint) = resolve_gm_mint(symbol, tokens)?;
    let w = load_wallet()?;
    let taker = w.pubkey();

    preflight_sell(&taker).await?;

    let gm_dec = jupiter::GM_SOL_DECIMALS;
    let raw_gm = jupiter::token_to_raw(amount, gm_dec)?;

    println!("Getting quote for {} {} → USDC ...", amount, sym);
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
