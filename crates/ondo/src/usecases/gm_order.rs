use eyre::{Result, WrapErr, eyre};

use crate::{amounts, jupiter, solana, token_list};
use super::gm::{
    SellOrderReady, BuyOrderReady, DEFAULT_SLIPPAGE_BPS, GmTradeError, GmTradeErrorKind,
    cost_exceeds_max_bps, limit_price_exceeded,
};
use super::gm_internal::{
    MIN_USDC_AMOUNT, check_sol_for_route, check_tradable, get_order_checked,
    min_usdc_raw, resolve_gm_mint, resolve_sell_amount,
};
use crate::types::Symbol;

/// Reject the quote when its all-in cost (spread + fee) exceeds `max_bps`.
/// Single chokepoint for close-all and both baskets, mirroring the gate in
/// `prepare_buy`/`prepare_sell`.
pub(crate) fn check_cost_gate(
    slippage_pct: Option<f64>,
    fee_bps: Option<u32>,
    max_bps: Option<u32>,
) -> Result<()> {
    if let Some((cost, max)) = cost_exceeds_max_bps(slippage_pct, fee_bps, max_bps) {
        return Err(GmTradeError::new(
            GmTradeErrorKind::CostTooHigh,
            format!("all-in cost {cost:.1} bps exceeds --max-bps {max}"),
        )
        .into());
    }
    Ok(())
}

/// Reject the quote when its implied price (USDC per token) violates
/// `--limit-price`. Single chokepoint for `prepare_buy`/`prepare_sell` —
/// the quote that passes here is the one executed.
pub(crate) fn check_limit_gate(
    is_buy: bool,
    usdc_raw: &str,
    token_raw: &str,
    limit_raw6: Option<u128>,
) -> Result<()> {
    if let Some((implied, limit)) = limit_price_exceeded(is_buy, usdc_raw, token_raw, limit_raw6) {
        let dir = if is_buy { ">" } else { "<" };
        return Err(GmTradeError::new(
            GmTradeErrorKind::ConditionNotMet,
            format!("quoted price {implied:.6} USDC/token {dir} --limit-price {limit:.6}"),
        )
        .into());
    }
    Ok(())
}

/// Phase 1 for parallel close-all: fetch + validate a sell order without executing.
pub async fn fetch_sell_order(
    symbol: &str,
    mint: &str,
    raw_amount: &str,
    taker: &str,
    json: bool,
    slippage_bps: Option<u32>,
    max_bps: Option<u32>,
) -> Result<SellOrderReady> {
    let display_amount = amounts::format_amount(raw_amount, jupiter::GM_SOL_DECIMALS);
    let (order, slippage_pct) = get_order_checked(
        mint,
        jupiter::USDC_MINT,
        raw_amount,
        taker,
        slippage_bps,
        json,
        None,
    )
    .await?;
    check_cost_gate(slippage_pct, order.fee_bps, max_bps)?;
    Ok(SellOrderReady {
        symbol: symbol.to_string(),
        mint: mint.to_string(),
        raw_amount: raw_amount.to_string(),
        display_amount,
        order,
        slippage_pct,
        slippage_bps,
    })
}

/// High-level phase 1 for sell-basket: resolve symbol, compute raw amount from user input
/// (exact token amount, "50%", or "all"), check tradable, fetch + validate sell order.
#[allow(clippy::too_many_arguments)]
pub async fn fetch_sell_order_by_symbol(
    symbol_str: &str,
    amount_str: &str,
    taker: &str,
    json: bool,
    rpc_url: Option<&str>,
    slippage_bps: Option<u32>,
    max_bps: Option<u32>,
) -> Result<SellOrderReady> {
    let tokens = token_list::get_token_list();
    let sym = Symbol::from(symbol_str);
    let (sym, gm_mint) = resolve_gm_mint(&sym, tokens)?;
    check_tradable(&sym, None).await?;

    let gm_dec = jupiter::GM_SOL_DECIMALS;
    let bal = solana::get_balance(taker, &gm_mint, rpc_url).await
        .wrap_err_with(|| format!("failed to fetch {sym} balance"))?;
    if bal.balance <= 0.0 {
        return Err(GmTradeError::new(
            GmTradeErrorKind::NoPosition,
            format!("Balance is 0 for {sym} — nothing to sell"),
        )
        .into());
    }

    let (display_amount, raw_amount) =
        resolve_sell_amount(amount_str, &sym, &bal.raw_amount, gm_dec, bal.ui_balance)?;

    let mint_str = gm_mint.to_string();
    fetch_sell_order(&sym, &mint_str, &raw_amount, taker, json, slippage_bps, max_bps)
        .await
        .map(|mut r| {
            r.display_amount = display_amount;
            r
        })
}

/// Per-item minimum for basket buys — the same floor as single `buy`
/// (`MIN_USDC_AMOUNT`): Jupiter MMs reject tiny orders outright, so fail fast
/// locally and name the offending symbol instead of burning a quote round-trip.
fn check_basket_buy_minimums(items: &[(String, u128)]) -> Result<()> {
    let minimum = min_usdc_raw();
    for (sym, raw) in items {
        if *raw < minimum {
            return Err(GmTradeError::new(
                GmTradeErrorKind::AmountBelowMinimum,
                format!("Minimum buy amount is {MIN_USDC_AMOUNT} USDC per token; {sym} is below it"),
            )
            .into());
        }
    }
    Ok(())
}

/// Split a `--total` USDC amount across percent-weight basket pairs.
///
/// Every pair amount must be a percentage (`NN%`, up to 6 decimal places, > 0)
/// and the weights must sum to exactly 100. Each item gets
/// `total × weight / 100` floored to raw USDC; the rounding dust goes to the
/// largest weight (first on ties) so the spent sum equals `total` exactly.
/// Each computed amount must meet the per-item buy floor (`MIN_USDC_AMOUNT`).
/// All checks are local — safe to run before any wallet or network access.
pub fn split_total_by_weights(
    total: &str,
    pairs: &[(String, String)],
) -> Result<Vec<(String, u128)>> {
    // 100% expressed in micro-percent (6 decimal places), the same scale
    // token_to_raw produces for the individual weights.
    const WEIGHT_SCALE: u128 = 100_000_000;

    let total_raw: u128 = amounts::token_to_raw(total, jupiter::USDC_DECIMALS)
        .ok()
        .and_then(|r| r.parse().ok())
        .ok_or_else(|| {
            GmTradeError::new(
                GmTradeErrorKind::InvalidAmount,
                format!("Invalid --total '{total}': use a positive USDC amount with up to 6 decimal places"),
            )
        })?;

    let mut weights: Vec<u128> = Vec::with_capacity(pairs.len());
    let mut seen: Vec<String> = Vec::with_capacity(pairs.len());
    for (sym, amt) in pairs {
        let upper = sym.to_uppercase();
        if seen.contains(&upper) {
            return Err(GmTradeError::new(
                GmTradeErrorKind::InvalidAmount,
                format!("Duplicate symbol '{sym}': each token may appear once when splitting --total"),
            )
            .into());
        }
        seen.push(upper);
        let Some(pct) = amt.trim().strip_suffix('%') else {
            return Err(GmTradeError::new(
                GmTradeErrorKind::InvalidAmount,
                format!("With --total every amount must be a percentage (e.g. 50%); got '{amt}' for {sym}"),
            )
            .into());
        };
        let micro: u128 = amounts::token_to_raw(pct.trim(), 6)
            .ok()
            .and_then(|r| r.parse().ok())
            .ok_or_else(|| {
                GmTradeError::new(
                    GmTradeErrorKind::InvalidAmount,
                    format!("Invalid percentage '{amt}' for {sym}: use a positive percent with up to 6 decimal places"),
                )
            })?;
        weights.push(micro);
    }

    let sum: u128 = weights.iter().sum();
    if sum != WEIGHT_SCALE {
        return Err(GmTradeError::new(
            GmTradeErrorKind::InvalidAmount,
            format!(
                "Basket weights sum to {}%, expected exactly 100%",
                amounts::format_amount(&sum.to_string(), 6)
            ),
        )
        .into());
    }

    let mut out: Vec<(String, u128)> = pairs
        .iter()
        .zip(&weights)
        .map(|((sym, _), w)| (sym.clone(), total_raw * w / WEIGHT_SCALE))
        .collect();
    let spent: u128 = out.iter().map(|(_, r)| r).sum();
    let mut dust_idx = 0;
    for (i, w) in weights.iter().enumerate() {
        if *w > weights[dust_idx] {
            dust_idx = i;
        }
    }
    out[dust_idx].1 += total_raw - spent;

    let minimum = min_usdc_raw();
    for ((sym, raw), (_, amt)) in out.iter().zip(pairs) {
        if *raw < minimum {
            return Err(GmTradeError::new(
                GmTradeErrorKind::AmountBelowMinimum,
                format!(
                    "{sym} gets {} USDC at {amt} of {total} — minimum is {MIN_USDC_AMOUNT} USDC per token",
                    amounts::format_amount(&raw.to_string(), jupiter::USDC_DECIMALS)
                ),
            )
            .into());
        }
    }
    Ok(out)
}

/// Preflight for basket buy: each item meets the buy minimum and total USDC
/// coverage. Returns the wallet's SOL balance in lamports for the per-item
/// route-aware SOL gate (`check_sol_for_route`) applied after each quote.
/// Per-token session gating happens in `check_tradable` per order.
pub async fn preflight_basket_buy(
    pubkey: &str,
    items: &[(String, u128)],
    total_usdc_raw: u128,
    rpc_url: Option<&str>,
    snapshot: Option<super::gm_gas::BalanceSnapshot>,
) -> Result<u64> {
    check_basket_buy_minimums(items)?;
    if let Some(snap) = snapshot {
        // Reuse the auto-gas gate's sample — one wallet-state read per command.
        if snap.usdc_raw < total_usdc_raw {
            return Err(GmTradeError::new(
                GmTradeErrorKind::InsufficientFunds,
                format!(
                    "Insufficient USDC: {:.2} available, need {:.2} total\n  Fund wallet: {pubkey}",
                    snap.usdc_raw as f64 / 10f64.powi(jupiter::USDC_DECIMALS as i32),
                    total_usdc_raw as f64 / 10f64.powi(jupiter::USDC_DECIMALS as i32),
                ),
            )
            .into());
        }
        return Ok(snap.sol_lamports);
    }
    let (usdc_res, sol_raw_res) = tokio::join!(
        solana::get_usdc_balance_raw(pubkey, rpc_url),
        solana::get_sol_balance_raw(pubkey, rpc_url),
    );
    let (_, balance_raw) = usdc_res?;
    let balance: u128 = balance_raw
        .parse()
        .map_err(|_| eyre!("Invalid on-chain USDC amount: {balance_raw}"))?;
    if balance < total_usdc_raw {
        return Err(GmTradeError::new(
            GmTradeErrorKind::InsufficientFunds,
            format!(
                "Insufficient USDC: {:.2} available, need {:.2} total\n  Fund wallet: {pubkey}",
                balance as f64 / 10f64.powi(jupiter::USDC_DECIMALS as i32),
                total_usdc_raw as f64 / 10f64.powi(jupiter::USDC_DECIMALS as i32),
            ),
        )
        .into());
    }
    let sol_raw = sol_raw_res?;
    let sol_lamports: u64 = sol_raw
        .parse()
        .map_err(|_| eyre!("Invalid on-chain SOL amount: {sol_raw}"))?;
    Ok(sol_lamports)
}

/// Phase 1 for basket buy: resolve mint, check tradable, fetch + validate buy
/// order. `sol_lamports` feeds the route-aware SOL gate (`None` skips it —
/// dry-run previews don't execute).
pub async fn fetch_buy_order(
    symbol: &str,
    usdc_raw: &str,
    taker: &str,
    json: bool,
    slippage_bps: Option<u32>,
    max_bps: Option<u32>,
    sol_lamports: Option<u64>,
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
        Some(slippage_bps.unwrap_or(DEFAULT_SLIPPAGE_BPS)),
        json,
        None,
    )
    .await?;
    check_cost_gate(slippage_pct, order.fee_bps, max_bps)?;
    if let Some(sol) = sol_lamports {
        check_sol_for_route(order.gasless, sol)?;
    }
    Ok(BuyOrderReady {
        symbol: sym.to_string(),
        gm_mint,
        usdc_raw: usdc_raw.to_string(),
        usdc_display,
        order,
        slippage_pct,
        slippage_bps: Some(slippage_bps.unwrap_or(DEFAULT_SLIPPAGE_BPS)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basket_buy_rejects_items_below_minimum() {
        // Same floor as single `buy` (MIN_USDC_AMOUNT = 5 USDC): a basket item
        // below it must fail fast, naming the offending symbol — Jupiter MMs
        // reject tiny orders outright.
        let items = vec![
            ("AAPLon".to_string(), 12_000_000u128), // 12 USDC — fine
            ("TSLAon".to_string(), 4_999_999u128),  // 4.999999 USDC — below min
        ];
        let err = check_basket_buy_minimums(&items).expect_err("sub-minimum item must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("TSLAon"), "must name the offending symbol: {msg}");
        assert!(msg.contains("Minimum buy amount"), "must explain the floor: {msg}");
        // Typed for agents: error_kind must classify as amount_below_minimum.
        assert_eq!(super::super::gm::classify_error(&err), Some("amount_below_minimum"));
    }

    #[test]
    fn basket_buy_accepts_items_at_or_above_minimum() {
        let items = vec![
            ("AAPLon".to_string(), 5_000_000u128), // exactly 5 USDC
            ("SPYon".to_string(), 12_000_000u128),
        ];
        assert!(check_basket_buy_minimums(&items).is_ok());
    }

    #[test]
    fn limit_gate_produces_condition_not_met() {
        // implied 400 vs buy limit 399 → gated.
        let err = check_limit_gate(true, "100000000", "250000000", Some(399_000_000))
            .unwrap_err();
        assert_eq!(crate::usecases::gm::classify_error(&err), Some("condition_not_met"));
        let msg = format!("{err:#}");
        assert!(msg.contains("--limit-price"), "message names the flag: {msg}");
        // No limit → passes.
        assert!(check_limit_gate(true, "100000000", "250000000", None).is_ok());
        // Sell side: implied 400 ≥ limit 399 → passes.
        assert!(check_limit_gate(false, "100000000", "250000000", Some(399_000_000)).is_ok());
    }

    /// User task: "Buy SPYon, cron-style, but only fill at 750 or better" —
    /// `--limit-price 750token`. Composes `effective_limit_raw6` (token frame
    /// is a no-op conversion) with `check_limit_gate` the way `prepare_buy`
    /// really chains them. `effective_limit_raw6`/`check_limit_gate` are both
    /// `pub(crate)`, so this lives here rather than in the external
    /// `crates/ondo/tests/scenarios.rs`.
    #[test]
    fn cron_limit_buy_fires_only_at_its_own_token_price() {
        use super::super::gm::{effective_limit_raw6, LimitFrame};
        let limit = effective_limit_raw6(Some((750_000_000, LimitFrame::Token)), None)
            .unwrap()
            .unwrap();
        assert_eq!(limit, 750_000_000);

        // Implied 753.99 USDC/token (one raw token, 9 decimals): quote
        // usdc_raw = 753_990_000, token_raw = 1_000_000_000.
        let violated = check_limit_gate(true, "753990000", "1000000000", Some(limit)).is_err();
        assert!(violated, "753.99 must violate a 750 buy limit");

        // Implied 749.5 → better than the limit → fills.
        assert!(check_limit_gate(true, "749500000", "1000000000", Some(limit)).is_ok());

        // Equality (implied == limit) fills.
        assert!(check_limit_gate(true, "750000000", "1000000000", Some(limit)).is_ok());
    }

    /// Same intent expressed in the SHARE frame: "fill only at 748/share or
    /// better", with the position's dividend multiplier (m=1.0077209)
    /// converting the user's share-price intent into the token-raw frame the
    /// quote is actually gated in.
    #[test]
    fn cron_limit_buy_in_share_frame_converts_via_multiplier() {
        use super::super::gm::{effective_limit_raw6, LimitFrame};
        let m = 1.0077209_f64;
        let limit = effective_limit_raw6(Some((748_000_000, LimitFrame::Share)), Some(m))
            .unwrap()
            .unwrap();
        // 748 * 1.0077209 ≈ 753.775233 USDC/token.
        assert_eq!(limit, 753_775_233);

        // Implied 753.5 ≤ effective 753.775233 → fills.
        assert!(check_limit_gate(true, "753500000", "1000000000", Some(limit)).is_ok());
        // Implied 754.2 > effective 753.775233 → violated.
        assert!(check_limit_gate(true, "754200000", "1000000000", Some(limit)).is_err());
        // Equality (implied == the rounded effective limit) fills.
        assert!(check_limit_gate(true, "753775233", "1000000000", Some(limit)).is_ok());
    }

    /// User task: "A 10:1 split just happened — does my standing share-frame
    /// limit order still mean what I set it to mean?" A share-frame limit's
    /// intent (a target SHARE price) must survive a multiplier jump: the same
    /// limit re-expressed at the new (10x) multiplier lands back near the
    /// original effective token-raw price.
    #[test]
    fn share_frame_limit_survives_a_ten_to_one_split() {
        use super::super::gm::{effective_limit_raw6, LimitFrame};
        // Pre-split: 748share at m=1.0077209 → effective ≈ 753.775233.
        let pre = effective_limit_raw6(Some((748_000_000, LimitFrame::Share)), Some(1.0077209))
            .unwrap()
            .unwrap();
        // Post-split: the user re-expresses the same underlying share target
        // at 1/10th (74.8share) against the new multiplier (10.077).
        let post = effective_limit_raw6(Some((74_800_000, LimitFrame::Share)), Some(10.077))
            .unwrap()
            .unwrap();
        assert_eq!(pre, 753_775_233);
        assert_eq!(post, 753_759_600);
        // Both land within a few cents/token of each other — the split alone
        // must not materially move the user's share-price intent.
        let diff = (pre as f64 - post as f64).abs() / 1e6;
        assert!(diff < 0.02, "effective limit drifted {diff} USDC/token across the split");
    }

    fn wpairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(s, a)| (s.to_string(), a.to_string())).collect()
    }

    #[test]
    fn split_total_even_weights() {
        let out = split_total_by_weights(
            "1000",
            &wpairs(&[("TSLA", "50%"), ("NVDA", "30%"), ("SPY", "20%")]),
        )
        .unwrap();
        assert_eq!(
            out,
            vec![
                ("TSLA".to_string(), 500_000_000u128),
                ("NVDA".to_string(), 300_000_000u128),
                ("SPY".to_string(), 200_000_000u128),
            ]
        );
    }

    #[test]
    fn split_total_dust_goes_to_first_largest_weight() {
        // 10.000001 USDC at 50/50: each floors to 5.000000; the 1-micro dust
        // goes to the FIRST of the tied largest weights, so the spent sum
        // equals --total exactly.
        let out = split_total_by_weights("10.000001", &wpairs(&[("TSLA", "50%"), ("NVDA", "50%")])).unwrap();
        assert_eq!(out[0].1, 5_000_001);
        assert_eq!(out[1].1, 5_000_000);
    }

    #[test]
    fn split_total_fractional_weights() {
        let out = split_total_by_weights("100", &wpairs(&[("TSLA", "87.5%"), ("NVDA", "12.5%")])).unwrap();
        assert_eq!(out[0].1, 87_500_000);
        assert_eq!(out[1].1, 12_500_000);
    }

    #[test]
    fn split_total_weights_must_sum_to_100() {
        let err = split_total_by_weights(
            "1000",
            &wpairs(&[("TSLA", "50%"), ("NVDA", "30%"), ("SPY", "10%")]),
        )
        .unwrap_err();
        assert_eq!(super::super::gm::classify_error(&err), Some("invalid_amount"));
        let msg = err.to_string();
        assert!(msg.contains("90"), "must name the actual sum: {msg}");
        assert!(msg.contains("100"), "must name the expectation: {msg}");
    }

    #[test]
    fn split_total_rejects_non_percent_amounts() {
        for bad in ["100", "all"] {
            let err = split_total_by_weights("1000", &wpairs(&[("TSLA", bad), ("NVDA", "50%")]))
                .unwrap_err();
            assert_eq!(super::super::gm::classify_error(&err), Some("invalid_amount"), "input {bad}");
            assert!(err.to_string().contains("--total"), "mentions the flag: {err}");
        }
    }

    #[test]
    fn split_total_rejects_bad_percent_values() {
        // zero, negative, too many decimal places, non-numeric
        for bad in ["0%", "-5%", "12.3456789%", "abc%"] {
            let err = split_total_by_weights("1000", &wpairs(&[("TSLA", bad), ("NVDA", "50%")]))
                .unwrap_err();
            assert_eq!(super::super::gm::classify_error(&err), Some("invalid_amount"), "input {bad}");
        }
    }

    #[test]
    fn split_total_rejects_duplicate_symbols() {
        let err = split_total_by_weights("1000", &wpairs(&[("TSLA", "50%"), ("tsla", "50%")]))
            .unwrap_err();
        assert_eq!(super::super::gm::classify_error(&err), Some("invalid_amount"));
        assert!(err.to_string().contains("Duplicate"), "{err}");
    }

    #[test]
    fn split_total_enforces_per_item_minimum() {
        // 4% of 100 = 4 USDC, below the 5 USDC per-item floor.
        let err = split_total_by_weights("100", &wpairs(&[("TSLA", "96%"), ("SPY", "4%")]))
            .unwrap_err();
        assert_eq!(super::super::gm::classify_error(&err), Some("amount_below_minimum"));
        let msg = err.to_string();
        assert!(msg.contains("SPY") && msg.contains("4%"), "names symbol and weight: {msg}");
    }

    #[test]
    fn split_total_rejects_bad_total() {
        for bad in ["0", "-100", "abc", "10.1234567"] {
            let err = split_total_by_weights(bad, &wpairs(&[("TSLA", "100%")])).unwrap_err();
            assert_eq!(super::super::gm::classify_error(&err), Some("invalid_amount"), "input {bad}");
        }
    }
}
