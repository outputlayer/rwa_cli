use eyre::{Result, eyre};

/// Price frame of a `--limit-price` value. `Token` = per raw GM token (the
/// CLI's canonical price frame, matches Ondo API and Jupiter quotes).
/// `Share` = per underlying share; converted via the Scaled-UI multiplier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitFrame {
    Token,
    Share,
}

/// Convert a framed limit into the raw-token frame the gate compares in.
/// Share frame: limit × multiplier (a share limit deliberately floats with
/// dividend accrual — the user's intent is the SHARE price). `multiplier`
/// `None` means "mint has no scaled-UI extension" (= 1); RPC FAILURES must be
/// handled by the caller BEFORE calling this (fail closed, transient).
pub(crate) fn effective_limit_raw6(
    limit: Option<(u128, LimitFrame)>,
    multiplier: Option<f64>,
) -> Result<Option<u128>> {
    match limit {
        None => Ok(None),
        Some((v, LimitFrame::Token)) => Ok(Some(v)),
        Some((v, LimitFrame::Share)) => {
            let m = multiplier.unwrap_or(1.0);
            if !m.is_finite() || m <= 0.0 {
                return Err(eyre!("invalid scaled-UI multiplier {m} for share-frame --limit-price"));
            }
            let eff = (v as f64 * m).round();
            if !eff.is_finite() || eff < 1.0 {
                return Err(eyre!("share-frame --limit-price too small after multiplier conversion"));
            }
            Ok(Some(eff as u128))
        }
    }
}

/// The `--limit-price` gate decision: `usdc_raw`/`token_raw` are the quote's
/// USDC (6 decimals) and GM-token (9 decimals) sides; `limit_raw6` is the
/// limit in 10^-6 USDC per token. Buy is violated when implied > limit,
/// sell when implied < limit; equality passes. Unparseable or zero-token
/// quotes fail closed (violated) — a degenerate quote must never trade.
/// Returns `Some((implied_price, limit_price))` in display units when
/// violated; `None` when passing or no limit was given.
pub(crate) fn limit_price_exceeded(
    is_buy: bool,
    usdc_raw: &str,
    token_raw: &str,
    limit_raw6: Option<u128>,
) -> Option<(f64, f64)> {
    let limit = limit_raw6?;
    let limit_display = limit as f64 / 1e6;
    let (Ok(usdc), Ok(token)) = (usdc_raw.parse::<u128>(), token_raw.parse::<u128>()) else {
        return Some((f64::NAN, limit_display));
    };
    if token == 0 {
        return Some((f64::INFINITY, limit_display));
    }
    // implied (USDC/token) = (usdc/1e6) / (token/1e9) = usdc * 1e3 / token.
    // Integer comparison: implied <= limit  ⟺  usdc * 1e9 <= limit * token.
    let lhs = usdc.saturating_mul(1_000_000_000);
    let rhs = limit.saturating_mul(token);
    let violated = if is_buy { lhs > rhs } else { lhs < rhs };
    violated.then(|| ((usdc as f64) * 1e3 / (token as f64), limit_display))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_price_gate_decision() {
        // 100 USDC (6 dec) for 0.25 token (9 dec) → implied 400 USDC/token.
        let usdc = "100000000";
        let token = "250000000";
        // No limit → no gate.
        assert_eq!(limit_price_exceeded(true, usdc, token, None), None);
        // Buy: implied 400 ≤ limit 400 (equality) → passes.
        assert_eq!(limit_price_exceeded(true, usdc, token, Some(400_000_000)), None);
        // Buy: implied 400 > limit 399.999999 → violated.
        let (implied, limit) =
            limit_price_exceeded(true, usdc, token, Some(399_999_999)).unwrap();
        assert!((implied - 400.0).abs() < 1e-9);
        assert!((limit - 399.999999).abs() < 1e-9);
        // Buy: implied 400 ≤ limit 400.000001 → passes.
        assert_eq!(limit_price_exceeded(true, usdc, token, Some(400_000_001)), None);
        // Sell: implied 400 ≥ limit 400 (equality) → passes.
        assert_eq!(limit_price_exceeded(false, usdc, token, Some(400_000_000)), None);
        // Sell: implied 400 < limit 400.000001 → violated.
        assert!(limit_price_exceeded(false, usdc, token, Some(400_000_001)).is_some());
        // Sell: implied 400 ≥ limit 399.999999 → passes.
        assert_eq!(limit_price_exceeded(false, usdc, token, Some(399_999_999)), None);
        // Degenerate quote (zero token side) fails closed.
        assert!(limit_price_exceeded(true, usdc, "0", Some(400_000_000)).is_some());
        // Unparseable raw amounts fail closed.
        assert!(limit_price_exceeded(true, "garbage", token, Some(400_000_000)).is_some());
    }

    #[test]
    fn effective_limit_converts_share_frame_via_multiplier() {
        use LimitFrame::*;
        // No limit / token frame: passthrough, multiplier irrelevant.
        assert_eq!(effective_limit_raw6(None, None).unwrap(), None);
        assert_eq!(effective_limit_raw6(Some((753_000_000, Token)), None).unwrap(), Some(753_000_000));
        // Share frame: limit × multiplier (748 share × 1.0077209 ≈ 753.775233 token).
        let eff = effective_limit_raw6(Some((748_000_000, Share)), Some(1.0077209)).unwrap().unwrap();
        assert!((eff as i128 - 753_775_233).abs() <= 1, "eff {eff}");
        // Share frame, mint has no extension (None) → multiplier 1.
        assert_eq!(effective_limit_raw6(Some((748_000_000, Share)), None).unwrap(), Some(748_000_000));
        // Degenerate multiplier fails closed.
        assert!(effective_limit_raw6(Some((748_000_000, Share)), Some(0.0)).is_err());
        assert!(effective_limit_raw6(Some((748_000_000, Share)), Some(f64::NAN)).is_err());
    }
}
