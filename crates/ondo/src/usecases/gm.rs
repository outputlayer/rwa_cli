use eyre::{Result, WrapErr, eyre};

use crate::{amounts, api, jupiter, solana, token_list, wallet};
use crate::types::{Mint, Symbol};

// Re-export from sibling modules so callers can continue using `usecases::gm::*`.
pub use super::gm_execute::{execute_sell_from_order, execute_buy_from_order};
pub use super::gm_order::{fetch_sell_order, fetch_sell_order_by_symbol, fetch_buy_order, preflight_basket_buy, split_total_by_weights, split_total_equal};
pub use super::gm_gas::{ensure_gas, BalanceSnapshot, GasRefuel, REFUEL_USDC_RAW, SOL_LOW_WATER_LAMPORTS};
pub use super::gm_pnl::{compute_pnl, PnlSummary, TokenPnl};
pub use super::gm_positions::{
    compute_portfolio, filter_close_positions, ClosePosition, PortfolioPosition,
    PortfolioSummary, PortfolioUnavailable,
};
use super::gm_order::{check_cost_gate, check_limit_gate};

pub use super::gm_limit::LimitFrame;
pub(crate) use super::gm_limit::{effective_limit_raw6, limit_price_exceeded};
pub use super::gm_internal::{resolve_gm_mint, insufficient_balance_message};
use super::gm_internal::{
    check_tradable, check_sol_for_route, preflight_buy_raw,
    get_order_checked, resolve_sell_amount,
};
use super::gm_execute::{execute_with_retry, finalize_execution, record_swap_outcome, SwapAuditCtx, SwapParams};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GmTradeErrorKind {
    MarketClosed,
    NotTradable,
    SlippageTooHigh,
    CostTooHigh,
    AmountBelowMinimum,
    InsufficientFunds,
    NoPosition,
    /// `--limit-price` condition not met by the quoted implied price.
    ConditionNotMet,
    /// Ondo has paused trading for this asset (a dividend window, or the market
    /// being closed for it — weekends flag most non-24/7 tokens as paused).
    TradingPaused,
    /// `--json` without `-y`: non-interactive mode fails closed instead of
    /// silently executing a real trade/transfer.
    ConfirmationRequired,
    /// Symbol not in the GM token list (typo or non-GM asset).
    UnknownToken,
    /// Amount failed to parse (bad format, too many decimals, bad percentage).
    InvalidAmount,
    /// Recipient is not a valid Solana address.
    InvalidAddress,
    /// `--wallet`/`RWA_WALLET` named a wallet absent from the registry.
    UnknownWallet,
    /// `send` recipient equals the sender.
    SelfSend,
    /// `keys export --json` without `--reveal`.
    RevealRequired,
    /// An admin operation (policy edit, key export, decrypt) needs the
    /// passphrase typed at a live terminal — env/keychain are deliberately
    /// not consulted, so non-interactive runs fail closed.
    InteractiveRequired,
    /// `send` recipient is not in the wallet's allowed-recipient policy
    /// (add with `keys policy allow`, human-only).
    RecipientNotAllowed,
    /// Another rwa process holds the exclusive lock — transient by definition.
    LockContention,
}

#[derive(Debug)]
pub struct GmTradeError {
    pub kind: GmTradeErrorKind,
    pub detail: String,
}

impl GmTradeError {
    pub fn new(kind: GmTradeErrorKind, detail: impl Into<String>) -> Self {
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

impl GmTradeErrorKind {
    /// Every variant, in declaration order. Source list for the docs-coverage
    /// guard (`error_kinds_are_documented` in the cli crate asserts each label
    /// appears in llms.txt / README / CLAUDE.md). Adding a variant already
    /// breaks `label()`'s exhaustive match at compile time — when you fix that,
    /// add the variant here and document it, or the guard fails.
    pub const ALL: [GmTradeErrorKind; 19] = [
        Self::MarketClosed,
        Self::NotTradable,
        Self::SlippageTooHigh,
        Self::CostTooHigh,
        Self::AmountBelowMinimum,
        Self::InsufficientFunds,
        Self::NoPosition,
        Self::ConditionNotMet,
        Self::TradingPaused,
        Self::ConfirmationRequired,
        Self::UnknownToken,
        Self::InvalidAmount,
        Self::InvalidAddress,
        Self::UnknownWallet,
        Self::SelfSend,
        Self::RevealRequired,
        Self::InteractiveRequired,
        Self::RecipientNotAllowed,
        Self::LockContention,
    ];

    /// Stable label used in JSON `error_kind` fields and error Display.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::MarketClosed => "market_closed",
            Self::NotTradable => "not_tradable",
            Self::SlippageTooHigh => "slippage_too_high",
            Self::CostTooHigh => "cost_too_high",
            Self::AmountBelowMinimum => "amount_below_minimum",
            Self::InsufficientFunds => "insufficient_funds",
            Self::NoPosition => "no_position",
            Self::ConditionNotMet => "condition_not_met",
            Self::TradingPaused => "trading_paused",
            Self::ConfirmationRequired => "confirmation_required",
            Self::UnknownToken => "unknown_token",
            Self::InvalidAmount => "invalid_amount",
            Self::InvalidAddress => "invalid_address",
            Self::UnknownWallet => "unknown_wallet",
            Self::SelfSend => "self_send",
            Self::RevealRequired => "reveal_required",
            Self::InteractiveRequired => "interactive_required",
            Self::RecipientNotAllowed => "recipient_not_allowed",
            Self::LockContention => "lock_contention",
        }
    }
}

impl std::fmt::Display for GmTradeErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl std::error::Error for GmTradeError {}

/// Classify an eyre error as a stable error kind label, suitable for JSON
/// `error_kind` fields consumed by agents/scripts. Returns `None` for opaque
/// errors that aren't one of the known structured types.
///
/// Recognizes `GmTradeError`, Jupiter `ExecuteFailure`, Solana `TransactionError`
/// (e.g. `confirmation_timeout`, `on_chain_failure`), and Solana RPC failures
/// (surfaced as `rpc_unavailable`).
#[must_use]
pub fn classify_error(err: &eyre::Error) -> Option<&'static str> {
    for cause in err.chain() {
        if let Some(g) = cause.downcast_ref::<GmTradeError>() {
            return Some(g.kind.label());
        }
        if let Some(f) = cause.downcast_ref::<jupiter::ExecuteFailure>() {
            return Some(f.kind.label());
        }
        if let Some(t) = cause.downcast_ref::<crate::solana::TransactionError>() {
            return Some(t.kind.label());
        }
        if cause.downcast_ref::<crate::solana::SolanaRpcError>().is_some() {
            return Some("rpc_unavailable");
        }
        if cause.downcast_ref::<crate::solana::NoTokenAccount>().is_some() {
            return Some("no_position");
        }
    }
    None
}

/// Whether an `error_kind` label denotes a transient failure worth retrying
/// (network/exchange hiccup) vs a permanent one (bad input, no funds, closed
/// market). Drives the CLI exit code: transient → 75 (EX_TEMPFAIL), else 1.
///
/// `/execute` labels (`failed_to_land`, `quote_expired`, `swap_rejected`, …)
/// are classified straight from `ExecuteFailureKind::retry_action()` — a
/// label is transient iff Jupiter's own retry loop (`execute_with_retry`)
/// considers it worth a fresh order/resubmit. That's the single source of
/// truth so this can't drift from the retry loop as new codes are added.
#[must_use]
pub fn is_transient_kind(kind: &str) -> bool {
    if matches!(kind, "rpc_unavailable" | "confirmation_timeout" | "lock_contention") {
        return true;
    }
    jupiter::ExecuteFailureKind::ALL
        .into_iter()
        .find(|k| k.label() == kind)
        .is_some_and(|k| k.retry_action() != jupiter::ExecuteRetryAction::None)
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
    /// Scaled-UI multiplier (shares per raw token). Informational; `None`
    /// when unknown. Required (fail-closed) only for share-frame limits.
    pub multiplier: Option<f64>,
    pub(crate) swap: SwapParamsOwned,
    output_decimals: u8,
}

pub struct SwapExecution {
    pub output_amount: String,
    /// Raw on-chain output amount (smallest units) — feeds the trade ledger.
    pub output_amount_raw: String,
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
    /// Slippage setting the quote was fetched with — reused when the execute
    /// retry loop refetches, so reroutes honor the user's tolerance.
    pub slippage_bps: Option<u32>,
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
    /// Slippage setting the quote was fetched with — reused when the execute
    /// retry loop refetches, so reroutes honor the user's tolerance.
    pub slippage_bps: Option<u32>,
}

#[allow(clippy::too_many_arguments)]
pub async fn prepare_buy(
    wallet: &wallet::Wallet,
    symbol: &Symbol,
    amount: &str,
    rpc_url: Option<&str>,
    slippage_bps: Option<u32>,
    json: bool,
    quote_only: bool,
    max_bps: Option<u32>,
    balances: Option<BalanceSnapshot>,
    limit_price: Option<(u128, LimitFrame)>,
) -> Result<SwapPlan> {
    let tokens = token_list::get_token_list();
    let (symbol, gm_mint) = resolve_gm_mint(symbol, tokens)?;
    let taker = wallet.pubkey();

    let raw_usdc = amounts::resolve_amount_to_raw(amount, jupiter::USDC_DECIMALS, || {
        let taker = taker.clone();
        let rpc = rpc_url.map(str::to_string);
        async move {
            // `%`/`all` amounts resolve against the auto-gas snapshot when
            // available — no extra fetch.
            if let Some(snap) = balances {
                return Ok(snap.usdc_raw.to_string());
            }
            let (_, raw) = solana::get_usdc_balance_raw(&taker, rpc.as_deref()).await?;
            Ok(raw)
        }
    })
    .await?;
    let usdc_amount = amounts::format_amount(&raw_usdc, jupiter::USDC_DECIMALS);

    let (preflight_res, tradable_res, multiplier_res) = tokio::join!(
        preflight_buy_raw(&taker, &raw_usdc, rpc_url, !quote_only, balances),
        check_tradable(&symbol, None),
        solana::get_mint_multiplier(&gm_mint, rpc_url),
    );
    let sol_lamports = preflight_res?;
    tradable_res?;
    // Share-frame limits must not execute on an unknown multiplier: propagate
    // the RPC error (transient → exit 75; cron retries next tick).
    let multiplier = match (&limit_price, multiplier_res) {
        (Some((_, LimitFrame::Share)), Err(e)) => return Err(e).wrap_err(
            "share-frame --limit-price requires the mint's scaled-UI multiplier",
        ),
        (_, Ok(m)) => m,
        (_, Err(_)) => None, // informational only — best-effort
    };

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

    check_cost_gate(slippage_pct, order.fee_bps, max_bps)?;
    let limit_raw6 = effective_limit_raw6(limit_price, multiplier)?;
    check_limit_gate(true, &order.in_amount, &order.out_amount, limit_raw6)?;
    // Route-aware SOL gate: gasless routes need no SOL (USDC-only wallets
    // trade); self-paid routes (Metis) need MIN_SOL_FOR_FEES.
    if !quote_only {
        check_sol_for_route(order.gasless, sol_lamports)?;
    }

    Ok(SwapPlan {
        symbol,
        amount: amounts::format_amount(&order.out_amount, jupiter::GM_SOL_DECIMALS),
        counter_amount: usdc_amount,
        order,
        slippage_pct,
        multiplier,
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

/// Derive the Scaled-UI multiplier (underlying shares per raw token) from a
/// balance's raw/ui-amount pair: `ui_balance / balance`, only when
/// `ui_balance` is present, `balance` is strictly positive, and the ratio is
/// finite and positive. Pure extraction of `prepare_sell`'s inline derivation
/// (previously untested — no direct unit test existed) so it is testable
/// without a wallet or RPC; no behavior change.
fn derive_balance_multiplier(balance: f64, ui_balance: Option<f64>) -> Option<f64> {
    ui_balance
        .filter(|_| balance > 0.0)
        .map(|ui| ui / balance)
        .filter(|m| m.is_finite() && *m > 0.0)
}

#[allow(clippy::too_many_arguments)]
pub async fn prepare_sell(
    wallet: &wallet::Wallet,
    symbol: &Symbol,
    amount: &str,
    rpc_url: Option<&str>,
    slippage_bps: Option<u32>,
    json: bool,
    max_bps: Option<u32>,
    limit_price: Option<(u128, LimitFrame)>,
) -> Result<SwapPlan> {
    let tokens = token_list::get_token_list();
    let (symbol, gm_mint) = resolve_gm_mint(symbol, tokens)?;
    let taker = wallet.pubkey();
    let gm_dec = jupiter::GM_SOL_DECIMALS;

    let (tradable_res, bal_res) = tokio::join!(
        check_tradable(&symbol, None),
        solana::get_balance(&taker, &gm_mint, rpc_url),
    );
    tradable_res?;
    // Name the SYMBOL, not the raw mint: `NoTokenAccount` alone prints
    // "No token account found for mint Gwh9…ondo", which means nothing to a
    // human. classify_error still finds the typed cause in the chain.
    let bal = bal_res.map_err(|e| {
        e.wrap_err(format!(
            "You hold no {symbol} — nothing to sell (see `rwa gm portfolio`)"
        ))
    })?;
    if bal.balance <= 0.0 {
        return Err(GmTradeError::new(
            GmTradeErrorKind::NoPosition,
            format!("You hold no {symbol} — nothing to sell (see `rwa gm portfolio`)"),
        )
        .into());
    }

    let multiplier = derive_balance_multiplier(bal.balance, bal.ui_balance);
    let multiplier = match (multiplier, &limit_price) {
        (None, Some((_, LimitFrame::Share))) => {
            solana::get_mint_multiplier(&gm_mint, rpc_url).await.wrap_err(
                "share-frame --limit-price requires the mint's scaled-UI multiplier",
            )?
        }
        (m, _) => m,
    };

    let (sell_amount, raw_gm) = resolve_sell_amount(amount, &symbol, &bal.raw_amount, gm_dec, bal.ui_balance)?;

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

    check_cost_gate(slippage_pct, order.fee_bps, max_bps)?;
    let limit_raw6 = effective_limit_raw6(limit_price, multiplier)?;
    check_limit_gate(false, &order.out_amount, &order.in_amount, limit_raw6)?;

    Ok(SwapPlan {
        symbol,
        amount: sell_amount,
        counter_amount: amounts::format_amount(&order.out_amount, jupiter::USDC_DECIMALS),
        order,
        slippage_pct,
        multiplier,
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
    // Single buy/sell: same audit + ledger trail as the basket paths.
    let op = if plan.swap.output_mint.as_ref() == jupiter::USDC_MINT { "sell" } else { "buy" };
    let ctx = SwapAuditCtx {
        op,
        symbol: plan.symbol.to_string(),
        raw_amount: plan.swap.raw_amount.clone(),
        taker: plan.swap.taker.clone(),
        slippage_bps: plan.swap.slippage_bps,
        backend: plan.order.backend.label().to_string(),
    };
    let outcome = execute_with_retry(wallet, &plan.order, json, &params)
        .await
        .wrap_err("swap execution failed")
        .map(|result| finalize_execution(&plan.order, &result, plan.output_decimals));
    record_swap_outcome(ctx, &outcome);
    let exec = outcome?;
    if let Some(actual) = exec.actual_slippage_pct
        && let Some(quoted) = plan.slippage_pct
    {
        let drift = actual - quoted;
        if drift.abs() > 1.0 && !json {
            eprintln!(
                "Warning: actual slippage {actual:.2}% differs from quoted {quoted:.2}% (drift {drift:+.2}%)"
            );
        }
    }
    Ok(exec)
}

pub fn parse_sell_pct(amount: Option<&str>) -> Result<f64> {
    let Some(raw) = amount else { return Ok(100.0); };
    let s = raw.trim();
    let pct_str = s
        .strip_suffix('%')
        .ok_or_else(|| eyre!("close-all amount must be a percentage (e.g. 10%, 50%)"))?;
    crate::amounts::parse_pct(pct_str, s)
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

/// Portfolio balances with the Jupiter Ultra holdings fallback: Solana RPC
/// first; when every endpoint is unavailable, fall back to the holdings API
/// (result marked `source: jupiter`). The fallback lives here — not in the
/// solana layer — so `solana` stays a leaf module with no jupiter dependency.
pub async fn fetch_portfolio_balances(
    wallet_addr: &str,
    tokens: &[token_list::GmTokenEntry],
    rpc_url: Option<&str>,
) -> Result<solana::PortfolioBalances> {
    match solana::get_portfolio_balances(wallet_addr, tokens, rpc_url).await {
        Err(e) if solana::is_rpc_unavailable(&e) => {
            crate::jupiter::holdings::get_holdings_balances(wallet_addr, tokens)
                .await
                .map_err(|je| {
                    eyre!("Solana RPC unavailable and Jupiter holdings fallback failed: {je} (rpc: {e})")
                })
        }
        res => res,
    }
}

pub async fn fetch_tradable_set(api_url: Option<&str>) -> std::collections::HashSet<String> {
    // Weekends/holidays map to Ondo's offhours session — select tokens trade
    // 24/7, so the set is never statically empty.
    let session = api::current_session();
    api::fetch_session_limits(api_url)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|l| l.is_tradable(session))
        .map(|l| l.symbol.to_uppercase())
        .collect()
}

/// All-in quoted cost in bps (`fee_bps − slippage_pct·100`), or `None` when no
/// cost data is available. Mirrors the "Est. all-in" shown in previews.
pub(crate) fn all_in_cost_bps(slippage_pct: Option<f64>, fee_bps: Option<u32>) -> Option<f64> {
    match (slippage_pct, fee_bps) {
        (None, None) => None,
        (s, f) => Some(f.unwrap_or(0) as f64 - s.unwrap_or(0.0) * 100.0),
    }
}

/// The cost gate decision: returns `Some((cost_bps, max))` when a `max_bps` is
/// set, the all-in cost is computable, and it strictly exceeds `max` (→ reject);
/// `None` otherwise (→ allow). Pure, so the money-path decision is unit-tested.
pub(crate) fn cost_exceeds_max_bps(
    slippage_pct: Option<f64>,
    fee_bps: Option<u32>,
    max_bps: Option<u32>,
) -> Option<(f64, u32)> {
    let max = max_bps?;
    let cost = all_in_cost_bps(slippage_pct, fee_bps)?;
    (cost > max as f64).then_some((cost, max))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::gm_internal::{calc_slippage, check_slippage, MIN_SOL_FOR_FEES};

    #[test]
    fn slippage_from_price_impact() {
        // `price_impact` is a PERCENT, normalized at source (swap/v2 native;
        // Metis mapper ×100). calc_slippage returns it verbatim, and the
        // price_impact branch takes priority over the USD values whenever it's
        // `Some`. So -0.5 here means -0.5%.
        let order = jupiter::OrderResponse {
            in_amount: "100".into(),
            out_amount: "99".into(),
            in_usd_value: Some(100.0),
            out_usd_value: Some(99.0),
            price_impact: Some(-0.5),
            ..Default::default()
        };
        let slip = calc_slippage(&order);
        assert!((slip.unwrap() - (-0.5)).abs() < f64::EPSILON, "got {:?}", slip);
    }

    #[test]
    fn check_slippage_blocks_a_real_four_percent_impact() {
        // Behavioral: check_slippage (the actual trade-path consumer) must
        // BLOCK a -4% impact at the -3% hard gate. `price_impact` is a percent
        // here (as the Metis mapper now produces from a "-0.04" fraction ×100).
        let order = jupiter::OrderResponse {
            price_impact: Some(-4.0),
            router: Some("metis-v1-lite".into()),
            ..Default::default()
        };
        let err = check_slippage(&order, false).unwrap_err();
        let te = err.downcast_ref::<GmTradeError>().expect("typed error");
        assert_eq!(te.kind, GmTradeErrorKind::SlippageTooHigh);
    }

    #[test]
    fn slippage_from_usd_values() {
        // Guard: the USD branch is untouched by the QM-1 fix and must keep
        // returning a percent unchanged.
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
    fn derive_balance_multiplier_matches_hand_computed_scaled_ratio() {
        // SPYon fixture from the scenario suite: raw balance 20.369073,
        // Scaled-UI balance 20.526341. m = ui/raw = 1.0077209208293376
        // (hand-divided here, independent of any cached value in the code).
        // A mutant swapping the operands (balance/ui) would give
        // 0.9923380... instead; dropping the division entirely (returning
        // ui_balance verbatim) would give 20.526341.
        let m = derive_balance_multiplier(20.369073, Some(20.526341)).unwrap();
        assert!((m - 1.007_720_920_829_337_6).abs() < 1e-12, "got {m}");
    }

    #[test]
    fn derive_balance_multiplier_none_without_ui_balance() {
        // No Scaled-UI info from this balance source (e.g. Jupiter Ultra
        // holdings) — must not fabricate a multiplier.
        assert_eq!(derive_balance_multiplier(5.0, None), None);
    }

    #[test]
    fn derive_balance_multiplier_zero_balance_guards_against_div_by_zero() {
        // balance <= 0 must short-circuit to None rather than divide by
        // zero. A mutant relaxing `> 0.0` to `>= 0.0` would let this through
        // (5.0 / 0.0 = inf, which the finite-check would then also need to
        // catch — this test targets the balance guard specifically).
        assert_eq!(derive_balance_multiplier(0.0, Some(5.0)), None);
    }

    #[test]
    fn derive_balance_multiplier_rejects_non_finite_and_non_positive_ratios() {
        // Non-finite ratio (NaN) must not leak through as a "valid" multiplier.
        assert_eq!(derive_balance_multiplier(1.0, Some(f64::NAN)), None);
        // A negative ui_balance (shouldn't occur, but the fn must not treat
        // it as a valid multiplier) yields a negative ratio -> filtered out.
        assert_eq!(derive_balance_multiplier(5.0, Some(-5.0)), None);
        // Exactly 1.0 (no scaling) is a legitimate multiplier — must NOT be
        // filtered by this fn (that's a display-layer decision elsewhere).
        assert_eq!(derive_balance_multiplier(5.0, Some(5.0)), Some(1.0));
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
        // Audit fix: assert the computed value, not just Ok — a sign flip or
        // wrong in/out pairing inside ±3% would otherwise pass unnoticed.
        // $100 in → $99 out = −1% slippage, allowed and reported as-is.
        let slip = check_slippage(&order, true).expect("within limit");
        assert!((slip.unwrap() - (-1.0)).abs() < 0.01, "slippage {slip:?}");
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
    fn classify_error_recognizes_fund_errors() {
        let err: eyre::Report = GmTradeError::new(
            GmTradeErrorKind::InsufficientFunds,
            "Insufficient USDC: 1.00 (need 12.00)",
        )
        .into();
        assert_eq!(classify_error(&err), Some("insufficient_funds"));

        let err: eyre::Report = GmTradeError::new(
            GmTradeErrorKind::NoPosition,
            "Balance is 0 for TSLAon — nothing to sell",
        )
        .into();
        assert_eq!(classify_error(&err), Some("no_position"));

        // Missing token account (sell with no ATA) classifies as no_position too.
        let err: eyre::Report = crate::solana::NoTokenAccount {
            mint: "k18WJUULWheRkSpSquYGdNNmtuE2Vbw1hpuUi92ondo".into(),
        }
        .into();
        assert_eq!(classify_error(&err), Some("no_position"));
    }

    #[test]
    fn transient_kind_classification_for_exit_codes() {
        for k in ["rpc_unavailable", "execute_unavailable", "confirmation_timeout"] {
            assert!(is_transient_kind(k), "{k} must be transient");
        }
        for k in ["market_closed", "insufficient_funds", "cost_too_high", "no_position", "slippage_too_high", "condition_not_met", "trading_paused", "confirmation_required"] {
            assert!(!is_transient_kind(k), "{k} must be permanent");
        }
    }

    /// M2: `/execute` failure kinds must exit 75 (transient) exactly when
    /// `ExecuteFailureKind::retry_action()` says a retry is worth attempting —
    /// that's the source of truth, not a hand-picked list. This covers more
    /// than the four originally-suspected labels: `retry_action()` also marks
    /// `missing_cached_order`, `swap_rejected`, and `route_unfillable` as
    /// `RefreshOrder` (i.e. Jupiter itself already retries them internally via
    /// `execute_with_retry`), so they must be transient too, consistent with
    /// `execute_unavailable`'s existing exit-75 treatment.
    #[test]
    fn execute_transient_kinds_exit_75() {
        for label in [
            "failed_to_land",
            "rfq_failed_to_land",
            "internal_error",
            "quote_expired",
            "missing_cached_order",
            "swap_rejected",
            "route_unfillable",
        ] {
            assert!(
                is_transient_kind(label),
                "{label} carries a retry action (retry_action() != None) and must be transient (exit 75)"
            );
        }
        // Kinds with `retry_action() == None` stay permanent: nothing about a
        // fresh command invocation would fix these (bad payload, unparseable
        // response, genuinely unknown code).
        for label in [
            "invalid_signed_transaction",
            "invalid_message_bytes",
            "unknown_aggregator_error",
            "unknown_rfq_error",
            "invalid_payload",
            "unknown",
        ] {
            assert!(!is_transient_kind(label), "{label} must be permanent");
        }
    }

    /// Cross-check `is_transient_kind` against `ExecuteFailureKind::ALL`
    /// directly so the classification can never silently drift from
    /// `retry_action()` as new variants are added.
    #[test]
    fn is_transient_kind_matches_retry_action_for_every_execute_kind() {
        for kind in jupiter::ExecuteFailureKind::ALL {
            let expected = kind.retry_action() != jupiter::ExecuteRetryAction::None;
            assert_eq!(
                is_transient_kind(kind.label()),
                expected,
                "{} : is_transient_kind must match retry_action() != None",
                kind.label()
            );
        }
    }

    #[test]
    fn trading_paused_label_and_not_transient() {
        assert_eq!(GmTradeErrorKind::TradingPaused.to_string(), "trading_paused");
        assert!(!is_transient_kind("trading_paused"));
    }

    #[test]
    fn classify_error_recognizes_amount_below_minimum() {
        let err: eyre::Report = GmTradeError::new(
            GmTradeErrorKind::AmountBelowMinimum,
            "Minimum buy amount is 1 USDC",
        )
        .into();
        assert_eq!(classify_error(&err), Some("amount_below_minimum"));
    }

    #[test]
    fn classify_error_recognizes_transaction_error() {
        use crate::solana::{TransactionError, TransactionErrorKind};
        // Wrapped the way callers do: typed error inside an eyre chain.
        let err = eyre::Report::new(TransactionError {
            kind: TransactionErrorKind::ConfirmationTimeout,
            detail: "transaction may still land: abc123".into(),
        })
        .wrap_err("swap execution failed");
        assert_eq!(classify_error(&err), Some("confirmation_timeout"));

        let err: eyre::Report = TransactionError {
            kind: TransactionErrorKind::OnChainFailure,
            detail: "program failed".into(),
        }
        .into();
        assert_eq!(classify_error(&err), Some("on_chain_failure"));
    }

    #[test]
    fn classify_error_maps_rpc_failures_to_rpc_unavailable() {
        use crate::solana::SolanaRpcError;
        use crate::solana::SolanaRpcErrorKind;
        let err: eyre::Report = SolanaRpcError {
            kind: SolanaRpcErrorKind::Unavailable,
            method: Some("getBalance".into()),
            url: None,
            status: None,
            code: None,
            detail: "all RPC endpoints failed".into(),
        }
        .into();
        assert_eq!(classify_error(&err), Some("rpc_unavailable"));
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
            offhours: None,
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
            offhours: None,
        };
        // Audit fix: assert the parsed value, not merely "> 0" — a string
        // parse bug yielding 1.0 (or dropping digits) passed the old check.
        assert_eq!(limits.max_notional(Session::Overnight), Some(100000.0));
        // A session with no limits entry has no cap.
        assert_eq!(limits.max_notional(Session::Regular), None);
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
    fn cost_exceeds_max_bps_decision() {
        // cost 55 (fee10, slip -0.45%) > max 30 -> reject, returns (cost, max)
        assert_eq!(cost_exceeds_max_bps(Some(-0.45), Some(10), Some(30)), Some((55.0, 30)));
        // cost 55 <= max 100 -> allow
        assert_eq!(cost_exceeds_max_bps(Some(-0.45), Some(10), Some(100)), None);
        // no max set -> allow
        assert_eq!(cost_exceeds_max_bps(Some(-0.45), Some(10), None), None);
        // favorable cost (-10 bps) never exceeds, even at max 0
        assert_eq!(cost_exceeds_max_bps(Some(0.20), Some(10), Some(0)), None);
        // no cost data -> allow
        assert_eq!(cost_exceeds_max_bps(None, None, Some(5)), None);
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

    #[test]
    fn condition_not_met_classifies() {
        assert_eq!(GmTradeErrorKind::ConditionNotMet.to_string(), "condition_not_met");
        let err: eyre::Error = GmTradeError::new(
            GmTradeErrorKind::ConditionNotMet,
            "quoted price 401.20 USDC/token > --limit-price 400",
        )
        .into();
        assert_eq!(classify_error(&err), Some("condition_not_met"));
        assert!(!is_transient_kind("condition_not_met"));
    }

    #[test]
    fn confirmation_required_classifies() {
        assert_eq!(GmTradeErrorKind::ConfirmationRequired.to_string(), "confirmation_required");
        let err: eyre::Error = GmTradeError::new(
            GmTradeErrorKind::ConfirmationRequired,
            "--json is non-interactive: add -y to execute, or use --dry-run to preview",
        )
        .into();
        assert_eq!(classify_error(&err), Some("confirmation_required"));
        assert!(!is_transient_kind("confirmation_required"));
    }

    #[test]
    fn input_and_environment_kinds_classify_with_stable_labels() {
        // The audit-added kinds: common user-input failures that previously
        // surfaced as `error_kind: null`, forcing agents to match English prose.
        let cases: &[(GmTradeErrorKind, &str)] = &[
            (GmTradeErrorKind::UnknownToken, "unknown_token"),
            (GmTradeErrorKind::InvalidAmount, "invalid_amount"),
            (GmTradeErrorKind::InvalidAddress, "invalid_address"),
            (GmTradeErrorKind::UnknownWallet, "unknown_wallet"),
            (GmTradeErrorKind::SelfSend, "self_send"),
            (GmTradeErrorKind::RevealRequired, "reveal_required"),
            (GmTradeErrorKind::LockContention, "lock_contention"),
        ];
        for &(kind, label) in cases {
            assert_eq!(kind.to_string(), label);
            let err: eyre::Error = GmTradeError::new(kind, "x").into();
            assert_eq!(classify_error(&err), Some(label), "classify {label}");
        }
        // Lock contention is the one transient among them (retry when the
        // other process exits → exit 75 in BOTH human and json modes); the
        // rest are permanent bad-input failures (exit 1).
        assert!(is_transient_kind("lock_contention"));
        for label in [
            "unknown_token",
            "invalid_amount",
            "invalid_address",
            "unknown_wallet",
            "self_send",
            "reveal_required",
        ] {
            assert!(!is_transient_kind(label), "{label} must not be transient");
        }
    }

    #[test]
    fn amount_parse_failures_are_typed_invalid_amount() {
        // End-to-end through the real parser: garbage, excess decimals, and a
        // bad percentage must all classify — not just the enum in isolation.
        for input in ["abc", "1.2.3", "5.1234567"] {
            let err = crate::amounts::token_to_raw(input, 6).unwrap_err();
            assert_eq!(classify_error(&err), Some("invalid_amount"), "input {input}");
        }
    }

    #[test]
    fn unknown_symbol_is_typed_unknown_token() {
        let tokens = crate::token_list::get_token_list();
        let err = crate::symbol_resolve::resolve_token("NOTATOKEN", tokens).unwrap_err();
        assert_eq!(classify_error(&err), Some("unknown_token"));
        // The message still teaches the discovery path for humans.
        assert!(err.to_string().contains("gm search"), "should point at search: {err}");
    }

    #[test]
    fn interactive_required_is_typed_and_labeled() {
        let err: eyre::Error = GmTradeError::new(
            GmTradeErrorKind::InteractiveRequired,
            "keys decrypt requires typing the passphrase at a terminal",
        )
        .into();
        assert_eq!(classify_error(&err), Some("interactive_required"));
    }

    #[test]
    fn recipient_not_allowed_is_typed_and_labeled() {
        let err: eyre::Error = GmTradeError::new(
            GmTradeErrorKind::RecipientNotAllowed,
            "recipient is not in the allowed list",
        )
        .into();
        assert_eq!(classify_error(&err), Some("recipient_not_allowed"));
    }

    #[test]
    fn all_error_kinds_listed_with_unique_labels() {
        // Compile-time tripwire: adding a variant breaks this exhaustive match
        // (and `label()`'s), a reminder to add it to `ALL` and document it —
        // the cli-crate `error_kinds_are_documented` guard then checks the docs.
        for k in GmTradeErrorKind::ALL {
            match k {
                GmTradeErrorKind::MarketClosed
                | GmTradeErrorKind::NotTradable
                | GmTradeErrorKind::SlippageTooHigh
                | GmTradeErrorKind::CostTooHigh
                | GmTradeErrorKind::AmountBelowMinimum
                | GmTradeErrorKind::InsufficientFunds
                | GmTradeErrorKind::NoPosition
                | GmTradeErrorKind::ConditionNotMet
                | GmTradeErrorKind::TradingPaused
                | GmTradeErrorKind::ConfirmationRequired
                | GmTradeErrorKind::UnknownToken
                | GmTradeErrorKind::InvalidAmount
                | GmTradeErrorKind::InvalidAddress
                | GmTradeErrorKind::UnknownWallet
                | GmTradeErrorKind::SelfSend
                | GmTradeErrorKind::RevealRequired
                | GmTradeErrorKind::InteractiveRequired
                | GmTradeErrorKind::RecipientNotAllowed
                | GmTradeErrorKind::LockContention => {}
            }
        }
        let mut seen = std::collections::HashSet::new();
        for k in GmTradeErrorKind::ALL {
            let l = k.label();
            assert!(!l.is_empty(), "empty label for {k:?}");
            assert!(seen.insert(l), "duplicate error_kind label: {l}");
        }
    }
}
