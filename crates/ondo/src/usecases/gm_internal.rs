use eyre::{Result, eyre};

use crate::{api, gm, jupiter, solana, token_list};
use crate::types::{Mint, Symbol};
use super::gm::{GmTradeError, GmTradeErrorKind};

/// Hard block — reject the trade if slippage exceeds this after all retries.
pub(crate) const MAX_SLIPPAGE_PCT: f64 = 3.0;
/// Slippage threshold that triggers a fresh quote retry (seek a better MM).
pub(crate) const SLIPPAGE_RETRY_PCT: f64 = 1.0;
/// Maximum retries when slippage exceeds the retry threshold.
/// jupiterz routes to multiple MMs — one may quote -10% on small orders.
/// Retrying cycles through MMs until we get a reasonable fill.
pub(crate) const MAX_SLIPPAGE_RETRIES: u32 = 5;
/// Maximum retries for transient swap execution failures.
pub(crate) const MAX_SWAP_RETRIES: u32 = 2;
/// Minimum buy/sell amount in USDC.
pub(crate) const MIN_USDC_AMOUNT: f64 = 1.0;
/// Minimum SOL balance required to cover transaction fees (rent + priority).
pub(crate) const MIN_SOL_FOR_FEES: f64 = 0.002;

pub(crate) fn resolve_gm_mint(symbol: &Symbol, tokens: &[token_list::GmTokenEntry]) -> Result<(Symbol, Mint)> {
    let entry = gm::resolve_token(symbol, tokens)?;
    let mint = entry
        .solana_address
        .ok_or_else(|| eyre!("No Solana address for {}", entry.symbol))?;
    Ok((Symbol::from(entry.symbol), Mint::from(mint)))
}

/// Compute `value * pct / 100` using integer math to avoid f64 precision loss.
pub(crate) fn calc_slippage(order: &jupiter::OrderResponse) -> Option<f64> {
    if let Some(pi) = order.price_impact {
        return Some(pi);
    }
    match (order.in_usd_value, order.out_usd_value) {
        (Some(usd_in), Some(usd_out)) if usd_in > 0.0 => Some((usd_out - usd_in) / usd_in * 100.0),
        _ => None,
    }
}

pub(crate) fn slippage_block_hint(s: f64, order: &jupiter::OrderResponse) -> String {
    let router = order.router.as_deref().unwrap_or("unknown");
    format!(
        "slippage {s:.2}% via {router} exceeds -{MAX_SLIPPAGE_PCT:.0}% after {MAX_SLIPPAGE_RETRIES} retries. \
         Try a larger amount or wait for better liquidity."
    )
}

pub(crate) fn check_slippage(order: &jupiter::OrderResponse, json: bool) -> Result<Option<f64>> {
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

pub(crate) async fn get_order_checked(
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
    json: bool,
    jupiter_url: Option<&str>,
) -> Result<(jupiter::OrderResponse, Option<f64>)> {
    let mut order = jupiter::get_order(jupiter_url, input_mint, output_mint, amount, taker, slippage_bps).await?;
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
            order = jupiter::get_order(jupiter_url, input_mint, output_mint, amount, taker, slippage_bps).await?;
            continue;
        }
        return Ok((order, slip));
    }
    // All retries exhausted — apply hard block if still above safety-net threshold.
    let slippage_pct = check_slippage(&order, json)?;
    Ok((order, slippage_pct))
}

pub(crate) fn check_trading_hours() -> Result<()> {
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

pub(crate) async fn check_tradable(symbol: &str, api_url: Option<&str>) -> Result<()> {
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

/// Pure affordability check for a buy. Separated from `preflight_buy_raw` so it
/// is unit-testable and so `--quote-only` can skip it. Amounts are raw on-chain
/// units; `requested` is raw USDC.
fn check_buy_funds(usdc_balance_raw: &str, sol_lamports: u64, requested: u128) -> Result<()> {
    let balance: u128 = usdc_balance_raw
        .parse()
        .map_err(|_| eyre!("Invalid on-chain USDC amount: {usdc_balance_raw}"))?;
    if balance < requested {
        return Err(eyre!(
            "Insufficient USDC: {:.6} USDC (need {:.6})",
            balance as f64 / 10f64.powi(jupiter::USDC_DECIMALS as i32),
            requested as f64 / 10f64.powi(jupiter::USDC_DECIMALS as i32)
        ));
    }
    let sol = sol_lamports as f64 / 1_000_000_000.0;
    if sol < MIN_SOL_FOR_FEES {
        return Err(eyre!(
            "Insufficient SOL for transaction fees: have {sol:.6} SOL, need ~{MIN_SOL_FOR_FEES} SOL."
        ));
    }
    Ok(())
}

pub(crate) async fn preflight_buy_raw(
    pubkey: &str,
    raw_usdc_amount: &str,
    rpc_url: Option<&str>,
    check_funds: bool,
) -> Result<()> {
    check_trading_hours()?;
    let requested: u128 = raw_usdc_amount
        .parse()
        .map_err(|_| eyre!("Invalid USDC amount: {raw_usdc_amount}"))?;
    let minimum = 10u128.pow(jupiter::USDC_DECIMALS as u32) * MIN_USDC_AMOUNT as u128;
    if requested < minimum {
        return Err(eyre!("Minimum buy amount is {MIN_USDC_AMOUNT} USDC"));
    }
    if !check_funds {
        return Ok(());
    }
    let (usdc_res, sol_raw_res) = tokio::join!(
        solana::get_usdc_balance_raw(pubkey, rpc_url),
        solana::get_sol_balance_raw(pubkey, rpc_url),
    );
    let (_, balance_raw) = usdc_res?;
    let sol_raw = sol_raw_res?;
    let sol_lamports: u64 = sol_raw
        .parse()
        .map_err(|_| eyre!("Invalid on-chain SOL amount: {sol_raw}"))?;
    check_buy_funds(&balance_raw, sol_lamports, requested)
        .map_err(|e| eyre!("{e}\n  Fund wallet: {pubkey}"))
}

pub(crate) fn preflight_sell() -> Result<()> {
    check_trading_hours()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slippage_block_hint_includes_router() {
        let order = jupiter::OrderResponse {
            router: Some("jupiterz".into()),
            ..Default::default()
        };
        let hint = slippage_block_hint(-5.0, &order);
        assert!(hint.contains("jupiterz"), "hint: {hint}");
        assert!(hint.contains("-5.00%"), "hint: {hint}");
        assert!(hint.contains("retries"), "hint: {hint}");
    }

    #[test]
    fn slippage_block_hint_liquidity_message() {
        let order = jupiter::OrderResponse {
            router: Some("jupiter".into()),
            ..Default::default()
        };
        let hint = slippage_block_hint(-4.0, &order);
        assert!(hint.contains("liquidity") || hint.contains("larger"), "hint: {hint}");
    }

    #[test]
    fn check_buy_funds_ok_when_sufficient() {
        assert!(super::check_buy_funds("100000000", 10_000_000, 50_000_000).is_ok());
    }

    #[test]
    fn check_buy_funds_errors_on_insufficient_usdc() {
        let err = super::check_buy_funds("50000000", 10_000_000, 100_000_000).unwrap_err();
        assert!(err.to_string().contains("Insufficient USDC"));
    }

    #[test]
    fn check_buy_funds_errors_on_insufficient_sol() {
        let err = super::check_buy_funds("100000000", 0, 50_000_000).unwrap_err();
        assert!(err.to_string().contains("Insufficient SOL"));
    }

    #[test]
    fn check_buy_funds_sol_boundary_is_strict_less_than() {
        // exactly MIN_SOL_FOR_FEES (2_000_000 lamports) passes; one below fails.
        assert!(super::check_buy_funds("100000000", 2_000_000, 50_000_000).is_ok());
        assert!(super::check_buy_funds("100000000", 1_999_999, 50_000_000).is_err());
    }
}
