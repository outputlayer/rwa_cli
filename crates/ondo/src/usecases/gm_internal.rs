use eyre::{Result, WrapErr, eyre};

use crate::api::{self, HaltAttestation, OndoError};
use crate::{gm, jupiter, solana, token_list};
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

pub(crate) async fn check_trading_hours() -> Result<()> {
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
    check_halt_attestation_policy(None).await?;
    Ok(())
}

/// Opt-in Headless Oracle pre-trade halt gate.
///
/// Controlled by `RWA_VERIFY_HALT`:
///   - unset (default) → skip entirely; preserves the previous behavior
///   - `1` or `soft` → block on a SUCCESSFUL signed HALTED/CLOSED/UNKNOWN
///     response; on HO network error, emit a stderr warning and proceed
///     (do not halt the trade)
///   - `strict` → also halt on HO network error / unreachability
///     (full fail-closed, matches HO's published contract)
///
/// `RWA_HALT_MIC` overrides the queried MIC (default `XNYS`).
/// `base_url` is `None` in production; tests inject an httpmock URL.
///
/// Catches real-time HALTED conditions (LULD circuit breakers, regulatory
/// halts, exchange outages) that the local NYSE-calendar `current_session()`
/// and Ondo's session-limits `tradable` flag cannot detect. The local
/// schedule can never know an exchange is *currently* halted — only that it
/// is *scheduled* to be open.
pub(crate) async fn check_halt_attestation_policy(base_url: Option<&str>) -> Result<()> {
    let Some(mode) = halt_verification_mode() else {
        return Ok(());
    };
    let mic = std::env::var("RWA_HALT_MIC").unwrap_or_else(|_| "XNYS".to_string());
    check_halt_attestation(&mic, mode, base_url).await
}

/// Pure-input version of the halt-attestation gate. Tests construct an
/// httpmock URL + chosen mode and assert behavior directly, without touching
/// process-global env vars. Production calls flow through
/// `check_halt_attestation_policy` which reads `RWA_VERIFY_HALT` / `RWA_HALT_MIC`.
pub(crate) async fn check_halt_attestation(
    mic: &str,
    mode: HaltVerificationMode,
    base_url: Option<&str>,
) -> Result<()> {
    match api::verify_market_open(mic, base_url).await {
        Ok(HaltAttestation::Open) => Ok(()),
        Ok(other) => Err(GmTradeError::new(
            GmTradeErrorKind::MarketHalted,
            format!(
                "Headless Oracle attests {mic} = {} (signed attestation overrides local schedule). \
                 Receipt source: https://headlessoracle.com — /v5/status with X-Oracle-Key=RWA_HEADLESS_KEY for production, /v5/demo keyless for eval.",
                other.label()
            ),
        )
        .into()),
        Err(e) => {
            let is_network_error = e
                .chain()
                .any(|c| c.downcast_ref::<OndoError>().is_some_and(OndoError::is_retryable));
            match (mode, is_network_error) {
                (HaltVerificationMode::Soft, true) => {
                    eprintln!(
                        "WARNING: Headless Oracle unreachable for halt verification ({e}); \
                         proceeding without it. Set RWA_VERIFY_HALT=strict to fail closed on this."
                    );
                    Ok(())
                }
                (HaltVerificationMode::Strict, true) => Err(GmTradeError::new(
                    GmTradeErrorKind::MarketHalted,
                    format!(
                        "RWA_VERIFY_HALT=strict and Headless Oracle is unreachable ({e}). \
                         Unset RWA_VERIFY_HALT or set it to `1`/`soft` to fall through on network errors."
                    ),
                )
                .into()),
                // Verification failure (bad signature, TTL expired, MIC mismatch, …) is NOT a
                // network error — it is a hard signal that something is wrong with the response.
                // Both soft and strict treat this as MarketHalted.
                (_, false) => Err(GmTradeError::new(
                    GmTradeErrorKind::MarketHalted,
                    format!("Headless Oracle attestation rejected: {e}"),
                )
                .into()),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HaltVerificationMode {
    Soft,
    Strict,
}

fn halt_verification_mode() -> Option<HaltVerificationMode> {
    parse_halt_verification_mode(std::env::var("RWA_VERIFY_HALT").ok().as_deref())
}

fn parse_halt_verification_mode(raw: Option<&str>) -> Option<HaltVerificationMode> {
    let raw = raw?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "false" | "off" | "no" => None,
        "strict" => Some(HaltVerificationMode::Strict),
        // "1", "soft", "true", "on", "yes", or anything else non-empty → soft default.
        _ => Some(HaltVerificationMode::Soft),
    }
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
    let sol = sol_lamports as f64 / 1_000_000_000.0;
    if sol < MIN_SOL_FOR_FEES {
        return Err(GmTradeError::new(
            GmTradeErrorKind::InsufficientFunds,
            format!(
                "Insufficient SOL for transaction fees: have {sol:.6} SOL, need ~{MIN_SOL_FOR_FEES} SOL."
            ),
        )
        .into());
    }
    Ok(())
}

pub(crate) async fn preflight_buy_raw(
    pubkey: &str,
    raw_usdc_amount: &str,
    rpc_url: Option<&str>,
    check_funds: bool,
) -> Result<()> {
    check_trading_hours().await?;
    let requested: u128 = raw_usdc_amount
        .parse()
        .map_err(|_| eyre!("Invalid USDC amount: {raw_usdc_amount}"))?;
    let minimum = 10u128.pow(jupiter::USDC_DECIMALS as u32) * MIN_USDC_AMOUNT as u128;
    if requested < minimum {
        return Err(GmTradeError::new(
            GmTradeErrorKind::AmountBelowMinimum,
            format!("Minimum buy amount is {MIN_USDC_AMOUNT} USDC"),
        )
        .into());
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
        .wrap_err_with(|| format!("Fund wallet: {pubkey}"))
}

pub(crate) async fn preflight_sell() -> Result<()> {
    check_trading_hours().await
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

    // ── Headless Oracle halt-attestation gate ─────────────────────────────
    //
    // Production env-driven entry (`check_halt_attestation_policy`) reads
    // `RWA_VERIFY_HALT`. Tests target the pure `check_halt_attestation`
    // entry directly to avoid process-global env-var contention between
    // parallel test threads.

    /// Live receipt for XNYS from 2026-06-08 — long expired (TTL = 60s).
    /// Serving it forces `verify_market_open` to walk the full signature
    /// verification path and then trip on TTL. The TTL error is NOT an
    /// `OndoError`, so it must be treated as a verification failure
    /// (MarketHalted under both soft and strict), not a network fall-through.
    const EXPIRED_LIVE_RECEIPT: &str = r#"{"receipt_id":"06894d3d-9ac7-41e1-9d5c-90867d195370","issued_at":"2026-06-08T11:19:09.432Z","expires_at":"2026-06-08T11:20:09.432Z","issuer":"headlessoracle.com","mic":"XNYS","status":"CLOSED","source":"SCHEDULE","halt_detection":"active","receipt_mode":"demo","schema_version":"v5.0","public_key_id":"key_2026_v1","signature":"7396c686c54d63b4738d95429ef4644ce0ba0477a2e77e0640ff960454adb65aba59191b8a256b5410ade9e7a57f3bf53dd75142fe98813061019f0730173e03"}"#;

    #[tokio::test]
    async fn halt_gate_strict_fails_closed_on_network_error() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/v5/demo");
                then.status(503).body("service unavailable");
            })
            .await;

        let err = super::check_halt_attestation(
            "XNYS",
            HaltVerificationMode::Strict,
            Some(&server.base_url()),
        )
        .await
        .expect_err("strict + network error must block");
        let typed = err
            .downcast_ref::<GmTradeError>()
            .expect("must surface as a typed GmTradeError so render_error emits market_halted");
        assert_eq!(typed.kind, GmTradeErrorKind::MarketHalted);
        assert!(typed.detail.contains("strict"), "detail must mention strict: {}", typed.detail);
    }

    #[tokio::test]
    async fn halt_gate_soft_falls_through_on_network_error() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/v5/demo");
                then.status(503).body("service unavailable");
            })
            .await;

        // Soft mode: network error must NOT block. Emits a stderr warning;
        // the test captures only the return value.
        super::check_halt_attestation(
            "XNYS",
            HaltVerificationMode::Soft,
            Some(&server.base_url()),
        )
        .await
        .expect("soft + network error must fall through and allow the trade");
    }

    #[tokio::test]
    async fn halt_gate_verification_failure_blocks_in_both_modes() {
        use httpmock::prelude::*;
        // Expired live receipt — signature verifies, but TTL fails. TTL
        // failure is NOT an OndoError, so it is a verification failure and
        // both modes must reject (it would be unsafe to fall through on a
        // signature-verifiable rejection from HO).
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/v5/demo");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(EXPIRED_LIVE_RECEIPT);
            })
            .await;

        for mode in [HaltVerificationMode::Soft, HaltVerificationMode::Strict] {
            let err = super::check_halt_attestation("XNYS", mode, Some(&server.base_url()))
                .await
                .expect_err("verification failure must block (TTL is not a network error)");
            let typed = err
                .downcast_ref::<GmTradeError>()
                .expect("must surface as typed GmTradeError");
            assert_eq!(typed.kind, GmTradeErrorKind::MarketHalted, "mode = {mode:?}");
            assert!(
                typed.detail.contains("rejected"),
                "detail must classify as verification rejection, got: {}",
                typed.detail
            );
        }
    }

    #[tokio::test]
    async fn halt_gate_rejects_signature_by_unknown_key_in_all_modes() {
        use ed25519_dalek::{Signer, SigningKey};
        use httpmock::prelude::*;
        use rand::rngs::OsRng;
        use serde_json::{Value, json};
        use std::collections::BTreeMap;

        // The "Oracle signs HALTED" success branch can only be exercised
        // end-to-end with a live HO-signed HALTED receipt — we obviously
        // can't mint one. The enum-mapping from a verified HALTED receipt
        // is covered by api::halt_attestation::tests::round_trip_sign_and_verify_with_ephemeral_key.
        // Here we assert the orthogonal security property: a HALTED receipt
        // signed by anyone OTHER than the production key is rejected as a
        // verification failure (still MarketHalted, never silently allowed).
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let receipt_no_sig = json!({
            "expires_at": "2999-01-01T00:00:00.000Z",
            "halt_detection": "active",
            "issued_at": "2999-01-01T00:00:00.000Z",
            "issuer": "headlessoracle.com",
            "mic": "XNYS",
            "public_key_id": "imposter_key",
            "receipt_id": "00000000-0000-0000-0000-000000000000",
            "receipt_mode": "live",
            "schema_version": "v5.0",
            "source": "SCHEDULE",
            "status": "HALTED",
        });
        // Canonicalize identically to api::halt_attestation::canonical_payload.
        let obj = receipt_no_sig.as_object().unwrap();
        let sorted: BTreeMap<&str, &Value> = obj.iter().map(|(k, v)| (k.as_str(), v)).collect();
        let canonical = serde_json::to_string(&sorted).unwrap();
        let signature = signing_key.sign(canonical.as_bytes());
        let mut full = receipt_no_sig;
        full.as_object_mut().unwrap().insert(
            "signature".to_string(),
            Value::String(hex::encode(signature.to_bytes())),
        );
        let body = serde_json::to_string(&full).unwrap();

        let server = MockServer::start_async().await;
        server
            .mock_async(move |when, then| {
                when.method(GET).path("/v5/demo");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(body.clone());
            })
            .await;

        // Either mode must reject — imposter signature is a verification
        // failure, NOT a network error, so both fail closed.
        for mode in [HaltVerificationMode::Soft, HaltVerificationMode::Strict] {
            let err = super::check_halt_attestation("XNYS", mode, Some(&server.base_url()))
                .await
                .expect_err("imposter-signed receipt must be rejected under all modes");
            let typed = err
                .downcast_ref::<GmTradeError>()
                .expect("must surface as typed GmTradeError");
            assert_eq!(typed.kind, GmTradeErrorKind::MarketHalted);
        }
    }

    #[test]
    fn halt_verification_mode_parsing() {
        // Pure parser — no process-env mutation, safe under parallel tests.
        let cases = [
            (None,            None),
            (Some(""),        None),
            (Some("0"),       None),
            (Some("false"),   None),
            (Some("off"),     None),
            (Some("no"),      None),
            (Some("1"),       Some(HaltVerificationMode::Soft)),
            (Some("soft"),    Some(HaltVerificationMode::Soft)),
            (Some("SOFT"),    Some(HaltVerificationMode::Soft)),
            (Some("true"),    Some(HaltVerificationMode::Soft)),
            (Some("yes"),     Some(HaltVerificationMode::Soft)),
            (Some("strict"),  Some(HaltVerificationMode::Strict)),
            (Some("STRICT "), Some(HaltVerificationMode::Strict)),
        ];
        for (raw, want) in cases {
            assert_eq!(super::parse_halt_verification_mode(raw), want, "raw={raw:?}");
        }
    }
}
