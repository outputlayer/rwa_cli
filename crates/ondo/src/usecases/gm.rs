use eyre::{Result, WrapErr, eyre};

use crate::{amounts, api, jupiter, solana, token_list, wallet};
use crate::types::{Mint, Symbol};

// Re-export from sibling modules so callers can continue using `usecases::gm::*`.
pub use super::gm_execute::{execute_sell_from_order, execute_buy_from_order};
pub use super::gm_order::{fetch_sell_order, fetch_sell_order_by_symbol, fetch_buy_order, preflight_basket_buy};

use super::gm_internal::{
    resolve_gm_mint, check_tradable, preflight_buy_raw, preflight_sell,
    get_order_checked,
};
use super::gm_execute::{calc_actual_slippage, execute_with_retry, SwapParams};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GmTradeErrorKind {
    MarketClosed,
    NotTradable,
    SlippageTooHigh,
    CostTooHigh,
}

#[derive(Debug)]
pub struct GmTradeError {
    pub kind: GmTradeErrorKind,
    pub detail: String,
}

impl GmTradeError {
    pub(crate) fn new(kind: GmTradeErrorKind, detail: impl Into<String>) -> Self {
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
            Self::CostTooHigh => "cost_too_high",
        };
        f.write_str(label)
    }
}

impl std::error::Error for GmTradeError {}

/// Classify an eyre error as a stable error kind label, suitable for JSON
/// `error_kind` fields consumed by agents/scripts. Returns `None` for opaque
/// errors that aren't one of the known structured types.
///
/// Currently recognizes `GmTradeError` and Jupiter `ExecuteFailure`. RPC-layer
/// failures bubble up as opaque `eyre` errors today and return `None`.
#[must_use]
pub fn classify_error(err: &eyre::Error) -> Option<&'static str> {
    for cause in err.chain() {
        if let Some(g) = cause.downcast_ref::<GmTradeError>() {
            return Some(match g.kind {
                GmTradeErrorKind::MarketClosed => "market_closed",
                GmTradeErrorKind::NotTradable => "not_tradable",
                GmTradeErrorKind::SlippageTooHigh => "slippage_too_high",
                GmTradeErrorKind::CostTooHigh => "cost_too_high",
            });
        }
        if let Some(f) = cause.downcast_ref::<jupiter::ExecuteFailure>() {
            return Some(f.kind.label());
        }
    }
    None
}

/// Default slippage limit in basis points (100 = 1%).
pub const DEFAULT_SLIPPAGE_BPS: u32 = 100;
/// Minimum sell value in USD (Jupiter MM rejects tiny orders).
pub const MIN_SELL_VALUE_USD: f64 = 1.5;

pub struct SwapPlan {
    pub symbol: Symbol,
    pub amount: String,
    pub counter_amount: String,
    pub order: jupiter::OrderResponse,
    pub slippage_pct: Option<f64>,
    pub(crate) swap: SwapParamsOwned,
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

pub(crate) struct SwapParamsOwned {
    pub(crate) input_mint: Mint,
    pub(crate) output_mint: Mint,
    pub(crate) raw_amount: String,
    pub(crate) taker: String,
    pub(crate) slippage_bps: Option<u32>,
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

pub async fn prepare_buy(
    wallet: &wallet::Wallet,
    symbol: &Symbol,
    amount: &str,
    rpc_url: Option<&str>,
    slippage_bps: Option<u32>,
    json: bool,
    quote_only: bool,
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
        preflight_buy_raw(&taker, &raw_usdc, rpc_url, !quote_only),
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
        input_mint: &plan.swap.input_mint,
        output_mint: &plan.swap.output_mint,
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
    super::gm_internal::check_trading_hours()
}

/// All-in quoted cost in bps (`fee_bps − slippage_pct·100`), or `None` when no
/// cost data is available. Mirrors the "Est. all-in" shown in previews.
pub(crate) fn all_in_cost_bps(slippage_pct: Option<f64>, fee_bps: Option<u32>) -> Option<f64> {
    match (slippage_pct, fee_bps) {
        (None, None) => None,
        (s, f) => Some(f.unwrap_or(0) as f64 - s.unwrap_or(0.0) * 100.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::gm_internal::{calc_slippage, check_slippage, MIN_SOL_FOR_FEES};

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

    // ── classify_error ────────────────────────────────────────

    #[test]
    fn classify_error_recognizes_gm_trade_error() {
        let err: eyre::Error = GmTradeError::new(GmTradeErrorKind::SlippageTooHigh, "x").into();
        assert_eq!(classify_error(&err), Some("slippage_too_high"));

        let err: eyre::Error = GmTradeError::new(GmTradeErrorKind::MarketClosed, "y").into();
        assert_eq!(classify_error(&err), Some("market_closed"));

        let err: eyre::Error = GmTradeError::new(GmTradeErrorKind::NotTradable, "z").into();
        assert_eq!(classify_error(&err), Some("not_tradable"));
    }

    #[test]
    fn classify_error_recognizes_jupiter_execute_failure() {
        let err: eyre::Error = jupiter::ExecuteFailure {
            kind: jupiter::ExecuteFailureKind::QuoteExpired,
            code: Some(-2003),
            message: "expired".to_string(),
        }
        .into();
        assert_eq!(classify_error(&err), Some("quote_expired"));

        let err: eyre::Error = jupiter::ExecuteFailure {
            kind: jupiter::ExecuteFailureKind::SwapRejected,
            code: Some(-2004),
            message: "rejected".to_string(),
        }
        .into();
        assert_eq!(classify_error(&err), Some("swap_rejected"));
    }

    #[test]
    fn classify_error_returns_none_for_opaque_eyre_error() {
        let err: eyre::Error = eyre::eyre!("unstructured failure");
        assert_eq!(classify_error(&err), None);
    }

    #[test]
    fn classify_error_sees_kind_through_wrap_err() {
        use eyre::WrapErr;
        let inner: eyre::Error = jupiter::ExecuteFailure {
            kind: jupiter::ExecuteFailureKind::QuoteExpired,
            code: Some(-2003),
            message: "expired".to_string(),
        }
        .into();
        // Reproduce the real path: execute error wrapped with human context.
        let wrapped = Err::<(), eyre::Error>(inner)
            .wrap_err("swap execution failed")
            .unwrap_err();
        assert_eq!(classify_error(&wrapped), Some("quote_expired"));
    }

    #[test]
    fn all_in_cost_bps_matches_preview_formula() {
        assert_eq!(all_in_cost_bps(Some(-0.45), Some(10)), Some(55.0));
        assert_eq!(all_in_cost_bps(Some(-0.30), None), Some(30.0));
        assert_eq!(all_in_cost_bps(None, Some(10)), Some(10.0));
        assert_eq!(all_in_cost_bps(Some(0.20), Some(10)), Some(-10.0));
        assert_eq!(all_in_cost_bps(None, None), None);
    }

    #[test]
    fn cost_too_high_classifies() {
        use eyre::WrapErr;
        let err: eyre::Error = GmTradeError::new(GmTradeErrorKind::CostTooHigh, "x").into();
        assert_eq!(GmTradeErrorKind::CostTooHigh.to_string(), "cost_too_high");
        let wrapped = Err::<(), eyre::Error>(err).wrap_err("swap").unwrap_err();
        assert_eq!(classify_error(&wrapped), Some("cost_too_high"));
    }
}
