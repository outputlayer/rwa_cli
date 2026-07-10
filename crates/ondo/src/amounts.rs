use eyre::{eyre, Result};

use crate::usecases::gm::{GmTradeError, GmTradeErrorKind};

/// Typed user-input amount error → agents see `error_kind: "invalid_amount"`
/// instead of null and can branch without matching English prose.
fn invalid_amount(detail: impl Into<String>) -> eyre::Error {
    GmTradeError::new(GmTradeErrorKind::InvalidAmount, detail).into()
}

/// Convert a human-readable token amount to on-chain units with specified decimals.
pub fn token_to_raw(amount: &str, decimals: u8) -> Result<String> {
    let amount = amount.trim();
    if amount.is_empty() {
        return Err(invalid_amount("Amount must be positive"));
    }
    if amount.starts_with('-') || amount.starts_with('+') {
        return Err(invalid_amount("Amount must be positive"));
    }

    let d = decimals as usize;
    let parts: Vec<&str> = amount.split('.').collect();
    let (integer, frac) = match parts.len() {
        1 => (parts[0], ""),
        2 => (parts[0], parts[1]),
        _ => return Err(invalid_amount("Invalid amount: expected a number, `NN%`, or `all` (e.g. 100, 50%, all)")),
    };
    let integer = if integer.is_empty() { "0" } else { integer };
    if !integer.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
        return Err(invalid_amount("Invalid amount: expected a number, `NN%`, or `all` (e.g. 100, 50%, all)"));
    }
    if frac.len() > d {
        return Err(invalid_amount(format!(
            "Too many decimal places: got {}, max allowed is {d}",
            frac.len()
        )));
    }

    let frac_padded = format!("{:0<width$}", frac, width = d);
    let frac_trimmed = &frac_padded[..d];
    let raw = format!("{integer}{frac_trimmed}");
    let raw = raw.trim_start_matches('0');
    if raw.is_empty() {
        return Err(eyre!("Amount must be positive"));
    }
    Ok(raw.to_string())
}

/// Format on-chain amount to human-readable with given decimals.
#[must_use]
pub fn format_amount(raw: &str, decimals: u8) -> String {
    let d = decimals as usize;
    if raw.len() <= d {
        let zeros = d - raw.len();
        let frac = format!("{}{}", "0".repeat(zeros), raw);
        let frac = frac.trim_end_matches('0');
        if frac.is_empty() {
            "0".to_string()
        } else {
            format!("0.{frac}")
        }
    } else {
        let (integer, frac) = raw.split_at(raw.len() - d);
        let frac = frac.trim_end_matches('0');
        if frac.is_empty() {
            integer.to_string()
        } else {
            format!("{integer}.{frac}")
        }
    }
}

/// Compute `value * pct / 100` using integer math to avoid f64 precision loss.
#[must_use]
pub fn pct_of_u128(value: u128, pct: f64) -> u128 {
    let bps = (pct * 100.0).round() as u128;
    value * bps / 10_000
}

/// Resolve an amount expression (`all`, `50%`, `1.25`) to raw on-chain units.
pub async fn resolve_amount_to_raw<F, Fut>(
    raw: &str,
    decimals: u8,
    balance_raw_fn: F,
) -> Result<String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    let s = raw.trim();
    if s.eq_ignore_ascii_case("all") {
        let bal_raw = balance_raw_fn().await?;
        if bal_raw == "0" {
            return Err(eyre!("Balance is 0 — nothing to trade"));
        }
        return Ok(bal_raw);
    }
    if let Some(pct_str) = s.strip_suffix('%') {
        let pct: f64 = pct_str
            .parse()
            .map_err(|_| invalid_amount(format!("Invalid percentage: {s}")))?;
        if !(0.0..=100.0).contains(&pct) {
            return Err(invalid_amount(format!("Percentage must be 0–100, got {pct}")));
        }
        let bal_raw = balance_raw_fn().await?;
        let bal: u128 = bal_raw
            .parse()
            .map_err(|_| eyre!("Invalid on-chain amount: {bal_raw}"))?;
        if bal == 0 {
            return Err(eyre!("Balance is 0 — nothing to trade"));
        }
        return Ok(pct_of_u128(bal, pct).to_string());
    }
    token_to_raw(s, decimals)
}

/// Lamports per SOL exponent (SOL has 9 decimals).
const SOL_DECIMALS: u8 = 9;

/// SOL send-amount resolution with tx-fee reservation: `all`/`100%` sends the
/// balance minus the estimated fee; an exact amount must leave room for the
/// fee on top. The single home for the "reserve the tx fee" rule.
pub async fn resolve_sol_send_amount(
    amount: &str,
    balance_raw: &str,
    tx_fee_lamports: u64,
) -> Result<String> {
    let amount = amount.trim();
    let is_all = amount.eq_ignore_ascii_case("all") || amount == "100%";

    let balance_lamports: u64 = balance_raw
        .parse()
        .map_err(|_| eyre!("Invalid on-chain SOL amount: {balance_raw}"))?;
    if balance_lamports <= tx_fee_lamports {
        let sol_bal = balance_lamports as f64 / 1_000_000_000.0;
        let tx_fee = tx_fee_lamports as f64 / 1_000_000_000.0;
        return Err(eyre!(
            "Balance too low to cover tx fee ({sol_bal:.6} SOL, fee ~{tx_fee:.6})"
        ));
    }

    if is_all {
        return Ok((balance_lamports - tx_fee_lamports).to_string());
    }

    let raw_str = resolve_amount_to_raw(amount, SOL_DECIMALS, || {
        let balance_raw = balance_raw.to_string();
        async move { Ok(balance_raw) }
    })
    .await?;
    let raw_lamports: u64 = raw_str
        .parse()
        .map_err(|_| eyre!("Invalid SOL amount: {raw_str}"))?;
    let max_send_lamports = balance_lamports - tx_fee_lamports;
    if raw_lamports > max_send_lamports {
        let sol_bal = balance_lamports as f64 / 1_000_000_000.0;
        let send_sol = raw_lamports as f64 / 1_000_000_000.0;
        let tx_fee = tx_fee_lamports as f64 / 1_000_000_000.0;
        return Err(eyre!(
            "Insufficient SOL: have {sol_bal:.6}, sending {send_sol:.6} + fee ~{tx_fee:.6}"
        ));
    }
    Ok(raw_str)
}

#[cfg(test)]
mod sol_send_tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn all_reserves_fee_exactly() {
        let raw = resolve_sol_send_amount("all", "1000000000", 5000).await.unwrap();
        assert_eq!(raw, "999995000");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn percentage_uses_raw_math() {
        let raw = resolve_sol_send_amount("50%", "1000000000", 5000).await.unwrap();
        assert_eq!(raw, "500000000");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hundred_percent_matches_all() {
        let all = resolve_sol_send_amount("all", "1000000000", 5000).await.unwrap();
        let pct = resolve_sol_send_amount("100%", "1000000000", 5000).await.unwrap();
        assert_eq!(pct, all);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_over_spend() {
        let err = resolve_sol_send_amount("1", "1000000000", 5000).await.unwrap_err();
        assert!(err.to_string().contains("Insufficient SOL"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_balance_below_fee() {
        let err = resolve_sol_send_amount("all", "5000", 5000).await.unwrap_err();
        assert!(err.to_string().contains("Balance too low to cover tx fee"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn usdc_whole_number() {
        assert_eq!(token_to_raw("100", 6).unwrap(), "100000000");
    }

    #[test]
    fn usdc_with_decimals() {
        assert_eq!(token_to_raw("100.50", 6).unwrap(), "100500000");
    }

    #[test]
    fn token_tiny_amount() {
        assert_eq!(token_to_raw("0.000000001", 9).unwrap(), "1");
    }

    #[test]
    fn token_rejects_zero() {
        assert!(token_to_raw("0", 6).is_err());
    }

    #[test]
    fn token_rejects_negative() {
        assert!(token_to_raw("-5", 6).is_err());
    }

    #[test]
    fn token_rejects_garbage() {
        assert!(token_to_raw("abc", 6).is_err());
    }

    #[test]
    fn token_rejects_double_dot() {
        assert!(token_to_raw("1.2.3", 6).is_err());
    }

    #[test]
    fn token_rejects_extra_decimals() {
        assert!(token_to_raw("0.1234567891", 9).is_err());
    }

    #[test]
    fn format_whole_usdc() {
        assert_eq!(format_amount("100000000", 6), "100");
    }

    #[test]
    fn format_fractional_usdc() {
        assert_eq!(format_amount("100500000", 6), "100.5");
    }

    #[test]
    fn format_tiny_value() {
        assert_eq!(format_amount("1", 9), "0.000000001");
    }

    #[test]
    fn format_zero() {
        assert_eq!(format_amount("0", 6), "0");
    }

    #[test]
    fn roundtrip_many_values() {
        for (input, dec) in [("100", 6), ("0.5", 6), ("1.23", 9), ("999.999999", 6)] {
            let raw = token_to_raw(input, dec).unwrap();
            let formatted = format_amount(&raw, dec);
            assert_eq!(
                formatted, input,
                "roundtrip failed for {input} with {dec} decimals"
            );
        }
    }

    #[test]
    fn pct_33_33_of_value() {
        assert_eq!(pct_of_u128(1_000_000_000, 33.33), 333_300_000);
    }

    #[test]
    fn pct_large_value() {
        // 50% of 999_999_999_999_999_999 floors to 499_999_999_999_999_999
        // (hand-derived constant — not re-computed with the production
        // formula, which would hide a shared rounding/overflow bug).
        assert_eq!(pct_of_u128(999_999_999_999_999_999, 50.0), 499_999_999_999_999_999);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolve_exact_amount_to_raw() {
        let raw = resolve_amount_to_raw("1.25", 6, || async { Ok("999999999".to_string()) })
            .await
            .unwrap();
        assert_eq!(raw, "1250000");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolve_all_amount_to_raw() {
        let raw = resolve_amount_to_raw("all", 6, || async { Ok("42000000".to_string()) })
            .await
            .unwrap();
        assert_eq!(raw, "42000000");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolve_percentage_amount_to_raw() {
        let raw = resolve_amount_to_raw("25%", 6, || async { Ok("8000000".to_string()) })
            .await
            .unwrap();
        assert_eq!(raw, "2000000");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolve_amount_to_raw_rejects_extra_precision() {
        let err = resolve_amount_to_raw("0.1234567", 6, || async { Ok("9999999".to_string()) })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Too many decimal places"));
    }

    fn canonical(s: &str) -> String {
        match s.find('.') {
            None => {
                let t = s.trim_start_matches('0');
                if t.is_empty() {
                    "0".into()
                } else {
                    t.into()
                }
            }
            Some(dot) => {
                let int = s[..dot].trim_start_matches('0');
                let frac = s[dot + 1..].trim_end_matches('0');
                let int = if int.is_empty() { "0" } else { int };
                if frac.is_empty() {
                    int.into()
                } else {
                    format!("{int}.{frac}")
                }
            }
        }
    }

    proptest! {
        #[test]
        fn roundtrip_6_decimals(
            int in "[0-9]{1,6}",
            frac in proptest::option::of("[0-9]{0,6}")
        ) {
            let s = match frac {
                Some(f) if !f.is_empty() => format!("{int}.{f}"),
                _ => int.clone(),
            };
            if s.chars().all(|c| c == '0' || c == '.') {
                prop_assert!(token_to_raw("0", 6).is_err());
            } else {
                let expected = canonical(&s);
                let raw = token_to_raw(&expected, 6).unwrap();
                let back = format_amount(&raw, 6);
                prop_assert_eq!(back, expected);
            }
        }

        #[test]
        fn roundtrip_9_decimals(
            int in "[0-9]{1,6}",
            frac in proptest::option::of("[0-9]{0,9}")
        ) {
            let s = match frac {
                Some(f) if !f.is_empty() => format!("{int}.{f}"),
                _ => int.clone(),
            };
            if s.chars().all(|c| c == '0' || c == '.') {
                prop_assert!(token_to_raw("0", 9).is_err());
            } else {
                let expected = canonical(&s);
                let raw = token_to_raw(&expected, 9).unwrap();
                let back = format_amount(&raw, 9);
                prop_assert_eq!(back, expected);
            }
        }

        #[test]
        fn format_amount_is_canonical(raw_digits in "[1-9][0-9]{0,14}", decimals in 0u8..=9u8) {
            let out = format_amount(&raw_digits, decimals);
            prop_assert!(!out.starts_with("00"));
            prop_assert!(!out.ends_with('.'));
            prop_assert!(!out.contains("-."));
        }

        #[test]
        fn token_to_raw_rejects_non_positive(
            zeros in "[0]{1,6}",
            negative in "[1-9][0-9]{0,5}"
        ) {
            prop_assert!(token_to_raw("0", 6).is_err());
            prop_assert!(token_to_raw("-0", 6).is_err());
            prop_assert!(token_to_raw(&zeros, 6).is_err());
            let s = format!("-{negative}");
            prop_assert!(token_to_raw(&s, 6).is_err(), "should reject {s}");
        }

        #[test]
        fn token_to_raw_rejects_more_than_allowed_decimals(
            int in "[1-9][0-9]{0,5}",
            frac in "[0-9]{7,12}"
        ) {
            let s = format!("{int}.{frac}");
            prop_assert!(token_to_raw(&s, 6).is_err(), "should reject {s}");
        }

        #[test]
        fn pct_of_u128_never_exceeds_input_for_valid_percent(
            value in 1u128..=u64::MAX as u128,
            pct_bps in 0u32..=10_000u32,
        ) {
            let pct = pct_bps as f64 / 100.0;
            let result = pct_of_u128(value, pct);
            prop_assert!(result <= value);
        }
    }
}
