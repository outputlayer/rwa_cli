mod list;
mod portfolio;
mod send;
mod trade;

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
        /// Max slippage in basis points (e.g. 50 = 0.5%). Default: 100 (1%)
        #[arg(long)]
        slippage: Option<u32>,
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
        /// Max slippage in basis points (e.g. 50 = 0.5%). Default: 100 (1%)
        #[arg(long)]
        slippage: Option<u32>,
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
        /// Max slippage in basis points (e.g. 50 = 0.5%). Default: 100 (1%)
        #[arg(long)]
        slippage: Option<u32>,
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

    /// Close empty token accounts and reclaim SOL rent
    Reclaim {
        /// Only reclaim for a specific token (symbol or mint address)
        #[arg(long)]
        token: Option<String>,
    },
}

pub async fn execute(action: GmAction, json: bool, rpc_url: Option<&str>) -> Result<()> {
    match action {
        GmAction::Hours { tradable } => list::hours(json, tradable).await,
        GmAction::Quote { symbol, amount, sell, slippage } => trade::quote(&symbol, &amount, sell, json, rpc_url, Some(slippage.unwrap_or(DEFAULT_SLIPPAGE_BPS))).await,
        GmAction::Buy { symbol, amount, yes, slippage } => trade::buy(&symbol, &amount, yes, json, rpc_url, Some(slippage.unwrap_or(DEFAULT_SLIPPAGE_BPS))).await,
        GmAction::Sell { symbol, amount, yes, slippage } => trade::sell(&symbol, &amount, yes, json, rpc_url, Some(slippage.unwrap_or(DEFAULT_SLIPPAGE_BPS))).await,
        GmAction::Portfolio { wallet } => portfolio::portfolio(wallet.as_deref(), json, rpc_url).await,
        GmAction::History { symbol, range } => portfolio::history(&symbol, &range, json).await,
        GmAction::List { search } => list::list(json, search.as_deref()).await,
        GmAction::Send { token, amount, to, yes } => send::send(&token, &amount, &to, yes, json, rpc_url).await,
        GmAction::CloseAll { amount, yes } => trade::close_all(amount.as_deref(), yes, json, rpc_url).await,
        GmAction::Reclaim { token } => trade::reclaim(token.as_deref(), json, rpc_url).await,
    }
}

// ── JSON output types ──────────────────────────────────────

#[derive(Serialize)]
pub(super) struct HoursJson {
    pub status: &'static str,
    pub session: &'static str,
    pub session_hours: &'static str,
    pub now: String,
    pub countdown: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tradable_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tradable: Option<Vec<String>>,
}

#[derive(Serialize)]
pub(super) struct QuoteJson {
    pub input: String,
    pub input_token: String,
    pub output: String,
    pub output_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slippage_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_impact_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fee_bps: Option<u32>,
}

#[derive(Serialize)]
pub(super) struct TradeJson {
    pub status: &'static str,
    pub amount: String,
    pub token: String,
    pub counter_amount: String,
    pub counter_token: &'static str,
    pub tx: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slippage_pct: Option<f64>,
}

#[derive(Serialize)]
pub(super) struct PositionJson {
    pub token: String,
    pub balance: f64,
    pub price: f64,
    pub value_usd: f64,
    pub alloc_pct: f64,
    pub change_pct_24h: f64,
}

#[derive(Serialize)]
pub(super) struct PortfolioJson {
    pub wallet: String,
    pub sol: f64,
    pub usdc: f64,
    pub positions: Vec<PositionJson>,
    pub total_value_usd: f64,
    pub change_24h_usd: f64,
    pub change_24h_pct: f64,
}

#[derive(Serialize)]
pub(super) struct HistoryJson {
    pub symbol: String,
    pub range: String,
    pub candles: usize,
    pub first: HistoryCandleJson,
    pub last: HistoryCandleJson,
    pub high: f64,
    pub low: f64,
    pub change_pct: f64,
}

#[derive(Serialize)]
pub(super) struct HistoryCandleJson {
    pub timestamp: u64,
    pub price: f64,
}

#[derive(Serialize)]
pub(super) struct ListItemJson {
    pub symbol: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sector: Option<String>,
    pub tradable: bool,
}

#[derive(Serialize)]
pub(super) struct SendJson {
    pub status: &'static str,
    pub token: String,
    pub amount: String,
    pub recipient: String,
    pub tx: String,
}

#[derive(Serialize)]
pub(super) struct CloseAllResultJson {
    pub status: &'static str,
    pub sold: Vec<CloseItemJson>,
    pub failed: Vec<CloseFailJson>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<CloseSkipJson>,
    pub total_usdc: String,
}

#[derive(Serialize)]
pub(super) struct CloseSkipJson {
    pub token: String,
    pub estimated_usd: f64,
    pub reason: &'static str,
}

#[derive(Serialize)]
pub(super) struct CloseItemJson {
    pub token: String,
    pub amount: String,
    pub usdc: String,
    pub tx: String,
}

#[derive(Serialize)]
pub(super) struct CloseFailJson {
    pub token: String,
    pub error: String,
}

#[derive(Serialize)]
pub(super) struct ReclaimJson {
    pub status: &'static str,
    pub accounts_closed: usize,
    pub sol_reclaimed: String,
    pub signatures: Vec<String>,
}

// ── Shared helpers ─────────────────────────────────────────

fn json_out(v: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(v)?);
    Ok(())
}

fn confirm(msg: &str) -> bool {
    print!("{msg} [y/N] ");
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
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

fn resolve_gm_mint(symbol: &str, tokens: &[token_list::GmTokenEntry]) -> Result<(String, String)> {
    let entry = gm::resolve_token(symbol, tokens)?;
    let mint = entry.solana_address
        .ok_or_else(|| eyre::eyre!("No Solana address for {}", entry.symbol))?;
    Ok((entry.symbol.to_string(), mint.to_string()))
}

/// Compute `value * pct / 100` using integer math to avoid f64 precision loss.
fn pct_of_u128(value: u128, pct: f64) -> u128 {
    let bps = (pct * 100.0).round() as u128;
    value * bps / 10_000
}

/// Maximum allowed slippage before blocking the trade.
const MAX_SLIPPAGE_PCT: f64 = 3.0;
/// Slippage threshold that triggers a fresh quote retry.
const SLIPPAGE_RETRY_PCT: f64 = 1.0;
/// Maximum retries when slippage exceeds the retry threshold.
const MAX_SLIPPAGE_RETRIES: u32 = 3;
/// Default slippage limit in basis points (100 = 1%).
const DEFAULT_SLIPPAGE_BPS: u32 = 100;
/// Minimum SOL needed for transaction fees.
const MIN_SOL_FOR_GAS: f64 = 0.01;
/// Minimum buy/sell amount in USDC.
const MIN_USDC_AMOUNT: f64 = 1.0;
/// Minimum sell value in USD (Jupiter MM rejects tiny orders).
const MIN_SELL_VALUE_USD: f64 = 1.5;
/// USDC amount to swap for SOL when gas is low.
const TOPUP_USDC: f64 = 3.0;

fn calc_slippage(order: &jupiter::OrderResponse) -> Option<f64> {
    if let Some(pi) = order.price_impact {
        return Some(pi);
    }
    match (order.in_usd_value, order.out_usd_value) {
        (Some(usd_in), Some(usd_out)) if usd_in > 0.0 => {
            Some((usd_out - usd_in) / usd_in * 100.0)
        }
        _ => None,
    }
}

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

/// Get an order, retrying with fresh quotes if slippage exceeds the threshold.
async fn get_order_checked(
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
    json: bool,
) -> Result<(jupiter::OrderResponse, Option<f64>)> {
    let mut order = jupiter::get_order(input_mint, output_mint, amount, taker, slippage_bps).await?;
    for attempt in 1..=MAX_SLIPPAGE_RETRIES {
        let slip = calc_slippage(&order);
        if let Some(s) = slip {
            if s < -MAX_SLIPPAGE_PCT {
                return Err(eyre::eyre!(
                    "Slippage too high ({s:.2}%). Max allowed: -{MAX_SLIPPAGE_PCT:.0}%. \
                     Try a smaller amount or wait for better liquidity."
                ));
            }
            if s < -SLIPPAGE_RETRY_PCT && attempt <= MAX_SLIPPAGE_RETRIES {
                if !json {
                    eprintln!(
                        "Slippage {s:.2}% exceeds -{SLIPPAGE_RETRY_PCT:.0}% — refreshing quote ({attempt}/{MAX_SLIPPAGE_RETRIES})..."
                    );
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                order = jupiter::get_order(input_mint, output_mint, amount, taker, slippage_bps).await?;
                continue;
            }
        }
        // Slippage acceptable or unknown — proceed
        let slippage_pct = slip;
        if let Some(s) = slippage_pct {
            if s < -1.0 && !json {
                eprintln!("Warning: slippage {s:.2}%");
            }
        }
        return Ok((order, slippage_pct));
    }
    // Exhausted retries — use last order, apply normal check
    let slippage_pct = check_slippage(&order, json)?;
    Ok((order, slippage_pct))
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

/// Resolve percentage or "all" amounts into absolute numbers.
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
                "Insufficient USDC: {usdc:.2} USDC (need {usdc_amount:.2})\n  Fund wallet: {pubkey}"
            ));
        }
        return Ok(());
    }
    let mut issues = Vec::new();
    if sol < MIN_SOL_FOR_GAS {
        issues.push(format!("Insufficient SOL for gas: {sol:.6} SOL (need ≥{MIN_SOL_FOR_GAS})"));
    }
    if usdc < usdc_amount {
        issues.push(format!("Insufficient USDC: {usdc:.2} USDC (need {usdc_amount:.2})"));
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
            "Insufficient SOL for gas: {sol:.6} SOL (need ≥{MIN_SOL_FOR_GAS})\n  Fund wallet: {pubkey}"
        ));
    }
    Ok(())
}

async fn topup_sol(w: &wallet::Wallet, pubkey: &str, json: bool, rpc_url: Option<&str>) -> Result<()> {
    if !json {
        eprintln!("SOL too low for gas — swapping ${TOPUP_USDC:.0} USDC → SOL ...");
    }
    let raw_usdc = jupiter::usdc_to_raw(&format!("{TOPUP_USDC:.2}"))?;
    let order = jupiter::get_order(jupiter::USDC_MINT, jupiter::SOL_MINT, &raw_usdc, pubkey, None).await?;
    execute_with_retry(w, &order, json, jupiter::USDC_MINT, jupiter::SOL_MINT, &raw_usdc, pubkey, None).await?;
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    let new_sol = solana::get_sol_balance(pubkey, rpc_url).await?;
    if !json {
        eprintln!("SOL topped up: {new_sol:.6} SOL");
    }
    Ok(())
}

const MAX_SWAP_RETRIES: u32 = 2;

#[allow(clippy::too_many_arguments)]
async fn execute_with_retry(
    w: &wallet::Wallet,
    order: &jupiter::OrderResponse,
    json: bool,
    input_mint: &str,
    output_mint: &str,
    raw_amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
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
                        jupiter::get_order(input_mint, output_mint, raw_amount, taker, slippage_bps).await?
                    );
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| eyre::eyre!("Swap failed after retries")))
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── pct_of_u128 ──────────────────────────────────────────

    #[test]
    fn pct_50_of_1000() {
        assert_eq!(pct_of_u128(1_000_000_000, 50.0), 500_000_000);
    }

    #[test]
    fn pct_100_of_value() {
        assert_eq!(pct_of_u128(1_000_000_000, 100.0), 1_000_000_000);
    }

    #[test]
    fn pct_10_of_value() {
        assert_eq!(pct_of_u128(1_000_000_000, 10.0), 100_000_000);
    }

    #[test]
    fn pct_33_33_of_value() {
        // 33.33% of 1_000_000_000 = 333_300_000 (via 3333 bps)
        assert_eq!(pct_of_u128(1_000_000_000, 33.33), 333_300_000);
    }

    #[test]
    fn pct_zero() {
        assert_eq!(pct_of_u128(1_000_000_000, 0.0), 0);
    }

    #[test]
    fn pct_large_value() {
        // Test with a value that would overflow f64 precision
        let large: u128 = 999_999_999_999_999_999;
        let result = pct_of_u128(large, 50.0);
        assert_eq!(result, large * 5000 / 10_000);
    }

    // ── clean_name ────────────────────────────────────────────

    #[test]
    fn clean_name_removes_suffix() {
        assert_eq!(clean_name("Tesla (Ondo Tokenized)"), "Tesla");
    }

    #[test]
    fn clean_name_no_suffix() {
        assert_eq!(clean_name("Apple"), "Apple");
    }

    // ── token_type_from_name ──────────────────────────────────

    #[test]
    fn detect_etf() {
        assert_eq!(token_type_from_name("SPDR S&P 500 ETF Trust"), "etf");
        assert_eq!(token_type_from_name("Vanguard Total Stock Market Index Fund"), "etf");
    }

    #[test]
    fn detect_stock() {
        assert_eq!(token_type_from_name("Tesla"), "stock");
        assert_eq!(token_type_from_name("Apple Inc"), "stock");
    }
}
