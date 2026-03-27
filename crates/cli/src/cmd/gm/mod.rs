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

// ── Rounded float serializers for token-efficient JSON ─────

/// Round to 2 decimal places (prices, USD values).
fn ser_f64_2<S: serde::Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64((v * 100.0).round() / 100.0)
}
/// Round to 4 decimal places (percentages, slippage).
fn ser_f64_4<S: serde::Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64((v * 10000.0).round() / 10000.0)
}
fn ser_opt_f64_2<S: serde::Serializer>(v: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(val) => s.serialize_some(&((val * 100.0).round() / 100.0)),
        None => s.serialize_none(),
    }
}
fn ser_opt_f64_4<S: serde::Serializer>(v: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(val) => s.serialize_some(&((val * 10000.0).round() / 10000.0)),
        None => s.serialize_none(),
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
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_f64_2")]
    pub input_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_f64_2")]
    pub output_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_f64_4")]
    pub slippage_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_f64_4")]
    pub price_impact_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fee_bps: Option<u32>,
    /// Whether the token is tradable in the current Ondo GM session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tradable: Option<bool>,
    /// Whether this swap is gasless (Jupiter/MM pays gas).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gasless: Option<bool>,
    /// Which router won: jupiterz, iris, dflow, okx.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub router: Option<String>,
}

#[derive(Serialize)]
pub(super) struct TradeJson {
    pub status: &'static str,
    pub amount: String,
    pub token: String,
    pub counter_amount: String,
    pub counter_token: &'static str,
    pub tx: String,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_f64_4")]
    pub slippage_pct: Option<f64>,
    /// Whether this swap was gasless.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gasless: Option<bool>,
    /// Which router was used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub router: Option<String>,
}

#[derive(Serialize)]
pub(super) struct PositionJson {
    pub token: String,
    #[serde(serialize_with = "ser_f64_4")]
    pub balance: f64,
    #[serde(serialize_with = "ser_f64_2")]
    pub price: f64,
    #[serde(serialize_with = "ser_f64_2")]
    pub value_usd: f64,
    #[serde(serialize_with = "ser_f64_2")]
    pub alloc_pct: f64,
    #[serde(serialize_with = "ser_f64_2")]
    pub change_pct_24h: f64,
}

#[derive(Serialize)]
pub(super) struct PortfolioJson {
    pub wallet: String,
    #[serde(serialize_with = "ser_f64_4")]
    pub sol: f64,
    #[serde(serialize_with = "ser_f64_2")]
    pub usdc: f64,
    pub positions: Vec<PositionJson>,
    #[serde(serialize_with = "ser_f64_2")]
    pub total_value_usd: f64,
    #[serde(serialize_with = "ser_f64_2")]
    pub change_24h_usd: f64,
    #[serde(serialize_with = "ser_f64_2")]
    pub change_24h_pct: f64,
}

#[derive(Serialize)]
pub(super) struct HistoryJson {
    pub symbol: String,
    pub range: String,
    pub candles: usize,
    pub first: HistoryCandleJson,
    pub last: HistoryCandleJson,
    #[serde(serialize_with = "ser_f64_2")]
    pub high: f64,
    #[serde(serialize_with = "ser_f64_2")]
    pub low: f64,
    #[serde(serialize_with = "ser_f64_2")]
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
    #[serde(serialize_with = "ser_f64_2")]
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
/// Minimum buy/sell amount in USDC.
const MIN_USDC_AMOUNT: f64 = 1.0;
/// Minimum sell value in USD (Jupiter MM rejects tiny orders).
const MIN_SELL_VALUE_USD: f64 = 1.5;

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

/// Check if a specific token is tradable in the current Ondo session.
/// Returns Ok(()) if tradable, or an error with a clear message if not.
/// Silently passes if the session API is unreachable (fail-open).
async fn check_tradable(symbol: &str) -> Result<()> {
    let session = api::current_session();
    if session == api::Session::Closed {
        return Ok(()); // check_trading_hours() handles this
    }
    let limits = match api::fetch_session_limits().await {
        Ok(l) => l,
        Err(_) => return Ok(()), // fail-open: don't block trade if API is down
    };
    let sym_upper = symbol.to_uppercase();
    let is_tradable = limits.iter().any(|l| {
        l.symbol.to_uppercase() == sym_upper && l.is_tradable(session)
    });
    if !is_tradable {
        return Err(eyre::eyre!(
            "{symbol} is not tradable in current session ({}).\n  \
             Run `rwa gm hours --tradable` to see which tokens are available.\n  \
             Run `rwa gm list --search {symbol}` to check tradable status.",
            session.label()
        ));
    }
    Ok(())
}

/// Fetch the set of tradable token symbols for the current session.
/// Returns an empty set if the API is unreachable (fail-open).
async fn fetch_tradable_set() -> std::collections::HashSet<String> {
    let session = api::current_session();
    if session == api::Session::Closed {
        return std::collections::HashSet::new();
    }
    api::fetch_session_limits().await
        .unwrap_or_default()
        .into_iter()
        .filter(|l| l.is_tradable(session))
        .map(|l| l.symbol.to_uppercase())
        .collect()
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

// Jupiter Ultra handles gas for swaps:
// - JupiterZ (RFQ): market maker pays signature + priority fee
// - Ultra Automatic: Jupiter pays all gas when user has < 0.01 SOL
// - Neither covers token account rent, but Jupiter creates ATAs in the swap tx
// So preflight only needs to check USDC balance (for buy) and trading hours.

async fn preflight_buy(pubkey: &str, usdc_amount: f64, rpc_url: Option<&str>) -> Result<()> {
    check_trading_hours()?;
    if usdc_amount < MIN_USDC_AMOUNT {
        return Err(eyre::eyre!("Minimum buy amount is {MIN_USDC_AMOUNT} USDC"));
    }
    let usdc = solana::get_usdc_balance(pubkey, rpc_url).await?;
    if usdc < usdc_amount {
        return Err(eyre::eyre!(
            "Insufficient USDC: {usdc:.2} USDC (need {usdc_amount:.2})\n  Fund wallet: {pubkey}"
        ));
    }
    Ok(())
}

fn preflight_sell() -> Result<()> {
    check_trading_hours()
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
                // Fresh order needed: expired quote/request, rejected, or RFQ failure
                let needs_new_order = msg.contains("code -1)")   // request expired
                    || msg.contains("code -2003")                // RFQ quote expired
                    || msg.contains("code -2004")                // RFQ swap rejected
                    || msg.contains("code -2005");               // RFQ failure
                // Retry same order: transient landing failure
                let retry_same = msg.contains("code -1000")     // aggregator failed to land
                    || msg.contains("code -2000");               // RFQ MM failed to land

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

    // ── calc_slippage ─────────────────────────────────────────

    #[test]
    fn slippage_from_price_impact() {
        let order = jupiter::OrderResponse {
            in_amount: "100".into(),
            out_amount: "99".into(),
            in_usd_value: Some(100.0),
            out_usd_value: Some(99.0),
            price_impact: Some(-0.5),
            ..Default::default()
        };
        let slip = calc_slippage(&order);
        assert!((slip.unwrap() - (-0.5)).abs() < f64::EPSILON);
    }

    #[test]
    fn slippage_from_usd_values() {
        let order = jupiter::OrderResponse {
            in_amount: "100".into(),
            out_amount: "97".into(),
            in_usd_value: Some(100.0),
            out_usd_value: Some(97.0),
            ..Default::default()
        };
        let slip = calc_slippage(&order);
        assert!((slip.unwrap() - (-3.0)).abs() < 0.01);
    }

    #[test]
    fn slippage_none_when_no_data() {
        let order = jupiter::OrderResponse {
            in_amount: "100".into(),
            out_amount: "97".into(),
            ..Default::default()
        };
        assert!(calc_slippage(&order).is_none());
    }

    // ── check_slippage ────────────────────────────────────────

    #[test]
    fn check_slippage_blocks_above_max() {
        let order = jupiter::OrderResponse {
            in_amount: "100".into(),
            out_amount: "96".into(),
            in_usd_value: Some(100.0),
            out_usd_value: Some(96.0),
            ..Default::default()
        };
        // -4% slippage should be blocked (max is -3%)
        let result = check_slippage(&order, true);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Slippage too high"));
    }

    #[test]
    fn check_slippage_allows_within_limit() {
        let order = jupiter::OrderResponse {
            in_amount: "100".into(),
            out_amount: "99".into(),
            in_usd_value: Some(100.0),
            out_usd_value: Some(99.0),
            ..Default::default()
        };
        // -1% slippage is fine
        let result = check_slippage(&order, true);
        assert!(result.is_ok());
    }
}
