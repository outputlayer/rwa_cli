use eyre::{Result, WrapErr, eyre};

use crate::{amounts, jupiter, solana, token_list};
use super::gm::{
    SellOrderReady, BuyOrderReady, DEFAULT_SLIPPAGE_BPS, GmTradeError, GmTradeErrorKind,
    cost_exceeds_max_bps,
};
use super::gm_internal::{
    MIN_SOL_FOR_FEES, MIN_USDC_AMOUNT, check_tradable, check_trading_hours, get_order_checked,
    resolve_gm_mint,
};
use crate::types::Symbol;

/// Reject the quote when its all-in cost (spread + fee) exceeds `max_bps`.
/// Single chokepoint for close-all and both baskets, mirroring the gate in
/// `prepare_buy`/`prepare_sell`.
fn check_cost_gate(
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
        let scaled = amounts::pct_of_u128(raw, pct);
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
    let minimum = 10u128.pow(jupiter::USDC_DECIMALS as u32) * MIN_USDC_AMOUNT as u128;
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

/// Preflight for basket buy: verify trading is open, each item meets the buy
/// minimum, total USDC coverage, and SOL for fees.
pub async fn preflight_basket_buy(
    pubkey: &str,
    items: &[(String, u128)],
    total_usdc_raw: u128,
    rpc_url: Option<&str>,
) -> Result<()> {
    check_trading_hours()?;
    check_basket_buy_minimums(items)?;
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
    slippage_bps: Option<u32>,
    max_bps: Option<u32>,
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
    Ok(BuyOrderReady {
        symbol: sym.to_string(),
        gm_mint,
        usdc_raw: usdc_raw.to_string(),
        usdc_display,
        order,
        slippage_pct,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basket_buy_rejects_items_below_minimum() {
        // Same floor as single `buy` (MIN_USDC_AMOUNT = 1 USDC): a basket item
        // below it must fail fast, naming the offending symbol — Jupiter MMs
        // reject tiny orders outright.
        let items = vec![
            ("AAPLon".to_string(), 12_000_000u128), // 12 USDC — fine
            ("TSLAon".to_string(), 999_999u128),    // 0.999999 USDC — below min
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
            ("AAPLon".to_string(), 1_000_000u128), // exactly 1 USDC
            ("SPYon".to_string(), 12_000_000u128),
        ];
        assert!(check_basket_buy_minimums(&items).is_ok());
    }
}
