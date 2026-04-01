use eyre::{Result, WrapErr, eyre};

use crate::{amounts, api, gm, jupiter, solana, token_list, wallet};
use crate::types::{Mint, Symbol};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GmTradeErrorKind {
    MarketClosed,
    NotTradable,
    SlippageTooHigh,
}

#[derive(Debug)]
pub struct GmTradeError {
    pub kind: GmTradeErrorKind,
    pub detail: String,
}

impl GmTradeError {
    fn new(kind: GmTradeErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for GmTradeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GM trade error [{}]: {}", self.kind, self.detail)
    }
}

impl std::fmt::Display for GmTradeErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::MarketClosed => "market_closed",
            Self::NotTradable => "not_tradable",
            Self::SlippageTooHigh => "slippage_too_high",
        };
        f.write_str(label)
    }
}

impl std::error::Error for GmTradeError {}

/// Safety-net slippage — block the trade entirely if ALL MMs return worse than this.
const MAX_SLIPPAGE_PCT: f64 = 10.0;
/// Slippage threshold that triggers a fresh quote retry (seek a better MM).
const SLIPPAGE_RETRY_PCT: f64 = 1.0;
/// Maximum retries when slippage exceeds the retry threshold.
/// jupiterz routes to multiple MMs — one may quote -10% on small orders.
/// Retrying cycles through MMs until we get a reasonable fill.
const MAX_SLIPPAGE_RETRIES: u32 = 5;
/// Maximum retries for transient swap execution failures.
const MAX_SWAP_RETRIES: u32 = 2;
/// Default slippage limit in basis points (100 = 1%).
pub const DEFAULT_SLIPPAGE_BPS: u32 = 100;
/// Minimum buy/sell amount in USDC.
const MIN_USDC_AMOUNT: f64 = 1.0;
/// Minimum sell value in USD (Jupiter MM rejects tiny orders).
pub const MIN_SELL_VALUE_USD: f64 = 1.5;
/// Minimum SOL balance required to cover transaction fees (rent + priority).
const MIN_SOL_FOR_FEES: f64 = 0.002;

pub struct SwapPlan {
    pub symbol: Symbol,
    pub amount: String,
    pub counter_amount: String,
    pub order: jupiter::OrderResponse,
    pub slippage_pct: Option<f64>,
    pub swap: SwapParamsOwned,
    output_decimals: u8,
}

pub struct SwapExecution {
    pub output_amount: String,
    pub signature: String,
    /// Actual slippage computed from execute response vs order quote.
    /// `None` when the execute response omits amount fields.
    pub actual_slippage_pct: Option<f64>,
}

pub struct CloseSkip {
    pub token: String,
    pub estimated_usd: f64,
    pub reason: &'static str,
}

/// Pre-fetched sell order ready for parallel execution (phase 1 result).
pub struct SellOrderReady {
    pub symbol: String,
    pub mint: String,
    pub raw_amount: String,
    pub display_amount: String,
    pub order: jupiter::OrderResponse,
    pub slippage_pct: Option<f64>,
}

pub struct SwapParamsOwned {
    input_mint: Mint,
    output_mint: Mint,
    raw_amount: String,
    taker: String,
    slippage_bps: Option<u32>,
}

struct SwapParams<'a> {
    input_mint: &'a Mint,
    output_mint: &'a Mint,
    raw_amount: &'a str,
    taker: &'a str,
    slippage_bps: Option<u32>,
}

pub async fn prepare_buy(
    wallet: &wallet::Wallet,
    symbol: &Symbol,
    amount: &str,
    rpc_url: Option<&str>,
    slippage_bps: Option<u32>,
    json: bool,
) -> Result<SwapPlan> {
    let tokens = token_list::get_token_list();
    let (symbol, gm_mint) = resolve_gm_mint(symbol, tokens)?;
    let taker = wallet.pubkey();

    let raw_usdc = amounts::resolve_amount_to_raw(amount, jupiter::USDC_DECIMALS, || {
        let taker = taker.clone();
        let rpc = rpc_url.map(str::to_string);
        async move {
            let (_, raw) = solana::get_usdc_balance_raw(&taker, rpc.as_deref()).await?;
            Ok(raw)
        }
    })
    .await?;
    let usdc_amount = amounts::format_amount(&raw_usdc, jupiter::USDC_DECIMALS);

    let (preflight_res, tradable_res) = tokio::join!(
        preflight_buy_raw(&taker, &raw_usdc, rpc_url),
        check_tradable(&symbol, None),
    );
    preflight_res?;
    tradable_res?;

    let (order, slippage_pct) = get_order_checked(
        jupiter::USDC_MINT,
        &gm_mint,
        &raw_usdc,
        &taker,
        slippage_bps,
        json,
        None,
    )
    .await?;

    Ok(SwapPlan {
        symbol,
        amount: amounts::format_amount(&order.out_amount, jupiter::GM_SOL_DECIMALS),
        counter_amount: usdc_amount,
        order,
        slippage_pct,
        swap: SwapParamsOwned {
            input_mint: Mint::from(jupiter::USDC_MINT),
            output_mint: gm_mint,
            raw_amount: raw_usdc,
            taker,
            slippage_bps,
        },
        output_decimals: jupiter::GM_SOL_DECIMALS,
    })
}

pub async fn prepare_sell(
    wallet: &wallet::Wallet,
    symbol: &Symbol,
    amount: &str,
    rpc_url: Option<&str>,
    slippage_bps: Option<u32>,
    json: bool,
) -> Result<SwapPlan> {
    let tokens = token_list::get_token_list();
    let (symbol, gm_mint) = resolve_gm_mint(symbol, tokens)?;
    let taker = wallet.pubkey();
    let gm_dec = jupiter::GM_SOL_DECIMALS;
    let is_all = amount.trim().eq_ignore_ascii_case("all");
    let is_pct = amount.trim().ends_with('%');

    preflight_sell()?;
    let (tradable_res, bal_res) = tokio::join!(
        check_tradable(&symbol, None),
        solana::get_balance(&taker, &gm_mint, rpc_url),
    );
    tradable_res?;
    let bal = bal_res?;
    if bal.balance <= 0.0 {
        return Err(eyre!("Balance is 0 — nothing to trade"));
    }

    let (sell_amount, raw_gm) = if is_all || is_pct {
        if is_all {
            (
                amounts::format_amount(&bal.raw_amount, gm_dec),
                bal.raw_amount.clone(),
            )
        } else {
            let pct_str = amount
                .trim()
                .strip_suffix('%')
                .ok_or_else(|| eyre!("expected percentage suffix"))?;
            let pct: f64 = pct_str
                .parse()
                .map_err(|_| eyre!("Invalid percentage: {amount}"))?;
            if !(0.0..=100.0).contains(&pct) {
                return Err(eyre!("Percentage must be 0–100, got {pct}"));
            }
            let raw: u128 = bal
                .raw_amount
                .parse()
                .map_err(|_| eyre!("Invalid on-chain amount: {}", bal.raw_amount))?;
            let pct_raw = amounts::pct_of_u128(raw, pct).to_string();
            (amounts::format_amount(&pct_raw, gm_dec), pct_raw)
        }
    } else {
        let raw = amounts::token_to_raw(amount, gm_dec)?;
        let raw_sell: u128 = raw.parse().map_err(|_| eyre!("Invalid amount: {amount}"))?;
        let raw_balance: u128 = bal
            .raw_amount
            .parse()
            .map_err(|_| eyre!("Invalid on-chain amount: {}", bal.raw_amount))?;
        if raw_sell > raw_balance {
            return Err(eyre!(
                "Insufficient {symbol} balance: have {}, trying to sell {}",
                amounts::format_amount(&bal.raw_amount, gm_dec),
                amounts::format_amount(&raw, gm_dec)
            ));
        }
        (amounts::format_amount(&raw, gm_dec), raw)
    };

    let (order, slippage_pct) = get_order_checked(
        &gm_mint,
        jupiter::USDC_MINT,
        &raw_gm,
        &taker,
        slippage_bps,
        json,
        None,
    )
    .await?;

    Ok(SwapPlan {
        symbol,
        amount: sell_amount,
        counter_amount: amounts::format_amount(&order.out_amount, jupiter::USDC_DECIMALS),
        order,
        slippage_pct,
        swap: SwapParamsOwned {
            input_mint: gm_mint,
            output_mint: Mint::from(jupiter::USDC_MINT),
            raw_amount: raw_gm,
            taker,
            slippage_bps,
        },
        output_decimals: jupiter::USDC_DECIMALS,
    })
}

pub async fn execute_swap(wallet: &wallet::Wallet, plan: &SwapPlan, json: bool) -> Result<SwapExecution> {
    let params = SwapParams {
        input_mint: &plan.swap.input_mint,   // &Mint
        output_mint: &plan.swap.output_mint, // &Mint
        raw_amount: &plan.swap.raw_amount,
        taker: &plan.swap.taker,
        slippage_bps: plan.swap.slippage_bps,
    };
    let result = execute_with_retry(wallet, &plan.order, json, &params).await
        .wrap_err("swap execution failed")?;
    let actual_slippage_pct = calc_actual_slippage(&plan.order, &result);
    if let Some(actual) = actual_slippage_pct
        && let Some(quoted) = plan.slippage_pct
    {
        let drift = actual - quoted;
        if drift.abs() > 1.0 && !json {
            eprintln!(
                "Warning: actual slippage {actual:.2}% differs from quoted {quoted:.2}% (drift {drift:+.2}%)"
            );
        }
    }
    Ok(SwapExecution {
        output_amount: result
            .output_amount_result
            .as_deref()
            .map(|r| amounts::format_amount(r, plan.output_decimals))
            .unwrap_or_else(|| amounts::format_amount(&plan.order.out_amount, plan.output_decimals)),
        signature: result.signature.unwrap_or_else(|| "unknown".to_string()),
        actual_slippage_pct,
    })
}

pub async fn execute_sell_raw(
    wallet: &wallet::Wallet,
    mint: &str,
    raw_amount: &str,
    taker: &str,
    json: bool,
) -> Result<SwapExecution> {
    let (order, _) = get_order_checked(
        mint,
        jupiter::USDC_MINT,
        raw_amount,
        taker,
        Some(DEFAULT_SLIPPAGE_BPS),
        json,
        None,
    )
    .await?;
    let input_mint = Mint::from(mint);
    let output_mint = Mint::from(jupiter::USDC_MINT);
    let params = SwapParams {
        input_mint: &input_mint,
        output_mint: &output_mint,
        raw_amount,
        taker,
        slippage_bps: Some(DEFAULT_SLIPPAGE_BPS),
    };
    let result = execute_with_retry(wallet, &order, json, &params).await?;
    let actual_slippage_pct = calc_actual_slippage(&order, &result);
    Ok(SwapExecution {
        output_amount: result
            .output_amount_result
            .as_deref()
            .map(|r| amounts::format_amount(r, jupiter::USDC_DECIMALS))
            .unwrap_or_else(|| amounts::format_amount(&order.out_amount, jupiter::USDC_DECIMALS)),
        signature: result.signature.unwrap_or_else(|| "unknown".to_string()),
        actual_slippage_pct,
    })
}

/// Phase 1 for parallel close-all: fetch + validate a sell order without executing.
pub async fn fetch_sell_order(
    symbol: &str,
    mint: &str,
    raw_amount: &str,
    taker: &str,
    json: bool,
) -> Result<SellOrderReady> {
    let display_amount = amounts::format_amount(raw_amount, jupiter::GM_SOL_DECIMALS);
    let (order, slippage_pct) = get_order_checked(
        mint,
        jupiter::USDC_MINT,
        raw_amount,
        taker,
        Some(DEFAULT_SLIPPAGE_BPS),
        json,
        None,
    )
    .await?;
    Ok(SellOrderReady {
        symbol: symbol.to_string(),
        mint: mint.to_string(),
        raw_amount: raw_amount.to_string(),
        display_amount,
        order,
        slippage_pct,
    })
}

/// Phase 2 for parallel close-all: execute a pre-fetched sell order.
pub async fn execute_sell_from_order(
    wallet: &wallet::Wallet,
    ready: SellOrderReady,
    json: bool,
) -> Result<SwapExecution> {
    let input_mint = Mint::from(ready.mint.as_str());
    let output_mint = Mint::from(jupiter::USDC_MINT);
    // taker is encoded in the signed tx; for RefreshOrder retry we need it separately
    // We grab it from the wallet since SellOrderReady is always for our own wallet.
    let taker = wallet.pubkey();
    let params = SwapParams {
        input_mint: &input_mint,
        output_mint: &output_mint,
        raw_amount: &ready.raw_amount,
        taker: &taker,
        slippage_bps: Some(DEFAULT_SLIPPAGE_BPS),
    };
    let result = execute_with_retry(wallet, &ready.order, json, &params).await?;
    let actual_slippage_pct = calc_actual_slippage(&ready.order, &result);
    Ok(SwapExecution {
        output_amount: result
            .output_amount_result
            .as_deref()
            .map(|r| amounts::format_amount(r, jupiter::USDC_DECIMALS))
            .unwrap_or_else(|| amounts::format_amount(&ready.order.out_amount, jupiter::USDC_DECIMALS)),
        signature: result.signature.unwrap_or_else(|| "unknown".to_string()),
        actual_slippage_pct,
    })
}

/// High-level phase 1 for sell-basket: resolve symbol, compute raw amount from user input
/// (exact token amount, "50%", or "all"), check tradable, fetch + validate sell order.
pub async fn fetch_sell_order_by_symbol(
    symbol_str: &str,
    amount_str: &str,
    taker: &str,
    json: bool,
    rpc_url: Option<&str>,
) -> Result<SellOrderReady> {
    let tokens = token_list::get_token_list();
    let sym = Symbol::from(symbol_str);
    let (sym, gm_mint) = resolve_gm_mint(&sym, tokens)?;
    check_tradable(&sym, None).await?;

    let gm_dec = jupiter::GM_SOL_DECIMALS;
    let bal = solana::get_balance(taker, &gm_mint, rpc_url).await
        .wrap_err_with(|| format!("failed to fetch {sym} balance"))?;
    if bal.balance <= 0.0 {
        return Err(eyre!("Balance is 0 for {sym} — nothing to sell"));
    }

    let is_all = amount_str.trim().eq_ignore_ascii_case("all");
    let is_pct = amount_str.trim().ends_with('%');
    let (display_amount, raw_amount) = if is_all {
        (
            amounts::format_amount(&bal.raw_amount, gm_dec),
            bal.raw_amount.clone(),
        )
    } else if is_pct {
        let pct_str = amount_str.trim().strip_suffix('%').unwrap_or("0");
        let pct: f64 = pct_str
            .parse()
            .map_err(|_| eyre!("Invalid percentage: {amount_str}"))?;
        if !(0.0..=100.0).contains(&pct) {
            return Err(eyre!("Percentage must be 0–100, got {amount_str}"));
        }
        let raw: u128 = bal.raw_amount.parse().map_err(|_| eyre!("Invalid balance"))?;
        let scaled = ((raw as f64) * pct / 100.0).round() as u128;
        (
            amounts::format_amount(&scaled.to_string(), gm_dec),
            scaled.to_string(),
        )
    } else {
        let raw = amounts::token_to_raw(amount_str, gm_dec)
            .map_err(|e| eyre!("Invalid amount '{amount_str}' for {sym}: {e}"))?;
        (amounts::format_amount(&raw, gm_dec), raw)
    };

    let mint_str = gm_mint.to_string();
    fetch_sell_order(&sym, &mint_str, &raw_amount, taker, json).await.map(|mut r| {
        r.display_amount = display_amount;
        r
    })
}

/// Pre-fetched buy order ready for parallel/sequential basket execution.
pub struct BuyOrderReady {
    pub symbol: String,
    pub gm_mint: Mint,
    pub usdc_raw: String,
    pub usdc_display: String,
    pub order: jupiter::OrderResponse,
    pub slippage_pct: Option<f64>,
}

/// Preflight for basket buy: verify trading is open, total USDC coverage, and SOL for fees.
pub async fn preflight_basket_buy(
    pubkey: &str,
    total_usdc_raw: u128,
    rpc_url: Option<&str>,
) -> Result<()> {
    check_trading_hours()?;
    let (usdc_res, sol_raw_res) = tokio::join!(
        solana::get_usdc_balance_raw(pubkey, rpc_url),
        solana::get_sol_balance_raw(pubkey, rpc_url),
    );
    let (_, balance_raw) = usdc_res?;
    let balance: u128 = balance_raw
        .parse()
        .map_err(|_| eyre!("Invalid on-chain USDC amount: {balance_raw}"))?;
    if balance < total_usdc_raw {
        return Err(eyre!(
            "Insufficient USDC: {:.2} available, need {:.2} total\n  Fund wallet: {pubkey}",
            balance as f64 / 10f64.powi(jupiter::USDC_DECIMALS as i32),
            total_usdc_raw as f64 / 10f64.powi(jupiter::USDC_DECIMALS as i32),
        ));
    }
    let sol_raw = sol_raw_res?;
    let sol_lamports: u64 = sol_raw
        .parse()
        .map_err(|_| eyre!("Invalid on-chain SOL amount: {sol_raw}"))?;
    let sol = sol_lamports as f64 / 1_000_000_000.0;
    if sol < MIN_SOL_FOR_FEES {
        return Err(eyre!(
            "Insufficient SOL for fees: have {sol:.6} SOL, need ~{MIN_SOL_FOR_FEES} SOL.\n  Fund wallet: {pubkey}"
        ));
    }
    Ok(())
}

/// Phase 1 for basket buy: resolve mint, check tradable, fetch + validate buy order.
pub async fn fetch_buy_order(
    symbol: &str,
    usdc_raw: &str,
    taker: &str,
    json: bool,
) -> Result<BuyOrderReady> {
    let tokens = token_list::get_token_list();
    let sym = Symbol::from(symbol);
    let (sym, gm_mint) = resolve_gm_mint(&sym, tokens)?;
    check_tradable(&sym, None).await?;
    let usdc_display = amounts::format_amount(usdc_raw, jupiter::USDC_DECIMALS);
    let (order, slippage_pct) = get_order_checked(
        jupiter::USDC_MINT,
        &gm_mint,
        usdc_raw,
        taker,
        Some(DEFAULT_SLIPPAGE_BPS),
        json,
        None,
    )
    .await?;
    Ok(BuyOrderReady {
        symbol: sym.to_string(),
        gm_mint,
        usdc_raw: usdc_raw.to_string(),
        usdc_display,
        order,
        slippage_pct,
    })
}

/// Phase 2 for basket buy: execute a pre-fetched buy order.
pub async fn execute_buy_from_order(
    wallet: &wallet::Wallet,
    ready: BuyOrderReady,
    json: bool,
) -> Result<SwapExecution> {
    let input_mint = Mint::from(jupiter::USDC_MINT);
    let output_mint = ready.gm_mint;
    let taker = wallet.pubkey();
    let params = SwapParams {
        input_mint: &input_mint,
        output_mint: &output_mint,
        raw_amount: &ready.usdc_raw,
        taker: &taker,
        slippage_bps: Some(DEFAULT_SLIPPAGE_BPS),
    };
    let result = execute_with_retry(wallet, &ready.order, json, &params).await?;
    let actual_slippage_pct = calc_actual_slippage(&ready.order, &result);
    Ok(SwapExecution {
        output_amount: result
            .output_amount_result
            .as_deref()
            .map(|r| amounts::format_amount(r, jupiter::GM_SOL_DECIMALS))
            .unwrap_or_else(|| {
                amounts::format_amount(&ready.order.out_amount, jupiter::GM_SOL_DECIMALS)
            }),
        signature: result.signature.unwrap_or_else(|| "unknown".to_string()),
        actual_slippage_pct,
    })
}

pub fn parse_sell_pct(amount: Option<&str>) -> Result<f64> {
    let Some(raw) = amount else { return Ok(100.0); };
    let s = raw.trim();
    let pct_str = s
        .strip_suffix('%')
        .ok_or_else(|| eyre!("close-all amount must be a percentage (e.g. 10%, 50%)"))?;
    let pct: f64 = pct_str.parse().map_err(|_| eyre!("Invalid percentage: {s}"))?;
    if !(0.0..=100.0).contains(&pct) {
        return Err(eyre!("Percentage must be 0–100, got {pct}"));
    }
    Ok(pct)
}

pub fn should_skip_position(
    symbol: &str,
    est_value: f64,
    tradable_set: &std::collections::HashSet<String>,
) -> Option<CloseSkip> {
    if est_value > 0.0 && est_value < MIN_SELL_VALUE_USD {
        return Some(CloseSkip {
            token: symbol.to_string(),
            estimated_usd: est_value,
            reason: "below $1.50 minimum",
        });
    }
    if !tradable_set.is_empty() && !tradable_set.contains(&symbol.to_uppercase()) {
        return Some(CloseSkip {
            token: symbol.to_string(),
            estimated_usd: est_value,
            reason: "not tradable in current session",
        });
    }
    None
}

pub async fn fetch_tradable_set(api_url: Option<&str>) -> std::collections::HashSet<String> {
    let session = api::current_session();
    if session == api::Session::Closed {
        return std::collections::HashSet::new();
    }
    api::fetch_session_limits(api_url)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|l| l.is_tradable(session))
        .map(|l| l.symbol.to_uppercase())
        .collect()
}

pub fn ensure_trading_open() -> Result<()> {
    check_trading_hours()
}

fn resolve_gm_mint(symbol: &Symbol, tokens: &[token_list::GmTokenEntry]) -> Result<(Symbol, Mint)> {
    let entry = gm::resolve_token(symbol, tokens)?;
    let mint = entry
        .solana_address
        .ok_or_else(|| eyre!("No Solana address for {}", entry.symbol))?;
    Ok((Symbol::from(entry.symbol), Mint::from(mint)))
}

/// Compute `value * pct / 100` using integer math to avoid f64 precision loss.
fn calc_slippage(order: &jupiter::OrderResponse) -> Option<f64> {
    if let Some(pi) = order.price_impact {
        return Some(pi);
    }
    match (order.in_usd_value, order.out_usd_value) {
        (Some(usd_in), Some(usd_out)) if usd_in > 0.0 => Some((usd_out - usd_in) / usd_in * 100.0),
        _ => None,
    }
}

fn slippage_block_hint(s: f64, order: &jupiter::OrderResponse) -> String {
    let router = order.router.as_deref().unwrap_or("unknown");
    format!(
        "slippage {s:.2}% via {router} exceeds -{MAX_SLIPPAGE_PCT:.0}% after {MAX_SLIPPAGE_RETRIES} retries. \
         Try a larger amount or wait for better liquidity."
    )
}

fn check_slippage(order: &jupiter::OrderResponse, json: bool) -> Result<Option<f64>> {
    let slip = calc_slippage(order);
    if let Some(s) = slip {
        if s < -MAX_SLIPPAGE_PCT {
            return Err(GmTradeError::new(
                GmTradeErrorKind::SlippageTooHigh,
                slippage_block_hint(s, order),
            )
            .into());
        }
        if s < -SLIPPAGE_RETRY_PCT && !json {
            eprintln!("Warning: slippage {s:.2}%");
        }
    }
    Ok(slip)
}

/// Compute actual slippage from execute response amounts vs order quote.
/// Returns `None` if the execute response omits the result amounts.
fn calc_actual_slippage(
    order: &jupiter::OrderResponse,
    exec: &jupiter::ExecuteResponse,
) -> Option<f64> {
    let actual_in: u128 = exec.input_amount_result.as_deref()?.parse().ok()?;
    let actual_out: u128 = exec.output_amount_result.as_deref()?.parse().ok()?;
    let quoted_in: u128 = order.in_amount.parse().ok()?;
    let quoted_out: u128 = order.out_amount.parse().ok()?;
    if quoted_in == 0 || quoted_out == 0 {
        return None;
    }
    // actual_ratio = actual_out / actual_in
    // quoted_ratio = quoted_out / quoted_in
    // slippage = (actual_ratio / quoted_ratio - 1) * 100
    let actual_ratio = actual_out as f64 / actual_in as f64;
    let quoted_ratio = quoted_out as f64 / quoted_in as f64;
    Some((actual_ratio / quoted_ratio - 1.0) * 100.0)
}

async fn get_order_checked(
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
    json: bool,
    jupiter_url: Option<&str>,
) -> Result<(jupiter::OrderResponse, Option<f64>)> {
    let mut order = jupiter::get_order(jupiter_url, input_mint, output_mint, amount, taker, slippage_bps).await
        .wrap_err("failed to get Jupiter quote")?;
    for attempt in 1..=MAX_SLIPPAGE_RETRIES {
        let slip = calc_slippage(&order);
        if let Some(s) = slip
            && s < -SLIPPAGE_RETRY_PCT
        {
            if !json {
                eprintln!(
                    "Slippage {s:.2}% exceeds -{SLIPPAGE_RETRY_PCT:.0}% — refreshing quote ({attempt}/{MAX_SLIPPAGE_RETRIES})..."
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            order = jupiter::get_order(jupiter_url, input_mint, output_mint, amount, taker, slippage_bps).await
                .wrap_err("failed to refresh Jupiter quote")?;
            continue;
        }
        return Ok((order, slip));
    }
    // All retries exhausted — apply hard block if still above safety-net threshold.
    let slippage_pct = check_slippage(&order, json)?;
    Ok((order, slippage_pct))
}

fn check_trading_hours() -> Result<()> {
    let session = api::current_session();
    if session == api::Session::Closed {
        use chrono_tz::US::Eastern;
        let now = chrono::Utc::now().with_timezone(&Eastern);
        return Err(GmTradeError::new(
            GmTradeErrorKind::MarketClosed,
            format!(
                "trading resumes Sunday 8:00 PM ET (current time: {} ET). Run `rwa gm hours` for session details",
                now.format("%A %I:%M %p")
            ),
        )
        .into());
    }
    Ok(())
}

async fn check_tradable(symbol: &str, api_url: Option<&str>) -> Result<()> {
    let session = api::current_session();
    if session == api::Session::Closed {
        return Ok(());
    }
    let limits = match api::fetch_session_limits(api_url).await {
        Ok(l) => l,
        Err(_) => return Ok(()),
    };
    let sym_upper = symbol.to_uppercase();
    let limit = limits
        .iter()
        .find(|l| l.symbol.to_uppercase() == sym_upper);

    let is_tradable = limit.map(|l| l.is_tradable(session)).unwrap_or(false);
    if !is_tradable {
        return Err(GmTradeError::new(
            GmTradeErrorKind::NotTradable,
            format!(
                "{symbol} is not tradable in current session ({}). Run `rwa gm hours --tradable` to see which tokens are available.",
                session.label()
            ),
        )
        .into());
    }

    // If max notional is explicitly 0 for this session, the token is marked tradable
    // but has no active liquidity — skip before even calling Jupiter.
    if let Some(max) = limit.and_then(|l| l.max_notional(session)) && max <= 0.0 {
        return Err(GmTradeError::new(
            GmTradeErrorKind::NotTradable,
            format!(
                "{symbol} has no active notional limit for the current session ({}) — likely illiquid. Try during Regular Market hours (9:30 AM – 4 PM ET).",
                session.label()
            ),
        )
        .into());
    }

    Ok(())
}

async fn preflight_buy_raw(pubkey: &str, raw_usdc_amount: &str, rpc_url: Option<&str>) -> Result<()> {
    check_trading_hours()?;
    let requested: u128 = raw_usdc_amount.parse().map_err(|_| eyre!("Invalid USDC amount: {raw_usdc_amount}"))?;
    let minimum = 10u128.pow(jupiter::USDC_DECIMALS as u32) * MIN_USDC_AMOUNT as u128;
    if requested < minimum {
        return Err(eyre!("Minimum buy amount is {MIN_USDC_AMOUNT} USDC"));
    }

    let (usdc_res, sol_raw_res) = tokio::join!(
        solana::get_usdc_balance_raw(pubkey, rpc_url),
        solana::get_sol_balance_raw(pubkey, rpc_url),
    );

    let (_, balance_raw) = usdc_res?;
    let balance: u128 = balance_raw.parse().map_err(|_| eyre!("Invalid on-chain USDC amount: {balance_raw}"))?;
    if balance < requested {
        return Err(eyre!(
            "Insufficient USDC: {:.6} USDC (need {:.6})\n  Fund wallet: {pubkey}",
            balance as f64 / 10f64.powi(jupiter::USDC_DECIMALS as i32),
            requested as f64 / 10f64.powi(jupiter::USDC_DECIMALS as i32)
        ));
    }

    let sol_raw = sol_raw_res?;
    let sol_lamports: u64 = sol_raw.parse().map_err(|_| eyre!("Invalid on-chain SOL amount: {sol_raw}"))?;
    let sol = sol_lamports as f64 / 1_000_000_000.0;
    if sol < MIN_SOL_FOR_FEES {
        return Err(eyre!(
            "Insufficient SOL for transaction fees: have {sol:.6} SOL, need ~{MIN_SOL_FOR_FEES} SOL.\n  Fund wallet: {pubkey}"
        ));
    }

    Ok(())
}

fn preflight_sell() -> Result<()> {
    check_trading_hours()
}

async fn execute_with_retry(
    wallet: &wallet::Wallet,
    order: &jupiter::OrderResponse,
    json: bool,
    params: &SwapParams<'_>,
) -> Result<jupiter::ExecuteResponse> {
    let mut current_order_owned: Option<jupiter::OrderResponse> = None;
    let mut last_err = None;

    for attempt in 0..=MAX_SWAP_RETRIES {
        let ord = current_order_owned.as_ref().unwrap_or(order);
        let execute_result = jupiter::execute_order(wallet, ord).await;
        match execute_result {
            Ok(resp) => return Ok(resp),
            Err(e) => {
                let retry_action = e
                    .downcast_ref::<jupiter::ExecuteFailure>()
                    .map(|failure| failure.kind.retry_action())
                    .unwrap_or(jupiter::ExecuteRetryAction::None);

                if retry_action == jupiter::ExecuteRetryAction::None || attempt == MAX_SWAP_RETRIES {
                    return Err(e);
                }
                if !json {
                    eprintln!(
                        "Transient error (attempt {}/{}): {e}, retrying in 3s...",
                        attempt + 1,
                        MAX_SWAP_RETRIES
                    );
                }
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                if retry_action == jupiter::ExecuteRetryAction::RefreshOrder {
                    current_order_owned = Some(
                        jupiter::get_order(
                            None,
                            params.input_mint,
                            params.output_mint,
                            params.raw_amount,
                            params.taker,
                            params.slippage_bps,
                        )
                        .await
                        .wrap_err("failed to refresh quote after transient error")?,
                    );
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| eyre!("Swap failed after retries")))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn check_slippage_blocks_above_max() {
        let order = jupiter::OrderResponse {
            in_amount: "100".into(),
            out_amount: "88".into(),
            in_usd_value: Some(100.0),
            out_usd_value: Some(88.0),
            ..Default::default()
        };
        let result = check_slippage(&order, true);
        assert!(result.is_err());
        let err = result.unwrap_err();
        let typed = err.downcast_ref::<GmTradeError>().expect("typed slippage error");
        assert_eq!(typed.kind, GmTradeErrorKind::SlippageTooHigh);
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
        let result = check_slippage(&order, true);
        assert!(result.is_ok());
    }

    #[test]
    fn market_closed_error_is_typed() {
        let err = GmTradeError::new(GmTradeErrorKind::MarketClosed, "closed");
        assert_eq!(err.kind, GmTradeErrorKind::MarketClosed);
        assert!(err.to_string().contains("market_closed"));
    }

    #[test]
    fn min_sol_for_fees_constant_is_reasonable() {
        // 0.002 SOL = 2_000_000 lamports — covers rent-exempt + typical priority fee
        let lamports = (MIN_SOL_FOR_FEES * 1_000_000_000.0) as u64;
        assert_eq!(lamports, 2_000_000);
        assert!(lamports > 0);
    }

    #[test]
    fn execute_failure_display_includes_error_text() {
        use crate::jupiter::{ExecuteFailure, ExecuteFailureKind};
        let err = ExecuteFailure {
            kind: ExecuteFailureKind::FailedToLand,
            code: Some(-1000),
            message: "landing failure".to_string(),
        };
        let msg = err.to_string();
        // Verify the error Display includes enough context for a retry log line.
        assert!(msg.contains("landing failure"));
        assert!(msg.contains("-1000"));
    }

    // ── parse_sell_pct ────────────────────────────────────────

    #[test]
    fn parse_sell_pct_none_returns_100() {
        assert_eq!(parse_sell_pct(None).unwrap(), 100.0);
    }

    #[test]
    fn parse_sell_pct_full_percent() {
        assert_eq!(parse_sell_pct(Some("100%")).unwrap(), 100.0);
    }

    #[test]
    fn parse_sell_pct_half_percent() {
        assert_eq!(parse_sell_pct(Some("50%")).unwrap(), 50.0);
    }

    #[test]
    fn parse_sell_pct_zero_percent() {
        assert_eq!(parse_sell_pct(Some("0%")).unwrap(), 0.0);
    }

    #[test]
    fn parse_sell_pct_decimal_percent() {
        assert_eq!(parse_sell_pct(Some("1.5%")).unwrap(), 1.5);
    }

    #[test]
    fn parse_sell_pct_over_100_is_err() {
        assert!(parse_sell_pct(Some("101%")).is_err());
    }

    #[test]
    fn parse_sell_pct_negative_is_err() {
        assert!(parse_sell_pct(Some("-1%")).is_err());
    }

    #[test]
    fn parse_sell_pct_missing_percent_suffix_is_err() {
        assert!(parse_sell_pct(Some("50")).is_err());
    }

    #[test]
    fn parse_sell_pct_non_numeric_is_err() {
        assert!(parse_sell_pct(Some("abc%")).is_err());
    }

    // ── should_skip_position ──────────────────────────────────

    #[test]
    fn should_skip_zero_value_never_blocks() {
        // est_value == 0.0 does NOT trigger the minimum check (condition is `> 0.0 && < MIN`)
        let tradable: std::collections::HashSet<String> = ["TSLAON".to_string()].into();
        assert!(should_skip_position("TSLAon", 0.0, &tradable).is_none());
    }

    #[test]
    fn should_skip_below_minimum_returns_skip() {
        let tradable: std::collections::HashSet<String> = ["TSLAON".to_string()].into();
        let skip = should_skip_position("TSLAon", 1.2, &tradable).unwrap();
        assert!(skip.reason.contains("below"));
    }

    #[test]
    fn should_skip_at_minimum_boundary_does_not_skip() {
        // 1.5 is exactly MIN_SELL_VALUE_USD — the condition is `< MIN`, not `<=`
        let tradable: std::collections::HashSet<String> = ["TSLAON".to_string()].into();
        assert!(should_skip_position("TSLAon", MIN_SELL_VALUE_USD, &tradable).is_none());
    }

    #[test]
    fn should_skip_empty_tradable_set_does_not_skip() {
        // Empty set = market closed, tradability check is bypassed entirely
        let tradable: std::collections::HashSet<String> = std::collections::HashSet::new();
        assert!(should_skip_position("TSLAon", 2.0, &tradable).is_none());
    }

    #[test]
    fn should_skip_symbol_not_in_tradable_set() {
        let tradable: std::collections::HashSet<String> = ["AAPLON".to_string()].into();
        let skip = should_skip_position("TSLAon", 2.0, &tradable).unwrap();
        assert!(skip.reason.contains("not tradable"));
    }

    #[test]
    fn should_skip_symbol_in_tradable_set_does_not_skip() {
        let tradable: std::collections::HashSet<String> = ["TSLAON".to_string()].into();
        assert!(should_skip_position("TSLAon", 2.0, &tradable).is_none());
    }

    // ── SessionLimits max_notional zero check ─────────────────

    #[test]
    fn session_limits_zero_notional_is_not_tradable_for_liquidity() {
        use crate::api::{Session, SessionInfo, SessionLimits};
        let limits = SessionLimits {
            symbol: "AMGNon".to_string(),
            premarket: None,
            regular: None,
            postmarket: None,
            overnight: Some(SessionInfo {
                tradable: true, // marked tradable, but notional = 0
                max_attestation_count: None,
                max_active_notional_value: Some("0".to_string()),
            }),
        };
        // is_tradable returns true (Ondo says yes)...
        assert!(limits.is_tradable(Session::Overnight));
        // ...but max_notional is 0.0, so our check would catch it
        let max = limits.max_notional(Session::Overnight);
        assert_eq!(max, Some(0.0));
    }

    #[test]
    fn session_limits_normal_notional_passes_liquidity_check() {
        use crate::api::{Session, SessionInfo, SessionLimits};
        let limits = SessionLimits {
            symbol: "LLYon".to_string(),
            premarket: None,
            regular: None,
            postmarket: None,
            overnight: Some(SessionInfo {
                tradable: true,
                max_attestation_count: None,
                max_active_notional_value: Some("100000".to_string()),
            }),
        };
        let max = limits.max_notional(Session::Overnight);
        assert!(max.unwrap_or(0.0) > 0.0);
    }

    // ── calc_actual_slippage ──────────────────────────────────

    #[test]
    fn calc_actual_slippage_zero_when_exact_match() {
        let order = jupiter::OrderResponse {
            in_amount: "1000".into(),
            out_amount: "2000".into(),
            ..Default::default()
        };
        let exec = jupiter::ExecuteResponse {
            input_amount_result: Some("1000".into()),
            output_amount_result: Some("2000".into()),
            ..Default::default()
        };
        let slip = calc_actual_slippage(&order, &exec);
        assert!(slip.is_some());
        assert!((slip.unwrap()).abs() < 0.001);
    }

    #[test]
    fn calc_actual_slippage_negative_when_got_less() {
        let order = jupiter::OrderResponse {
            in_amount: "1000".into(),
            out_amount: "1000".into(),
            ..Default::default()
        };
        // Got 990 out instead of 1000 → -1% slippage
        let exec = jupiter::ExecuteResponse {
            input_amount_result: Some("1000".into()),
            output_amount_result: Some("990".into()),
            ..Default::default()
        };
        let slip = calc_actual_slippage(&order, &exec).unwrap();
        assert!((slip - (-1.0)).abs() < 0.01, "got {slip}");
    }

    #[test]
    fn calc_actual_slippage_none_when_exec_missing_amounts() {
        let order = jupiter::OrderResponse {
            in_amount: "1000".into(),
            out_amount: "1000".into(),
            ..Default::default()
        };
        let exec = jupiter::ExecuteResponse {
            input_amount_result: None,
            output_amount_result: None,
            ..Default::default()
        };
        assert!(calc_actual_slippage(&order, &exec).is_none());
    }

    #[test]
    fn calc_actual_slippage_none_when_zero_quoted_in() {
        let order = jupiter::OrderResponse {
            in_amount: "0".into(),
            out_amount: "1000".into(),
            ..Default::default()
        };
        let exec = jupiter::ExecuteResponse {
            input_amount_result: Some("0".into()),
            output_amount_result: Some("1000".into()),
            ..Default::default()
        };
        assert!(calc_actual_slippage(&order, &exec).is_none());
    }

    // ── slippage_block_hint ───────────────────────────────────

    #[test]
    fn slippage_block_hint_includes_router() {
        let order = jupiter::OrderResponse {
            router: Some("jupiterz".into()),
            ..Default::default()
        };
        let hint = slippage_block_hint(-12.0, &order);
        assert!(hint.contains("jupiterz"), "hint: {hint}");
        assert!(hint.contains("-12.00%"), "hint: {hint}");
        assert!(hint.contains("retries"), "hint: {hint}");
    }

    #[test]
    fn slippage_block_hint_liquidity_message() {
        let order = jupiter::OrderResponse {
            router: Some("jupiter".into()),
            ..Default::default()
        };
        let hint = slippage_block_hint(-11.0, &order);
        assert!(hint.contains("liquidity") || hint.contains("larger"), "hint: {hint}");
    }

    // ── parse_sell_pct edge cases ─────────────────────────────

    #[test]
    fn parse_sell_pct_fractional_near_zero() {
        let pct = parse_sell_pct(Some("0.01%")).unwrap();
        assert!((pct - 0.01).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_sell_pct_exactly_100_percent() {
        let pct = parse_sell_pct(Some("100%")).unwrap();
        assert_eq!(pct, 100.0);
    }
}
