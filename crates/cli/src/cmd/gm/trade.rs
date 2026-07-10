use eyre::{Result, WrapErr, eyre};
use rwa_ondo::{amounts, jupiter, types::Symbol, usecases};
use rwa_ondo::usecases::gm::LimitFrame;

use super::*;

/// Parse `--limit-price` into (raw 10^-6 units, frame). Accepts, case-
/// insensitively:
/// - two values in either order: `748 share`, `share 748`, `748 token`
/// - one value with the unit joined directly or by a single ` `/`:`/`-`/`_`
///   separator: `748share`, `748:share`, `748-share`, `748_share`,
///   `share:748`, `share748`, or `"748 share"` passed as one shell arg
/// - a bare number: `748` (defaults to Token — backward compatible)
///
/// Number rules unchanged: strict decimal parse, >6 decimal places rejected,
/// must be > 0.
fn parse_limit_price(raw: Option<&[String]>) -> Result<Option<(u128, LimitFrame)>> {
    let Some(raw) = raw else { return Ok(None) };
    let (number, frame) = match raw {
        [one] => split_single_limit_price(one)?,
        [a, b] => split_two_value_limit_price(a, b)?,
        _ => {
            return Err(eyre!(
                "invalid --limit-price: expected a number and an optional unit (share/token)"
            ));
        }
    };
    if number.is_empty() {
        return Err(eyre!("invalid --limit-price: missing number"));
    }
    let raw6 = amounts::token_to_raw(&number, jupiter::USDC_DECIMALS)
        .wrap_err("invalid --limit-price")?;
    let v: u128 = raw6.parse().wrap_err("invalid --limit-price")?;
    if v == 0 {
        return Err(eyre!("--limit-price must be greater than 0"));
    }
    Ok(Some((v, frame)))
}

/// Case-insensitive match on the unit words (plural accepted — rejecting
/// `shares` over an `s` taught nothing); anything else is not a unit and
/// falls through to the "unknown unit" teaching error rather than being
/// silently accepted.
fn unit_word(s: &str) -> Option<LimitFrame> {
    match s.to_ascii_lowercase().as_str() {
        "share" | "shares" => Some(LimitFrame::Share),
        "token" | "tokens" => Some(LimitFrame::Token),
        _ => None,
    }
}

/// Digits and optional decimal points, and at least one digit — just
/// enough to distinguish "this looks like the number operand" from "this
/// looks like an attempted (possibly misspelled) unit" (real validation happens in token_to_raw).
fn looks_numeric(s: &str) -> bool {
    !s.is_empty()
        && s.chars().any(|c| c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_digit() || c == '.')
}

fn split_two_value_limit_price(a: &str, b: &str) -> Result<(String, LimitFrame)> {
    match (unit_word(a), unit_word(b)) {
        (Some(_), Some(_)) => Err(eyre!(
            "invalid --limit-price: expected one number and one unit, got two units \"{a}\" and \"{b}\""
        )),
        (Some(frame), None) => Ok((b.to_string(), frame)),
        (None, Some(frame)) => Ok((a.to_string(), frame)),
        (None, None) => match (looks_numeric(a), looks_numeric(b)) {
            (true, true) => Err(eyre!(
                "invalid --limit-price: expected one number and one unit, got two numbers \"{a}\" and \"{b}\""
            )),
            (true, false) => Err(eyre!(
                "invalid --limit-price: unknown unit \"{b}\" — use share or token (e.g. --limit-price 748 share)"
            )),
            (false, true) => Err(eyre!(
                "invalid --limit-price: unknown unit \"{a}\" — use share or token (e.g. --limit-price 748 share)"
            )),
            (false, false) => Err(eyre!(
                "invalid --limit-price: expected a number and a unit (share/token), got \"{a}\" and \"{b}\" (e.g. --limit-price 748 share)"
            )),
        },
    }
}

/// A single joined value. The unit must appear at the very start or end of
/// the (lowercased) string, optionally set off by one separator char from
/// ` :-_` — this also covers a value containing a literal space when passed
/// as one shell arg (e.g. `"748 share"`).
fn split_single_limit_price(input: &str) -> Result<(String, LimitFrame)> {
    let lower = input.to_ascii_lowercase();
    const SEPS: [char; 4] = [' ', ':', '-', '_'];
    const WORDS: [(&str, LimitFrame); 2] = [("share", LimitFrame::Share), ("token", LimitFrame::Token)];

    for (word, frame) in WORDS {
        if let Some(prefix) = lower.strip_suffix(word)
            && let Some(last) = prefix.chars().last()
        {
            let number = if SEPS.contains(&last) { &prefix[..prefix.len() - 1] } else { prefix };
            if !number.is_empty() {
                return Ok((number.to_string(), frame));
            }
        }
    }
    for (word, frame) in WORDS {
        if let Some(rest) = lower.strip_prefix(word)
            && let Some(first) = rest.chars().next()
        {
            let number = if SEPS.contains(&first) { &rest[1..] } else { rest };
            if !number.is_empty() {
                return Ok((number.to_string(), frame));
            }
        }
    }
    // No recognized unit found; treat the whole value as the number (bare
    // input, or garbage that `token_to_raw` will reject with a clear error).
    Ok((lower, LimitFrame::Token))
}

/// (share_price, shares_per_token) for display — only when the multiplier is
/// known and materially differs from 1.
fn share_view(multiplier: Option<f64>, token_amount: &str, usdc_amount: &str) -> (Option<f64>, Option<f64>) {
    let Some(m) = multiplier.filter(|m| (m - 1.0).abs() > 1e-9) else {
        return (None, None);
    };
    let (Ok(tokens), Ok(usdc)) = (token_amount.parse::<f64>(), usdc_amount.parse::<f64>()) else {
        return (None, Some(m));
    };
    if tokens <= 0.0 {
        return (None, Some(m));
    }
    (Some(usdc / tokens / m), Some(m))
}

/// Build the `TradeJson` envelope from the plan plus per-call variants. The
/// four call sites (buy/sell × dry-run/success) differ ONLY in the explicit
/// arguments; the plan-derived tail (token, quote metrics, limit echo, share
/// view) is filled once here so it cannot drift between copies. Note the
/// buy/sell asymmetry the caller owns: an executed BUY's `amount` (tokens
/// received) comes from the swap result, an executed SELL's `counter_amount`
/// (USDC received) does.
#[allow(clippy::too_many_arguments)]
fn trade_json(
    plan: &usecases::gm::SwapPlan,
    status: &'static str,
    amount: String,
    counter_amount: String,
    tx: String,
    gas_refuel: Option<GasRefuelJson>,
    actual_slippage_pct: Option<f64>,
    limit_price: Option<&[String]>,
    share_price: Option<f64>,
    shares_per_token: Option<f64>,
) -> TradeJson {
    TradeJson {
        gas_refuel,
        status,
        amount,
        token: plan.symbol.to_string(),
        counter_amount,
        counter_token: "USDC",
        tx,
        slippage_pct: plan.slippage_pct,
        actual_slippage_pct,
        price_impact_pct: plan.order.price_impact,
        fee_bps: plan.order.fee_bps,
        gasless: plan.order.gasless,
        router: plan.order.router.clone(),
        limit_price: limit_price_echo(limit_price),
        share_price,
        shares_per_token,
    }
}

/// Implied price in USDC per raw token (counter ÷ token amount), display-only.
/// `None` when either side fails to parse or the token amount is zero.
fn implied_price(token_amount: &str, usdc_amount: &str) -> Option<f64> {
    let tokens = token_amount.parse::<f64>().ok()?;
    let usdc = usdc_amount.parse::<f64>().ok()?;
    (tokens > 0.0).then(|| usdc / tokens)
}

/// Trade-economics block shared by the dry-run preview and the pre-consent
/// summary: implied price, quoted spread, fee, all-in estimate, slippage
/// tolerance, per-share view. The y/N moment must show no LESS than the
/// dry-run — the human decides with the same facts either way.
fn print_economics(
    plan: &usecases::gm::SwapPlan,
    share_price: Option<f64>,
    shares_per_token: Option<f64>,
    slippage: Option<u32>,
) {
    if let Some(p) = implied_price(&plan.amount, &plan.counter_amount) {
        println!("  Price:        ~{p:.2} USDC/token");
    }
    // Signed-cost convention: positive = costs you, negative = in your favor,
    // so Spread + Fee = Est. all-in. `slippage_pct` is favorable-positive, so
    // its cost contribution is `-s`.
    if let Some(s) = plan.slippage_pct {
        println!("  Spread/cost:  {:.4}% ({:.1} bps)", -s, -s * 100.0);
    }
    if let Some(fee) = plan.order.fee_bps {
        println!("  Jupiter fee:  {fee} bps");
    }
    if let (Some(s), Some(fee)) = (plan.slippage_pct, plan.order.fee_bps) {
        println!("  Est. all-in:  ~{:.1} bps  (− = in your favor)", fee as f64 - s * 100.0);
    }
    println!(
        "  Slippage tol: {} bps (worst-case fill bound)",
        slippage.unwrap_or(usecases::gm::DEFAULT_SLIPPAGE_BPS)
    );
    if let (Some(sp), Some(m)) = (share_price, shares_per_token) {
        println!("  Per share:    ~{sp:.2} USDC  (1 token = {m:.4} shares)");
    }
}

/// Render the already-parsed limit (raw 10^-6 units + frame) as a display
/// number and unit label — independent of which input form the user typed.
fn limit_display(parsed: (u128, LimitFrame)) -> (String, &'static str) {
    let (raw, frame) = parsed;
    let number = amounts::format_amount(&raw.to_string(), jupiter::USDC_DECIMALS);
    let unit = match frame {
        LimitFrame::Share => "USDC/share",
        LimitFrame::Token => "USDC/token",
    };
    (number, unit)
}

/// JSON echo of `--limit-price` as one canonical string: two-value input
/// joins with a single space (`748 share`); single-value input echoes
/// exactly as entered.
fn limit_price_echo(raw: Option<&[String]>) -> Option<String> {
    raw.map(|v| v.join(" "))
}

#[allow(clippy::too_many_arguments)]
pub async fn buy(
    symbol: &str,
    amount: &str,
    opts: ExecOpts,
    tuning: TradeTuning,
    quote_only: bool,
    limit_price: Option<&[String]>,
    rpc_url: Option<&str>,
    selected: Option<&str>,
) -> Result<()> {
    let ExecOpts { yes, dry_run, json } = opts;
    let TradeTuning { slippage, max_bps } = tuning;
    let limit_price_parsed = parse_limit_price(limit_price)?;
    // `--quote-only` previews any size by skipping the funds pre-flight; it still
    // loads the wallet (its pubkey is the Jupiter swap taker) and never executes.
    // Implemented for buy only — sell amounts derive from on-chain holdings.
    let dry_run = dry_run || quote_only;
    let w = load_wallet(selected)?;
    // Auto-refuel SOL from USDC before a real buy (no-op when SOL is fine).
    let (gas_refuel, balances) = if dry_run {
        (None, None)
    } else {
        let reserved = amounts::token_to_raw(amount, jupiter::USDC_DECIMALS)
            .ok()
            .and_then(|r| r.parse::<u128>().ok())
            .unwrap_or(0);
        auto_gas(&w, rpc_url, yes, json, reserved).await?
    };
    let symbol = Symbol::from(symbol);
    let plan = usecases::gm::prepare_buy(&w, &symbol, amount, rpc_url, slippage, json, quote_only, max_bps, balances, limit_price_parsed).await?;
    let (share_price, shares_per_token) = share_view(plan.multiplier, &plan.amount, &plan.counter_amount);

    if !json {
        println!(
            "Getting quote for {} USDC -> {} ...",
            plan.counter_amount, plan.symbol
        );
        println!("You will receive ~{} {}", plan.amount, plan.symbol);
    }

    if dry_run {
        if json {
            return json_out(&trade_json(
                &plan, "dry_run",
                plan.amount.clone(), plan.counter_amount.clone(), String::new(),
                None, None, limit_price, share_price, shares_per_token,
            ));
        }
        println!("\n[DRY RUN] Trade not executed.");
        println!("  Would buy:   ~{} {}", plan.amount, plan.symbol);
        println!("  Would spend:  {} USDC", plan.counter_amount);
        print_economics(&plan, share_price, shares_per_token, slippage);
        if let Some(parsed) = limit_price_parsed {
            // Reaching this print means check_limit_gate already passed inside prepare_*.
            let (num, unit) = limit_display(parsed);
            println!("  Limit price:  <= {num} {unit} (condition met)");
        }
        return Ok(());
    }

    // The real-money y/N moment shows the same economics as the dry-run —
    // previously it showed only the receive amount, the least-informed prompt
    // in the whole CLI at the point of highest stakes.
    if !json && !yes {
        print_economics(&plan, share_price, shares_per_token, slippage);
        if let Some(parsed) = limit_price_parsed {
            let (num, unit) = limit_display(parsed);
            println!("  Limit price:  <= {num} {unit} (condition met)");
        }
    }

    if !require_execution_consent(yes, json, "Proceed?")? {
        println!("Cancelled.");
        return Ok(());
    }

    if !json {
        println!("Executing swap...");
    }
    let result = usecases::gm::execute_swap(&w, &plan, json).await?;

    if json {
        return json_out(&trade_json(
            &plan, "success",
            result.output_amount, plan.counter_amount.clone(), solscan_tx_url(&result.signature),
            gas_refuel, result.actual_slippage_pct, limit_price, share_price, shares_per_token,
        ));
    }

    println!("\nSwap successful!");
    println!("  Bought:    {} {}", result.output_amount, plan.symbol);
    println!("  Spent:     {} USDC", plan.counter_amount);
    if let Some(s) = plan.slippage_pct.filter(|s| s.abs() > 0.05) {
        println!("  Spread:    {s:.4}%");
    }
    println!("  Tx:        {}", solscan_tx_url(&result.signature));
    Ok(())
}

pub async fn sell(
    symbol: &str,
    amount: &str,
    opts: ExecOpts,
    tuning: TradeTuning,
    limit_price: Option<&[String]>,
    rpc_url: Option<&str>,
    selected: Option<&str>,
) -> Result<()> {
    let ExecOpts { yes, dry_run, json } = opts;
    let TradeTuning { slippage, max_bps } = tuning;
    let limit_price_parsed = parse_limit_price(limit_price)?;
    let w = load_wallet(selected)?;
    let symbol = Symbol::from(symbol);
    let plan = usecases::gm::prepare_sell(&w, &symbol, amount, rpc_url, slippage, json, max_bps, limit_price_parsed).await?;
    let (share_price, shares_per_token) = share_view(plan.multiplier, &plan.amount, &plan.counter_amount);

    if !json {
        println!(
            "Getting quote for {} {} -> USDC ...",
            plan.amount, plan.symbol
        );
        println!("You will receive ~{} USDC", plan.counter_amount);
    }

    if dry_run {
        if json {
            return json_out(&trade_json(
                &plan, "dry_run",
                plan.amount.clone(), plan.counter_amount.clone(), String::new(),
                None, None, limit_price, share_price, shares_per_token,
            ));
        }
        println!("\n[DRY RUN] Trade not executed.");
        println!("  Would sell:    {} {}", plan.amount, plan.symbol);
        println!("  Would receive: ~{} USDC", plan.counter_amount);
        print_economics(&plan, share_price, shares_per_token, slippage);
        if let Some(parsed) = limit_price_parsed {
            // Reaching this print means check_limit_gate already passed inside prepare_*.
            let (num, unit) = limit_display(parsed);
            println!("  Limit price:  >= {num} {unit} (condition met)");
        }
        return Ok(());
    }

    // Same-facts rule as buy: the y/N prompt shows the dry-run economics.
    if !json && !yes {
        print_economics(&plan, share_price, shares_per_token, slippage);
        if let Some(parsed) = limit_price_parsed {
            let (num, unit) = limit_display(parsed);
            println!("  Limit price:  >= {num} {unit} (condition met)");
        }
    }

    if !require_execution_consent(yes, json, "Proceed?")? {
        println!("Cancelled.");
        return Ok(());
    }

    if !json {
        println!("Executing swap...");
    }
    let result = usecases::gm::execute_swap(&w, &plan, json).await?;

    if json {
        return json_out(&trade_json(
            &plan, "success",
            plan.amount.clone(), result.output_amount, solscan_tx_url(&result.signature),
            None, result.actual_slippage_pct, limit_price, share_price, shares_per_token,
        ));
    }

    println!("\nSwap successful!");
    println!("  Sold:      {} {}", plan.amount, plan.symbol);
    println!("  Received:  {} USDC", result.output_amount);
    if let Some(s) = plan.slippage_pct.filter(|s| s.abs() > 0.05) {
        println!("  Spread:    {s:.4}%");
    }
    println!("  Tx:        {}", solscan_tx_url(&result.signature));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{implied_price, limit_display, limit_price_echo, parse_limit_price, share_view};

    #[test]
    fn implied_price_divides_usdc_by_tokens() {
        // 5 USDC for 0.0121 TSLA ≈ 413.2 USDC/token — the number the human
        // needs at the consent prompt without doing mental division.
        let p = implied_price("0.0121", "5").unwrap();
        assert!((p - 413.22).abs() < 0.01, "got {p}");
        // Zero/garbage token amounts must not divide.
        assert_eq!(implied_price("0", "5"), None);
        assert_eq!(implied_price("abc", "5"), None);
        assert_eq!(implied_price("1", "junk"), None);
    }

    /// Build a `Vec<String>` from string literals for `parse_limit_price`.
    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn limit_price_parsing_bare_and_joined_single_value() {
        use rwa_ondo::usecases::gm::LimitFrame::{Share, Token};
        assert_eq!(parse_limit_price(None).unwrap(), None);
        assert_eq!(parse_limit_price(Some(&v(&["400"]))).unwrap(), Some((400_000_000, Token)));
        assert_eq!(parse_limit_price(Some(&v(&["400.50"]))).unwrap(), Some((400_500_000, Token)));
        assert_eq!(parse_limit_price(Some(&v(&["748share"]))).unwrap(), Some((748_000_000, Share)));
        assert_eq!(parse_limit_price(Some(&v(&["748SHARE"]))).unwrap(), Some((748_000_000, Share)));
        assert_eq!(parse_limit_price(Some(&v(&["753token"]))).unwrap(), Some((753_000_000, Token)));
        assert_eq!(parse_limit_price(Some(&v(&["748:share"]))).unwrap(), Some((748_000_000, Share)));
        assert_eq!(parse_limit_price(Some(&v(&["748-share"]))).unwrap(), Some((748_000_000, Share)));
        assert_eq!(parse_limit_price(Some(&v(&["748_share"]))).unwrap(), Some((748_000_000, Share)));
        assert_eq!(parse_limit_price(Some(&v(&["share:748"]))).unwrap(), Some((748_000_000, Share)));
        assert_eq!(parse_limit_price(Some(&v(&["share748"]))).unwrap(), Some((748_000_000, Share)));
        // One shell arg containing a literal space.
        assert_eq!(parse_limit_price(Some(&v(&["748 share"]))).unwrap(), Some((748_000_000, Share)));
        assert_eq!(parse_limit_price(Some(&v(&["0.000001"]))).unwrap(), Some((1, Token)));
        // Rejections: zero, negative, 7+ decimals, garbage, suffix without number, misspelled unit.
        assert!(parse_limit_price(Some(&v(&["0"]))).is_err());
        assert!(parse_limit_price(Some(&v(&["0share"]))).is_err());
        assert!(parse_limit_price(Some(&v(&["-5"]))).is_err());
        assert!(parse_limit_price(Some(&v(&["400.1234567"]))).is_err());
        assert!(parse_limit_price(Some(&v(&["abc"]))).is_err());
        assert!(parse_limit_price(Some(&v(&["share"]))).is_err());
        assert!(parse_limit_price(Some(&v(&["748shares"]))).is_err());
        assert!(parse_limit_price(Some(&v(&["all"]))).is_err());
    }

    #[test]
    fn limit_price_parsing_two_values_either_order() {
        use rwa_ondo::usecases::gm::LimitFrame::{Share, Token};
        assert_eq!(parse_limit_price(Some(&v(&["748", "share"]))).unwrap(), Some((748_000_000, Share)));
        assert_eq!(parse_limit_price(Some(&v(&["share", "748"]))).unwrap(), Some((748_000_000, Share)));
        assert_eq!(parse_limit_price(Some(&v(&["748", "token"]))).unwrap(), Some((748_000_000, Token)));
        assert_eq!(parse_limit_price(Some(&v(&["token", "748"]))).unwrap(), Some((748_000_000, Token)));
        assert_eq!(parse_limit_price(Some(&v(&["748", "SHARE"]))).unwrap(), Some((748_000_000, Share)));
    }

    #[test]
    fn limit_price_parsing_two_values_rejections() {
        use rwa_ondo::usecases::gm::LimitFrame::{Share, Token};
        // Plurals are accepted as aliases (rejecting `shares` over an `s`
        // taught nothing); a genuinely unknown unit still teaches.
        assert_eq!(parse_limit_price(Some(&v(&["748", "shares"]))).unwrap(), Some((748_000_000, Share)));
        assert_eq!(parse_limit_price(Some(&v(&["748", "tokens"]))).unwrap(), Some((748_000_000, Token)));
        let err = parse_limit_price(Some(&v(&["748", "sharez"]))).unwrap_err();
        assert!(err.to_string().contains("unknown unit \"sharez\""), "{err}");
        // Two units, no number.
        assert!(parse_limit_price(Some(&v(&["share", "token"]))).is_err());
        // Two numbers, no unit.
        assert!(parse_limit_price(Some(&v(&["748", "100"]))).is_err());
        // Two garbage values (neither numeric nor unit) -> teaching example.
        let err = parse_limit_price(Some(&v(&["abc", "xyz"]))).unwrap_err();
        assert!(err.to_string().contains("e.g. --limit-price 748 share"), "{err}");
    }

    #[test]
    fn limit_display_helper() {
        use rwa_ondo::usecases::gm::LimitFrame::{Share, Token};
        assert_eq!(limit_display((748_000_000, Share)), ("748".into(), "USDC/share"));
        assert_eq!(limit_display((753_000_000, Token)), ("753".into(), "USDC/token"));
        assert_eq!(limit_display((400_500_000, Token)), ("400.5".into(), "USDC/token"));
    }

    #[test]
    fn limit_price_echo_joins_multi_value_and_passes_through_single() {
        assert_eq!(limit_price_echo(None), None);
        assert_eq!(limit_price_echo(Some(&v(&["400.50"]))), Some("400.50".to_string()));
        assert_eq!(limit_price_echo(Some(&v(&["748share"]))), Some("748share".to_string()));
        assert_eq!(limit_price_echo(Some(&v(&["748", "share"]))), Some("748 share".to_string()));
        assert_eq!(limit_price_echo(Some(&v(&["share", "748"]))), Some("share 748".to_string()));
    }

    /// Audit fix: `share_view` is the one CLI function that divides by the
    /// Scaled-UI multiplier (share price = token price / m) and it had zero
    /// coverage — dropping the `/ m` would have passed every test in the repo.
    #[test]
    fn share_view_divides_token_price_by_multiplier() {
        let m = 1.0077209;
        // Sell 1 raw SPYon for 753.99 USDC → per-share price 753.99 / m
        // ≈ 748.2131 (independently computed), NOT 753.99 (no division) and
        // NOT 759.81 (multiplied instead of divided).
        let (sp, spt) = share_view(Some(m), "1", "753.99");
        let sp = sp.expect("share price must be present for m != 1");
        assert!((sp - 748.2131).abs() < 1e-3, "share price {sp}");
        assert!((sp * m - 753.99).abs() < 1e-6, "share price must invert to token price");
        assert_eq!(spt, Some(m));

        // Multi-token amounts divide through the token quantity too:
        // 20.369073 tokens for 15358.077351 USDC → same per-share price.
        let (sp2, _) = share_view(Some(m), "20.369073", "15358.077351");
        assert!((sp2.unwrap() - 748.2131).abs() < 1e-3, "share price {sp2:?}");
    }

    #[test]
    fn share_view_omitted_when_multiplier_unknown_or_one() {
        // Unknown multiplier → nothing to show.
        assert_eq!(share_view(None, "1", "753.99"), (None, None));
        // Multiplier ≈ 1 (within 1e-9) → omitted, not shown as a redundant 1x.
        assert_eq!(share_view(Some(1.0), "1", "753.99"), (None, None));
        assert_eq!(share_view(Some(1.0 + 5e-10), "1", "753.99"), (None, None));
        // A real but small multiplier IS shown.
        let (sp, spt) = share_view(Some(1.00001), "1", "753.99");
        assert!(sp.is_some());
        assert_eq!(spt, Some(1.00001));
        // Degenerate amounts: keep the multiplier, drop the price.
        assert_eq!(share_view(Some(1.0077209), "0", "753.99"), (None, Some(1.0077209)));
        assert_eq!(share_view(Some(1.0077209), "abc", "753.99"), (None, Some(1.0077209)));
    }
}
