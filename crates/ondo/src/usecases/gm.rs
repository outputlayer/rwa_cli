use eyre::{Result, eyre};

use crate::{amounts, api, gm, jupiter, solana, token_list, wallet};

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

/// Maximum allowed slippage before blocking the trade.
const MAX_SLIPPAGE_PCT: f64 = 3.0;
/// Slippage threshold that triggers a fresh quote retry.
const SLIPPAGE_RETRY_PCT: f64 = 1.0;
/// Maximum retries when slippage exceeds the retry threshold.
const MAX_SLIPPAGE_RETRIES: u32 = 3;
/// Maximum retries for transient swap execution failures.
const MAX_SWAP_RETRIES: u32 = 2;
/// Default slippage limit in basis points (100 = 1%).
pub const DEFAULT_SLIPPAGE_BPS: u32 = 100;
/// Minimum buy/sell amount in USDC.
const MIN_USDC_AMOUNT: f64 = 1.0;
/// Minimum sell value in USD (Jupiter MM rejects tiny orders).
pub const MIN_SELL_VALUE_USD: f64 = 1.5;

pub struct SwapPlan {
    pub symbol: String,
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
}

pub struct CloseSkip {
    pub token: String,
    pub estimated_usd: f64,
    pub reason: &'static str,
}

pub struct SwapParamsOwned {
    input_mint: String,
    output_mint: String,
    raw_amount: String,
    taker: String,
    slippage_bps: Option<u32>,
}

struct SwapParams<'a> {
    input_mint: &'a str,
    output_mint: &'a str,
    raw_amount: &'a str,
    taker: &'a str,
    slippage_bps: Option<u32>,
}

pub async fn prepare_buy(
    wallet: &wallet::Wallet,
    symbol: &str,
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
        check_tradable(&symbol),
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
    )
    .await?;

    Ok(SwapPlan {
        symbol,
        amount: amounts::format_amount(&order.out_amount, jupiter::GM_SOL_DECIMALS),
        counter_amount: usdc_amount,
        order,
        slippage_pct,
        swap: SwapParamsOwned {
            input_mint: jupiter::USDC_MINT.to_string(),
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
    symbol: &str,
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
        check_tradable(&symbol),
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
            output_mint: jupiter::USDC_MINT.to_string(),
            raw_amount: raw_gm,
            taker,
            slippage_bps,
        },
        output_decimals: jupiter::USDC_DECIMALS,
    })
}

pub async fn execute_swap(wallet: &wallet::Wallet, plan: &SwapPlan, json: bool) -> Result<SwapExecution> {
    let params = SwapParams {
        input_mint: &plan.swap.input_mint,
        output_mint: &plan.swap.output_mint,
        raw_amount: &plan.swap.raw_amount,
        taker: &plan.swap.taker,
        slippage_bps: plan.swap.slippage_bps,
    };
    let result = execute_with_retry(wallet, &plan.order, json, &params).await?;
    Ok(SwapExecution {
        output_amount: result
            .output_amount_result
            .as_deref()
            .map(|r| amounts::format_amount(r, plan.output_decimals))
            .unwrap_or_else(|| amounts::format_amount(&plan.order.out_amount, plan.output_decimals)),
        signature: result.signature.unwrap_or_else(|| "unknown".to_string()),
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
    )
    .await?;
    let params = SwapParams {
        input_mint: mint,
        output_mint: jupiter::USDC_MINT,
        raw_amount,
        taker,
        slippage_bps: Some(DEFAULT_SLIPPAGE_BPS),
    };
    let result = execute_with_retry(wallet, &order, json, &params).await?;
    Ok(SwapExecution {
        output_amount: result
            .output_amount_result
            .as_deref()
            .map(|r| amounts::format_amount(r, jupiter::USDC_DECIMALS))
            .unwrap_or_else(|| amounts::format_amount(&order.out_amount, jupiter::USDC_DECIMALS)),
        signature: result.signature.unwrap_or_else(|| "unknown".to_string()),
    })
}

pub fn parse_sell_pct(amount: Option<&str>) -> Result<f64> {
    let Some(s) = amount else { return Ok(100.0) };
    let s = s.trim();
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

pub async fn fetch_tradable_set() -> std::collections::HashSet<String> {
    let session = api::current_session();
    if session == api::Session::Closed {
        return std::collections::HashSet::new();
    }
    api::fetch_session_limits()
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

fn resolve_gm_mint(symbol: &str, tokens: &[token_list::GmTokenEntry]) -> Result<(String, String)> {
    let entry = gm::resolve_token(symbol, tokens)?;
    let mint = entry
        .solana_address
        .ok_or_else(|| eyre!("No Solana address for {}", entry.symbol))?;
    Ok((entry.symbol.to_string(), mint.to_string()))
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

fn check_slippage(order: &jupiter::OrderResponse, json: bool) -> Result<Option<f64>> {
    let slip = calc_slippage(order);
    if let Some(s) = slip {
        if s < -MAX_SLIPPAGE_PCT {
            return Err(GmTradeError::new(
                GmTradeErrorKind::SlippageTooHigh,
                format!(
                    "slippage {s:.2}% exceeds -{MAX_SLIPPAGE_PCT:.0}%. Try a smaller amount or wait for better liquidity."
                ),
            )
            .into());
        }
        if s < -1.0 && !json {
            eprintln!("Warning: slippage {s:.2}%");
        }
    }
    Ok(slip)
}

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
                return Err(GmTradeError::new(
                    GmTradeErrorKind::SlippageTooHigh,
                    format!(
                        "slippage {s:.2}% exceeds -{MAX_SLIPPAGE_PCT:.0}%. Try a smaller amount or wait for better liquidity."
                    ),
                )
                .into());
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
        let slippage_pct = slip;
        if let Some(s) = slippage_pct {
            if s < -1.0 && !json {
                eprintln!("Warning: slippage {s:.2}%");
            }
        }
        return Ok((order, slippage_pct));
    }
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

async fn check_tradable(symbol: &str) -> Result<()> {
    let session = api::current_session();
    if session == api::Session::Closed {
        return Ok(());
    }
    let limits = match api::fetch_session_limits().await {
        Ok(l) => l,
        Err(_) => return Ok(()),
    };
    let sym_upper = symbol.to_uppercase();
    let is_tradable = limits
        .iter()
        .any(|l| l.symbol.to_uppercase() == sym_upper && l.is_tradable(session));
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
    Ok(())
}

async fn preflight_buy_raw(pubkey: &str, raw_usdc_amount: &str, rpc_url: Option<&str>) -> Result<()> {
    check_trading_hours()?;
    let requested: u128 = raw_usdc_amount.parse().map_err(|_| eyre!("Invalid USDC amount: {raw_usdc_amount}"))?;
    let minimum = 10u128.pow(jupiter::USDC_DECIMALS as u32) * MIN_USDC_AMOUNT as u128;
    if requested < minimum {
        return Err(eyre!("Minimum buy amount is {MIN_USDC_AMOUNT} USDC"));
    }

    let (_, balance_raw) = solana::get_usdc_balance_raw(pubkey, rpc_url).await?;
    let balance: u128 = balance_raw.parse().map_err(|_| eyre!("Invalid on-chain USDC amount: {balance_raw}"))?;
    if balance < requested {
        return Err(eyre!(
            "Insufficient USDC: {:.6} USDC (need {:.6})\n  Fund wallet: {pubkey}",
            balance as f64 / 10f64.powi(jupiter::USDC_DECIMALS as i32),
            requested as f64 / 10f64.powi(jupiter::USDC_DECIMALS as i32)
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
                        "Transient error (attempt {}/{}), retrying in 3s...",
                        attempt + 1,
                        MAX_SWAP_RETRIES
                    );
                }
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                if retry_action == jupiter::ExecuteRetryAction::RefreshOrder {
                    current_order_owned = Some(
                        jupiter::get_order(
                            params.input_mint,
                            params.output_mint,
                            params.raw_amount,
                            params.taker,
                            params.slippage_bps,
                        )
                        .await?,
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
            out_amount: "96".into(),
            in_usd_value: Some(100.0),
            out_usd_value: Some(96.0),
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
}
