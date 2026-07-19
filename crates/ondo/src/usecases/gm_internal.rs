//! Shared preflight internals for buy/sell/basket paths: slippage measurement
//! and the 3% block (`calc_slippage`/`check_slippage`), tradability and
//! trading-pause checks, gasless-route SOL requirements, minimum-amount
//! enforcement, and the checked order fetch. Also home of the retry and
//! slippage tuning constants.

use eyre::{Result, WrapErr, eyre};

use crate::{amounts, api, jupiter, solana, symbol_resolve, token_list};
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
/// Multiplies BEFORE truncating so a fractional future minimum (e.g. 5.5)
/// converts exactly instead of silently flooring to 5.
pub(crate) fn min_usdc_raw() -> u128 {
    (MIN_USDC_AMOUNT * 10f64.powi(jupiter::USDC_DECIMALS as i32)) as u128
}

pub fn resolve_gm_mint(symbol: &Symbol, tokens: &[token_list::GmTokenEntry]) -> Result<(Symbol, Mint)> {
    let entry = symbol_resolve::resolve_token(symbol, tokens)?;
    let mint = entry
        .solana_address
        .ok_or_else(|| eyre!("No Solana address for {}", entry.symbol))?;
    Ok((Symbol::from(entry.symbol), Mint::from(mint)))
}

/// Estimate quote slippage in percent: price impact when the order reports it,
/// otherwise the in/out USD value delta.
pub(crate) fn calc_slippage(order: &jupiter::OrderResponse) -> Option<f64> {
    if let Some(pi) = order.price_impact {
        // `price_impact` is normalized to a PERCENT at each backend source
        // (swap/v2 already returns it as a percent; the Metis mapper converts
        // Jupiter's `priceImpactPct` fraction ×100 — see jupiter/order.rs), so
        // it is unit-consistent with the USD branch below and every consumer.
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
    match slip {
        Some(s) => {
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
        // QM-2: neither price_impact nor in/out USD values were reported, so
        // the -3% hard block above cannot be evaluated. Fail OPEN here — an
        // honest thin-token quote can legitimately omit both fields, and
        // hard-blocking would break real trades on nothing but missing
        // metrics — but say so, since a silent pass would contradict the
        // documented ">3% slippage is blocked" guarantee. Callers that set
        // `--max-bps` get a stricter fail-closed gate instead (see
        // `check_cost_gate`), since they explicitly asked for a verified cap.
        None if !json => {
            eprintln!("Warning: slippage could not be measured for this quote; the 3% guard was not applied.");
        }
        None => {}
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
        // Below the retry threshold (or unmeasurable) — apply the policy
        // (hard block, or QM-2's unmeasurable-slippage warning) before
        // returning. Previously this early return skipped `check_slippage`
        // entirely, so a quote that never reported slippage data returned
        // silently: no warning, and (had it somehow been well below -3%) no
        // hard block either. For a `Some` slip this is a no-op — the loop
        // guard above already proved it's not worse than -SLIPPAGE_RETRY_PCT
        // (1%), which is inside the -3% hard-block threshold too.
        let slippage_pct = check_slippage(&order, json)?;
        return Ok((order, slippage_pct));
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
    // Ondo pauses assets around dividend events AND flags most non-24/7
    // tokens as paused when the market is closed (weekends). Fail open on
    // fetch errors (mirrors the session-limits behavior below); the assets
    // response is disk-cached for 60s, so this is usually a free lookup.
    match api::fetch_assets().await {
        Ok(assets) => {
            if api::is_trading_paused(symbol, &assets) {
                return Err(GmTradeError::new(
                    GmTradeErrorKind::TradingPaused,
                    format!(
                        "{symbol} trading is paused by Ondo — either a dividend window (ex-dividend; ETFs longer) or the market being closed for this asset (weekends flag most non-24/7 tokens as paused). Run `rwa gm hours` for when trading resumes and `rwa gm tradable {symbol}` to re-check."
                    ),
                )
                .into());
            }
        }
        Err(e) => eprintln!("Warning: assets check unavailable ({e}); skipping trading-paused check for {symbol}."),
    }

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

/// Convert a raw token amount to a display float. Used for comparing ui_balance
/// to raw balance in the insufficient-balance message. Panics never occur:
/// `format_amount` handles all valid input.
fn raw_amount_to_f64_lossy(raw: &str, decimals: u8) -> f64 {
    amounts::format_amount(raw, decimals).parse().unwrap_or(0.0)
}

/// Format a Scaled-UI wallet-displayed balance for the insufficient-balance
/// note: fixed to 6 decimals (float noise like `20.526341000000002` is not
/// user-facing), then trailing zeros and a bare trailing dot are trimmed.
fn format_ui_trimmed(ui: f64) -> String {
    let s = format!("{ui:.6}");
    let s = s.trim_end_matches('0');
    s.trim_end_matches('.').to_string()
}

/// Build the "insufficient balance" message shared by `sell` and `send`:
/// names the raw balance, an optional wallet-displayed (Scaled-UI) note when
/// it differs enough from the raw balance to explain an overshoot, and the
/// requested raw amount. `verb` distinguishes "sell" from "send" in the
/// trailing sentence. `pub` (not `pub(crate)`) so the `send` command in the
/// `cli` crate can reuse it without duplicating the wallet-note formatting.
#[must_use]
pub fn insufficient_balance_message(
    symbol: &str,
    balance_raw: &str,
    decimals: u8,
    ui_balance: Option<f64>,
    requested_raw: &str,
    verb: &str,
) -> String {
    // Scaled-UI mints: wallets display raw × multiplier, so "sell/send what
    // Phantom shows" can exceed the raw balance — name both numbers.
    let wallet_note = ui_balance
        .filter(|ui| (ui - raw_amount_to_f64_lossy(balance_raw, decimals)).abs() > 1e-9)
        .map(|ui| format!(" (wallet displays ≈{})", format_ui_trimmed(ui)))
        .unwrap_or_default();
    format!(
        "Insufficient {symbol} balance: have {}{wallet_note}, trying to {verb} {}. Amounts are in raw tokens — use `all` or `NN%` to avoid unit mismatch.",
        amounts::format_amount(balance_raw, decimals),
        amounts::format_amount(requested_raw, decimals),
    )
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
    ui_balance: Option<f64>,
) -> Result<(String, String)> {
    let s = amount_str.trim();
    if s.eq_ignore_ascii_case("all") {
        return Ok((
            amounts::format_amount(balance_raw, decimals),
            balance_raw.to_string(),
        ));
    }
    if let Some(pct_str) = s.strip_suffix('%') {
        let pct = amounts::parse_pct(pct_str, amount_str)?;
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
            insufficient_balance_message(symbol, balance_raw, decimals, ui_balance, &raw, "sell"),
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
    fn check_slippage_fails_open_when_unmeasurable() {
        // QM-2: neither price_impact nor in/out USD values are reported —
        // calc_slippage returns None, so the -3% hard block cannot be
        // evaluated. Must fail OPEN (Ok(None), no error) rather than invent
        // a block; the policy still emits a stderr warning (not asserted
        // here) so the gap isn't silent.
        let order = jupiter::OrderResponse::default();
        let slip = check_slippage(&order, false).unwrap();
        assert_eq!(slip, None);
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
        let (d, r) = resolve_sell_amount("all", "TSLA", "1500000000", 9, None).unwrap();
        assert_eq!(r, "1500000000");
        assert_eq!(d, "1.5");

        let (_, r) = resolve_sell_amount("50%", "TSLA", "1500000000", 9, None).unwrap();
        assert_eq!(r, "750000000");

        let (d, r) = resolve_sell_amount("1", "TSLA", "1500000000", 9, None).unwrap();
        assert_eq!(r, "1000000000");
        assert_eq!(d, "1");
    }

    #[test]
    fn resolve_sell_amount_over_balance_is_typed_insufficient_funds() {
        let err = resolve_sell_amount("2", "TSLA", "1500000000", 9, None).unwrap_err();
        let te = err.downcast_ref::<GmTradeError>().expect("typed error");
        assert_eq!(te.kind, GmTradeErrorKind::InsufficientFunds);
        assert!(te.detail.contains("TSLA"), "detail: {}", te.detail);
    }

    #[test]
    fn resolve_sell_amount_rejects_bad_percentage() {
        assert!(resolve_sell_amount("150%", "TSLA", "1500000000", 9, None).is_err());
        assert!(resolve_sell_amount("-5%", "TSLA", "1500000000", 9, None).is_err());
        assert!(resolve_sell_amount("abc%", "TSLA", "1500000000", 9, None).is_err());
    }

    #[test]
    fn sell_overflow_error_names_wallet_displayed_balance() {
        // User holds 1.0 raw but the wallet displays 1.0077 — selling the
        // displayed number must explain the difference.
        let err = resolve_sell_amount("1.0077", "SPYon", "1000000000", 9, Some(1.0077))
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("have 1"), "msg: {msg}");
        assert!(msg.contains("wallet displays"), "msg: {msg}");
        assert!(msg.contains("1.0077"), "msg: {msg}");
        // No ui info → no wallet note.
        let err = resolve_sell_amount("2", "SPYon", "1000000000", 9, None).unwrap_err();
        let msg = format!("{err:#}");
        assert!(!msg.contains("wallet displays"), "msg: {msg}");
    }

    /// `send`'s overshoot error reuses the exact same message builder as
    /// `sell` (parity — see `crates/cli/src/cmd/gm/send.rs`), just with a
    /// different verb. Covers both the with-note and without-note cases.
    #[test]
    fn insufficient_balance_message_send_verb_names_wallet_displayed_balance() {
        // Balance is 1.0 raw; wallet displays 1.0077 (Scaled-UI multiplier);
        // requested_raw is itself raw units (1007700000 == 1.0077 at 9 decimals).
        let msg = insufficient_balance_message("SPYon", "1000000000", 9, Some(1.0077), "1007700000", "send");
        assert!(msg.contains("Insufficient SPYon balance"), "msg: {msg}");
        assert!(msg.contains("have 1"), "msg: {msg}");
        assert!(msg.contains("wallet displays ≈1.0077"), "msg: {msg}");
        assert!(msg.contains("trying to send 1.0077"), "msg: {msg}");
    }

    #[test]
    fn insufficient_balance_message_send_verb_omits_note_when_ui_matches_or_unknown() {
        // ui_balance unknown (e.g. Jupiter-fallback holdings source).
        let msg = insufficient_balance_message("TSLA", "1000000000", 9, None, "2000000000", "send");
        assert!(!msg.contains("wallet displays"), "msg: {msg}");
        assert!(msg.contains("trying to send 2"), "msg: {msg}");

        // ui_balance present but equal to the raw balance (no multiplier) —
        // still no note; this is the SOL/USDC/non-scaled-mint shape.
        let msg = insufficient_balance_message("TSLA", "1000000000", 9, Some(1.0), "2000000000", "send");
        assert!(!msg.contains("wallet displays"), "msg: {msg}");
    }

    #[test]
    fn insufficient_balance_message_sell_and_send_share_wallet_note_wording() {
        // Same balances, only the verb differs — proves the two call sites
        // (sell, send) share one wording rather than each formatting the
        // wallet-displayed note independently.
        let sell_msg = insufficient_balance_message("SPYon", "1000000000", 9, Some(1.0077209), "1500000000", "sell");
        let send_msg = insufficient_balance_message("SPYon", "1000000000", 9, Some(1.0077209), "1500000000", "send");
        assert_eq!(
            sell_msg.replace("sell", "send"),
            send_msg,
            "sell: {sell_msg}\nsend: {send_msg}"
        );
    }

    #[test]
    fn send_gm_token_overshoot_error_is_typed_insufficient_funds() {
        // The send command overshoot check (CLI side) now wraps insufficient
        // balance in a typed GmTradeError so --json emits error_kind parity
        // with sell. Verify classify_error labels it correctly.
        use super::super::gm::classify_error;
        let msg = insufficient_balance_message("SPYon", "1000000000", 9, Some(1.0077), "2000000000", "send");
        let err: eyre::Report = GmTradeError::new(
            GmTradeErrorKind::InsufficientFunds,
            msg,
        )
        .into();
        assert_eq!(classify_error(&err), Some("insufficient_funds"));
    }

    /// User task, end to end: "Sell my entire SPYon dividend position."
    /// `compute_portfolio` (pub API — see also `crates/ondo/tests/scenarios.rs`)
    /// prices the raw balance the wallet actually holds; `all` must resolve to
    /// that same raw amount, while typing the Phantom-displayed number instead
    /// must be refused and must name both balances. `resolve_sell_amount` is
    /// `pub(crate)`, so this composed scenario lives here rather than in the
    /// external integration-test crate.
    #[test]
    fn dividend_token_buy_to_sell_all_uses_raw_not_wallet_displayed() {
        let raw_amount = "20369073000"; // 20.369073 raw tokens, 9 decimals
        let raw_display = 20.369073_f64;
        let ui_display = 20.526341_f64; // Phantom shows raw × 1.0077209
        let asset: crate::api::OndoAsset = serde_json::from_value(serde_json::json!({
            "symbol": "SPYon",
            "assetName": "SPDR S&P 500 ETF",
            "primaryMarket": { "price": "753.99" }
        }))
        .unwrap();
        let balance = crate::solana::SolanaTokenBalance {
            symbol: "SPYon".into(),
            mint: crate::types::Mint::from("k18WJUULWheRkSpSquYGdNNmtuE2Vbw1hpuUi92ondo"),
            balance: raw_display,
            ui_balance: Some(ui_display),
            raw_amount: raw_amount.to_string(),
        };
        let summary = crate::usecases::gm::compute_portfolio(std::slice::from_ref(&balance), &[asset]);
        let pos = &summary.positions[0];
        assert!((pos.value_usd - raw_display * 753.99).abs() < 0.01, "value {}", pos.value_usd);
        assert!((pos.shares_per_token.unwrap() - 1.0077209).abs() < 1e-6);

        // Selling "all" sells the RAW amount, matching what was just priced.
        let (display, raw) = resolve_sell_amount("all", "SPYon", raw_amount, 9, Some(ui_display)).unwrap();
        assert_eq!(raw, raw_amount);
        assert_eq!(display, "20.369073");

        // Selling the wallet-displayed (ui) number instead overshoots the raw
        // balance and must fail, naming both the held and attempted amounts.
        let err = resolve_sell_amount(&ui_display.to_string(), "SPYon", raw_amount, 9, Some(ui_display))
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("20.369073"), "must name raw balance: {msg}");
        assert!(msg.contains("20.526341"), "must name attempted (wallet-displayed) amount: {msg}");
    }
}
