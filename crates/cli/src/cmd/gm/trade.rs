use eyre::Result;
use rwa_ondo::{types::Symbol, usecases};

use super::*;

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
    selected: Option<&str>,
) -> Result<()> {
    // `--quote-only` previews any size by skipping the funds pre-flight; it still
    // loads the wallet (its pubkey is the Jupiter swap taker) and never executes.
    // Implemented for buy only — sell amounts derive from on-chain holdings.
    let dry_run = dry_run || quote_only;
    let w = load_wallet(selected)?;
    let symbol = Symbol::from(symbol);
    let plan = usecases::gm::prepare_buy(&w, &symbol, amount, rpc_url, slippage, json, quote_only, max_bps).await?;

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
    selected: Option<&str>,
) -> Result<()> {
    let w = load_wallet(selected)?;
    let symbol = Symbol::from(symbol);
    let plan = usecases::gm::prepare_sell(&w, &symbol, amount, rpc_url, slippage, json, max_bps).await?;

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
