use eyre::{Result, WrapErr, eyre};

use crate::{amounts, jupiter, solana, token_list};
use super::gm::{SellOrderReady, BuyOrderReady, DEFAULT_SLIPPAGE_BPS};
use super::gm_internal::{resolve_gm_mint, get_order_checked, check_trading_hours, check_tradable, MIN_SOL_FOR_FEES};
use crate::types::Symbol;

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
        None,
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
    fetch_sell_order(&sym, &mint_str, &raw_amount, taker, json).await.map(|mut r| {
        r.display_amount = display_amount;
        r
    })
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
