use eyre::{Result, WrapErr, eyre};
use rwa_ondo::{amounts, jupiter, types::Symbol, usecases};

use super::*;

/// Parse `--limit-price` into raw 10^-6 USDC-per-token units. Reuses the
/// strict amount parser: >6 decimal places are rejected, never rounded.
fn parse_limit_price(raw: Option<&str>) -> Result<Option<u128>> {
    let Some(raw) = raw else { return Ok(None) };
    let raw6 = amounts::token_to_raw(raw, jupiter::USDC_DECIMALS)
        .wrap_err("invalid --limit-price")?;
    let v: u128 = raw6.parse().wrap_err("invalid --limit-price")?;
    if v == 0 {
        return Err(eyre!("--limit-price must be greater than 0"));
    }
    Ok(Some(v))
}

/// (share_price, shares_per_token) for display — only when the multiplier is
/// known and materially differs from 1.
fn share_view(plan: &usecases::gm::SwapPlan, token_amount: &str, usdc_amount: &str) -> (Option<f64>, Option<f64>) {
    let Some(m) = plan.multiplier.filter(|m| (m - 1.0).abs() > 1e-9) else {
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

#[allow(clippy::too_many_arguments)]
pub async fn buy(
    symbol: &str,
    amount: &str,
    yes: bool,
    dry_run: bool,
    json: bool,
    rpc_url: Option<&str>,
    slippage: Option<u32>,
    quote_only: bool,
    max_bps: Option<u32>,
    limit_price: Option<&str>,
    selected: Option<&str>,
) -> Result<()> {
    let limit_price_raw6 = parse_limit_price(limit_price)?;
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
    let plan = usecases::gm::prepare_buy(&w, &symbol, amount, rpc_url, slippage, json, quote_only, max_bps, balances, limit_price_raw6).await?;
    let (share_price, shares_per_token) = share_view(&plan, &plan.amount, &plan.counter_amount);

    if !json {
        println!(
            "Getting quote for {} USDC -> {} ...",
            plan.counter_amount, plan.symbol
        );
        println!("You will receive ~{} {}", plan.amount, plan.symbol);
    }

    if dry_run {
        if json {
            return json_out(&TradeJson {
                gas_refuel: None,
                status: "dry_run",
                amount: plan.amount,
                token: plan.symbol.to_string(),
                counter_amount: plan.counter_amount,
                counter_token: "USDC",
                tx: String::new(),
                slippage_pct: plan.slippage_pct,
                actual_slippage_pct: None,
                price_impact_pct: plan.order.price_impact,
                fee_bps: plan.order.fee_bps,
                gasless: plan.order.gasless,
                router: plan.order.router.clone(),
                limit_price: limit_price.map(str::to_string),
                share_price,
                shares_per_token,
            });
        }
        println!("\n[DRY RUN] Trade not executed.");
        println!("  Would buy:   ~{} {}", plan.amount, plan.symbol);
        println!("  Would spend:  {} USDC", plan.counter_amount);
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
        if let Some(lp) = limit_price {
            // Reaching this print means check_limit_gate already passed inside prepare_*.
            println!("  Limit price:  <= {lp} USDC/token (condition met)");
        }
        if let (Some(sp), Some(m)) = (share_price, shares_per_token) {
            println!("  Per share:    ~{sp:.2} USDC  (1 token = {m:.4} shares)");
        }
        return Ok(());
    }

    if !yes && !json && !confirm("Proceed?") {
        println!("Cancelled.");
        return Ok(());
    }

    if !json {
        println!("Executing swap...");
    }
    let result = usecases::gm::execute_swap(&w, &plan, json).await?;

    if json {
        return json_out(&TradeJson {
            gas_refuel,
            status: "success",
            amount: result.output_amount,
            token: plan.symbol.to_string(),
            counter_amount: plan.counter_amount,
            counter_token: "USDC",
            tx: solscan_tx_url(&result.signature),
            slippage_pct: plan.slippage_pct,
            actual_slippage_pct: result.actual_slippage_pct,
            price_impact_pct: plan.order.price_impact,
            fee_bps: plan.order.fee_bps,
            gasless: plan.order.gasless,
            router: plan.order.router.clone(),
            limit_price: limit_price.map(str::to_string),
            share_price,
            shares_per_token,
        });
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

#[allow(clippy::too_many_arguments)]
pub async fn sell(
    symbol: &str,
    amount: &str,
    yes: bool,
    dry_run: bool,
    json: bool,
    rpc_url: Option<&str>,
    slippage: Option<u32>,
    max_bps: Option<u32>,
    limit_price: Option<&str>,
    selected: Option<&str>,
) -> Result<()> {
    let limit_price_raw6 = parse_limit_price(limit_price)?;
    let w = load_wallet(selected)?;
    let symbol = Symbol::from(symbol);
    let plan = usecases::gm::prepare_sell(&w, &symbol, amount, rpc_url, slippage, json, max_bps, limit_price_raw6).await?;
    let (share_price, shares_per_token) = share_view(&plan, &plan.amount, &plan.counter_amount);

    if !json {
        println!(
            "Getting quote for {} {} -> USDC ...",
            plan.amount, plan.symbol
        );
        println!("You will receive ~{} USDC", plan.counter_amount);
    }

    if dry_run {
        if json {
            return json_out(&TradeJson {
                gas_refuel: None,
                status: "dry_run",
                amount: plan.amount,
                token: plan.symbol.to_string(),
                counter_amount: plan.counter_amount,
                counter_token: "USDC",
                tx: String::new(),
                slippage_pct: plan.slippage_pct,
                actual_slippage_pct: None,
                price_impact_pct: plan.order.price_impact,
                fee_bps: plan.order.fee_bps,
                gasless: plan.order.gasless,
                router: plan.order.router.clone(),
                limit_price: limit_price.map(str::to_string),
                share_price,
                shares_per_token,
            });
        }
        println!("\n[DRY RUN] Trade not executed.");
        println!("  Would sell:    {} {}", plan.amount, plan.symbol);
        println!("  Would receive: ~{} USDC", plan.counter_amount);
        // Signed-cost convention: positive = costs you, negative = in your favor,
        // so Spread + Fee = Est. all-in. `slippage_pct` is favorable-positive.
        if let Some(s) = plan.slippage_pct {
            println!("  Spread/cost:   {:.4}% ({:.1} bps)", -s, -s * 100.0);
        }
        if let Some(fee) = plan.order.fee_bps {
            println!("  Jupiter fee:   {fee} bps");
        }
        if let (Some(s), Some(fee)) = (plan.slippage_pct, plan.order.fee_bps) {
            println!("  Est. all-in:   ~{:.1} bps  (− = in your favor)", fee as f64 - s * 100.0);
        }
        if let Some(lp) = limit_price {
            // Reaching this print means check_limit_gate already passed inside prepare_*.
            println!("  Limit price:   >= {lp} USDC/token (condition met)");
        }
        if let (Some(sp), Some(m)) = (share_price, shares_per_token) {
            println!("  Per share:    ~{sp:.2} USDC  (1 token = {m:.4} shares)");
        }
        return Ok(());
    }

    if !yes && !json && !confirm("Proceed?") {
        println!("Cancelled.");
        return Ok(());
    }

    if !json {
        println!("Executing swap...");
    }
    let result = usecases::gm::execute_swap(&w, &plan, json).await?;

    if json {
        return json_out(&TradeJson {
            gas_refuel: None,
            status: "success",
            amount: plan.amount,
            token: plan.symbol.to_string(),
            counter_amount: result.output_amount,
            counter_token: "USDC",
            tx: solscan_tx_url(&result.signature),
            slippage_pct: plan.slippage_pct,
            actual_slippage_pct: result.actual_slippage_pct,
            price_impact_pct: plan.order.price_impact,
            fee_bps: plan.order.fee_bps,
            gasless: plan.order.gasless,
            router: plan.order.router.clone(),
            limit_price: limit_price.map(str::to_string),
            share_price,
            shares_per_token,
        });
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
    use super::parse_limit_price;

    #[test]
    fn limit_price_parsing() {
        assert_eq!(parse_limit_price(None).unwrap(), None);
        assert_eq!(parse_limit_price(Some("400")).unwrap(), Some(400_000_000));
        assert_eq!(parse_limit_price(Some("400.50")).unwrap(), Some(400_500_000));
        // Smallest representable price: 10^-6 USDC per token.
        assert_eq!(parse_limit_price(Some("0.000001")).unwrap(), Some(1));
        // Zero, negatives, 7+ decimals, and garbage are rejected — never rounded.
        assert!(parse_limit_price(Some("0")).is_err());
        assert!(parse_limit_price(Some("-5")).is_err());
        assert!(parse_limit_price(Some("400.1234567")).is_err());
        assert!(parse_limit_price(Some("abc")).is_err());
    }
}
