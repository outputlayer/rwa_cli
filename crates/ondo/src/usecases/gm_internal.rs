use eyre::{Result, WrapErr, eyre};

use crate::{amounts, api, gm, jupiter, solana, token_list};
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
/// Minimum buy amount in USDC. Jupiter RFQ MMs routinely decline orders below
/// ~5 USDC outside Regular hours, so fail fast locally instead of burning a
/// quote round-trip that ends in `route_unfillable`.
pub(crate) const MIN_USDC_AMOUNT: f64 = 5.0;
/// Minimum SOL balance required to cover transaction fees (rent + priority).
pub(crate) const MIN_SOL_FOR_FEES: f64 = 0.002;

/// The buy minimum in raw USDC units — single source for `buy` and `buy-basket`.
pub(crate) fn min_usdc_raw() -> u128 {
    10u128.pow(jupiter::USDC_DECIMALS as u32) * MIN_USDC_AMOUNT as u128
}

pub fn resolve_gm_mint(symbol: &Symbol, tokens: &[token_list::GmTokenEntry]) -> Result<(Symbol, Mint)> {
    let entry = gm::resolve_token(symbol, tokens)?;
    let mint = entry
        .solana_address
        .ok_or_else(|| eyre!("No Solana address for {}", entry.symbol))?;
    Ok((Symbol::from(entry.symbol), Mint::from(mint)))
}

/// Estimate quote slippage in percent: price impact when the order reports it,
/// otherwise the in/out USD value delta.
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

/// Per-token session gate. Weekends/holidays map to Ondo's `offhours` session
/// (24/7 trading for select flagship tokens), so there is no blanket
/// market-closed block anymore — a token that can't trade off-hours gets a
/// typed `market_closed`, everything else follows the per-session limits.
pub(crate) async fn check_tradable(symbol: &str, api_url: Option<&str>) -> Result<()> {
    let session = api::current_session();
    let off_hours = session == api::Session::Closed;
    let limits = match api::fetch_session_limits(api_url).await {
        Ok(l) => l,
        Err(e) => {
            if off_hours {
                // Off-hours with no limits data: fail closed. Only a handful
                // of tokens trade 24/7 and we can't tell which without the API.
                return Err(GmTradeError::new(
                    GmTradeErrorKind::MarketClosed,
                    format!(
                        "off-hours session and the limits endpoint is unreachable ({e}) — cannot verify {symbol} trades 24/7. Regular trading resumes Sunday 8:00 PM ET."
                    ),
                )
                .into());
            }
            // Fail open: an unreachable limits endpoint must not block trading,
            // but say so on stderr — a silently skipped check would mask real
            // outages (stderr never corrupts the JSON stdout contract).
            eprintln!("Warning: session-limits check unavailable ({e}); assuming {symbol} is tradable.");
            return Ok(());
        }
    };
    let sym_upper = symbol.to_uppercase();
    let limit = limits
        .iter()
        .find(|l| l.symbol.to_uppercase() == sym_upper);

    let is_tradable = limit.map(|l| l.is_tradable(session)).unwrap_or(false);
    if !is_tradable {
        if off_hours {
            use chrono_tz::US::Eastern;
            let now = chrono::Utc::now().with_timezone(&Eastern);
            return Err(GmTradeError::new(
                GmTradeErrorKind::MarketClosed,
                format!(
                    "{symbol} does not trade off-hours (weekend/holiday); only select flagship tokens trade 24/7. Regular trading resumes Sunday 8:00 PM ET (current time: {} ET). Run `rwa gm hours --tradable` to see what's available now.",
                    now.format("%A %I:%M %p")
                ),
            )
            .into());
        }
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

/// Pure USDC affordability check for a buy. Separated from `preflight_buy_raw`
/// so it is unit-testable and so `--quote-only` can skip it. Amounts are raw
/// on-chain units; `requested` is raw USDC. SOL is deliberately NOT checked
/// here — whether SOL is needed at all depends on the quoted route (see
/// `check_sol_for_route`).
fn check_buy_funds(usdc_balance_raw: &str, requested: u128) -> Result<()> {
    let balance: u128 = usdc_balance_raw
        .parse()
        .map_err(|_| eyre!("Invalid on-chain USDC amount: {usdc_balance_raw}"))?;
    if balance < requested {
        return Err(GmTradeError::new(
            GmTradeErrorKind::InsufficientFunds,
            format!(
                "Insufficient USDC: {:.6} USDC (need {:.6})",
                balance as f64 / 10f64.powi(jupiter::USDC_DECIMALS as i32),
                requested as f64 / 10f64.powi(jupiter::USDC_DECIMALS as i32)
            ),
        )
        .into());
    }
    Ok(())
}

/// SOL-for-fees gate, applied AFTER quoting: on a gasless route Jupiter is the
/// fee/rent payer, so a USDC-only wallet (zero SOL) can trade. A self-paid
/// route (`gasless` false or unknown — e.g. the Metis fallback) still needs
/// `MIN_SOL_FOR_FEES`.
pub(crate) fn check_sol_for_route(gasless: Option<bool>, sol_lamports: u64) -> Result<()> {
    if gasless == Some(true) {
        return Ok(());
    }
    let sol = sol_lamports as f64 / 1_000_000_000.0;
    if sol < MIN_SOL_FOR_FEES {
        return Err(GmTradeError::new(
            GmTradeErrorKind::InsufficientFunds,
            format!(
                "The quoted route is not gasless and needs SOL for fees: have {sol:.6} SOL, need ~{MIN_SOL_FOR_FEES} SOL. Fund the wallet with SOL, or retry — a gasless route needs none."
            ),
        )
        .into());
    }
    Ok(())
}

/// Buy preflight: minimum + USDC affordability. Returns the wallet's SOL
/// balance in lamports so the caller can run the route-aware SOL gate after
/// quoting (`check_sol_for_route`). Returns 0 without touching the RPC when
/// `check_funds` is false (`--quote-only`). When the auto-gas gate already
/// sampled the balances this command (`snapshot`), they are reused instead of
/// refetched — one wallet-state sample per command.
pub(crate) async fn preflight_buy_raw(
    pubkey: &str,
    raw_usdc_amount: &str,
    rpc_url: Option<&str>,
    check_funds: bool,
    snapshot: Option<super::gm_gas::BalanceSnapshot>,
) -> Result<u64> {
    let requested: u128 = raw_usdc_amount
        .parse()
        .map_err(|_| eyre!("Invalid USDC amount: {raw_usdc_amount}"))?;
    let minimum = min_usdc_raw();
    if requested < minimum {
        return Err(GmTradeError::new(
            GmTradeErrorKind::AmountBelowMinimum,
            format!("Minimum buy amount is {MIN_USDC_AMOUNT} USDC"),
        )
        .into());
    }
    if !check_funds {
        return Ok(0);
    }
    if let Some(snap) = snapshot {
        check_buy_funds(&snap.usdc_raw.to_string(), requested)
            .wrap_err_with(|| format!("Fund wallet: {pubkey}"))?;
        return Ok(snap.sol_lamports);
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
    check_buy_funds(&balance_raw, requested)
        .wrap_err_with(|| format!("Fund wallet: {pubkey}"))?;
    Ok(sol_lamports)
}

/// Resolve a sell amount expression (`all`, `50%`, exact) against the wallet's
/// GM-token balance, returning `(display_amount, raw_amount)`. The single
/// chokepoint for `sell` and `sell-basket`; selling more than the balance is a
/// typed `insufficient_funds` error, mirroring the buy path.
pub(crate) fn resolve_sell_amount(
    amount_str: &str,
    symbol: &str,
    balance_raw: &str,
    decimals: u8,
) -> Result<(String, String)> {
    let s = amount_str.trim();
    if s.eq_ignore_ascii_case("all") {
        return Ok((
            amounts::format_amount(balance_raw, decimals),
            balance_raw.to_string(),
        ));
    }
    if let Some(pct_str) = s.strip_suffix('%') {
        let pct: f64 = pct_str
            .parse()
            .map_err(|_| eyre!("Invalid percentage: {amount_str}"))?;
        if !(0.0..=100.0).contains(&pct) {
            return Err(eyre!("Percentage must be 0–100, got {pct}"));
        }
        let raw: u128 = balance_raw
            .parse()
            .map_err(|_| eyre!("Invalid on-chain amount: {balance_raw}"))?;
        let pct_raw = amounts::pct_of_u128(raw, pct).to_string();
        return Ok((amounts::format_amount(&pct_raw, decimals), pct_raw));
    }
    let raw = amounts::token_to_raw(s, decimals)?;
    let raw_sell: u128 = raw
        .parse()
        .map_err(|_| eyre!("Invalid amount: {amount_str}"))?;
    let raw_balance: u128 = balance_raw
        .parse()
        .map_err(|_| eyre!("Invalid on-chain amount: {balance_raw}"))?;
    if raw_sell > raw_balance {
        return Err(GmTradeError::new(
            GmTradeErrorKind::InsufficientFunds,
            format!(
                "Insufficient {symbol} balance: have {}, trying to sell {}",
                amounts::format_amount(balance_raw, decimals),
                amounts::format_amount(&raw, decimals)
            ),
        )
        .into());
    }
    Ok((amounts::format_amount(&raw, decimals), raw))
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
        assert!(super::check_buy_funds("100000000", 50_000_000).is_ok());
    }

    #[test]
    fn check_buy_funds_errors_on_insufficient_usdc() {
        let err = super::check_buy_funds("50000000", 100_000_000).unwrap_err();
        assert!(err.to_string().contains("Insufficient USDC"));
    }

    #[test]
    fn sol_gate_is_route_aware() {
        // Gasless route (Jupiter pays fees + rent): a USDC-only wallet with
        // ZERO SOL must be allowed to trade.
        assert!(check_sol_for_route(Some(true), 0).is_ok());
        // Self-paid route (Metis: gasless=false) or unknown: SOL required.
        let err = check_sol_for_route(Some(false), 0).unwrap_err();
        let te = err.downcast_ref::<GmTradeError>().expect("typed");
        assert_eq!(te.kind, GmTradeErrorKind::InsufficientFunds);
        assert!(check_sol_for_route(None, 0).is_err(), "unknown gasless = conservative");
        // Boundary: exactly MIN_SOL_FOR_FEES (2_000_000 lamports) passes.
        assert!(check_sol_for_route(None, 2_000_000).is_ok());
        assert!(check_sol_for_route(Some(false), 1_999_999).is_err());
    }

    #[test]
    fn resolve_sell_amount_all_pct_exact() {
        let (d, r) = resolve_sell_amount("all", "TSLA", "1500000000", 9).unwrap();
        assert_eq!(r, "1500000000");
        assert_eq!(d, "1.5");

        let (_, r) = resolve_sell_amount("50%", "TSLA", "1500000000", 9).unwrap();
        assert_eq!(r, "750000000");

        let (d, r) = resolve_sell_amount("1", "TSLA", "1500000000", 9).unwrap();
        assert_eq!(r, "1000000000");
        assert_eq!(d, "1");
    }

    #[test]
    fn resolve_sell_amount_over_balance_is_typed_insufficient_funds() {
        let err = resolve_sell_amount("2", "TSLA", "1500000000", 9).unwrap_err();
        let te = err.downcast_ref::<GmTradeError>().expect("typed error");
        assert_eq!(te.kind, GmTradeErrorKind::InsufficientFunds);
        assert!(te.detail.contains("TSLA"), "detail: {}", te.detail);
    }

    #[test]
    fn resolve_sell_amount_rejects_bad_percentage() {
        assert!(resolve_sell_amount("150%", "TSLA", "1500000000", 9).is_err());
        assert!(resolve_sell_amount("-5%", "TSLA", "1500000000", 9).is_err());
        assert!(resolve_sell_amount("abc%", "TSLA", "1500000000", 9).is_err());
    }


}
