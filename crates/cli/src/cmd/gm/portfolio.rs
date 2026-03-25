use eyre::Result;
use rwa_ondo::{api, solana, token_list};

use super::*;

pub async fn portfolio(wallet_addr: Option<&str>, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let tokens = token_list::get_token_list();
    let pubkey = match wallet_addr {
        Some(w) => {
            solana::validate_address(w)?;
            w.to_string()
        }
        None => load_wallet()?.pubkey(),
    };

    let (portfolio_bal, assets) = tokio::join!(
        solana::get_portfolio_balances(&pubkey, &tokens, rpc_url),
        api::fetch_assets()
    );
    let portfolio_bal = portfolio_bal?;
    let assets = assets?;
    let sol_bal = portfolio_bal.sol;
    let usdc_bal = portfolio_bal.usdc;
    let balances = portfolio_bal.gm_tokens;

    let mut positions = Vec::new();
    let mut total_value = 0.0;
    let mut total_prev_value = 0.0;

    for tb in &balances {
        let asset = api::find_asset(&tb.symbol, &assets);
        let (price, pct_24h) = match asset.and_then(|a| a.primary_market.as_ref()) {
            Some(pm) => {
                let p = api::parse_price(&pm.price);
                let pct = pm.price_change_pct_24h.as_deref().map(api::parse_price).unwrap_or(0.0);
                (p, pct)
            }
            None => (0.0, 0.0),
        };
        let value = tb.balance * price;
        let prev_value = if pct_24h.abs() > f64::EPSILON {
            value / (1.0 + pct_24h / 100.0)
        } else {
            value
        };
        total_value += value;
        total_prev_value += prev_value;
        positions.push(PositionJson {
            token: tb.symbol.clone(),
            balance: tb.balance,
            price,
            value_usd: value,
            alloc_pct: 0.0,
            change_pct_24h: pct_24h,
        });
    }

    if total_value.abs() > f64::EPSILON {
        for p in &mut positions {
            p.alloc_pct = (p.value_usd / total_value) * 100.0;
        }
    }

    positions.sort_by(|a, b| b.value_usd.partial_cmp(&a.value_usd).unwrap_or(std::cmp::Ordering::Equal));

    let total_change = total_value - total_prev_value;
    let total_pct = if total_prev_value.abs() > f64::EPSILON {
        (total_change / total_prev_value) * 100.0
    } else {
        0.0
    };

    if json {
        return json_out(&PortfolioJson {
            wallet: pubkey.clone(),
            sol: sol_bal,
            usdc: usdc_bal,
            positions,
            total_value_usd: total_value,
            change_24h_usd: total_change,
            change_24h_pct: total_pct,
        });
    }

    println!("Portfolio for {pubkey}\n");
    println!("  SOL:   {:.6}", sol_bal);
    println!("  USDC:  {:.2}", usdc_bal);

    if positions.is_empty() {
        println!("\nNo GM token positions.");
        return Ok(());
    }

    println!();
    println!(
        "{:<10} {:>12} {:>10} {:>12} {:>8} {:>8}",
        "TOKEN", "BALANCE", "PRICE", "VALUE", "ALLOC", "24h"
    );
    println!("{}", "-".repeat(64));

    for p in &positions {
        println!(
            "{:<10} {:>12.4} {:>10.2} {:>11.2} {:>7.1}% {:>+7.2}%",
            p.token, p.balance, p.price, p.value_usd, p.alloc_pct, p.change_pct_24h
        );
    }

    println!("{}", "-".repeat(64));
    println!(
        "{:<10} {:>12} {:>10} {:>11.2} {:>7} {:>+7.2}%",
        "TOTAL", "", "", total_value, "", total_pct
    );
    Ok(())
}

pub async fn history(symbol: &str, range: &str, json: bool) -> Result<()> {
    let candles = api::fetch_history(symbol, range).await?;
    if candles.is_empty() {
        return Err(eyre::eyre!("No price history for {} (range: {})", symbol, range));
    }

    let sym = symbol.to_uppercase();
    let sym = if sym.ends_with("ON") { sym } else { format!("{sym}ON") };

    let first = candles.first().unwrap();
    let last = candles.last().unwrap();
    let high = candles.iter().map(|c| c.high).fold(f64::NEG_INFINITY, f64::max);
    let low = candles.iter().map(|c| c.low).fold(f64::INFINITY, f64::min);
    let change_pct = if first.open > 0.0 {
        (last.close - first.open) / first.open * 100.0
    } else {
        0.0
    };

    if json {
        return json_out(&HistoryJson {
            symbol: sym,
            range: range.to_uppercase(),
            candles: candles.len(),
            first: HistoryCandleJson { timestamp: first.timestamp, price: first.open },
            last: HistoryCandleJson { timestamp: last.timestamp, price: last.close },
            high,
            low,
            change_pct,
        });
    }

    println!("{} Price History ({})", sym, range.to_uppercase());
    println!("{}", "-".repeat(50));
    println!("  Period:    {} candles", candles.len());
    println!("  Open:      ${:.2}", first.open);
    println!("  Close:     ${:.2}", last.close);
    println!("  High:      ${:.2}", high);
    println!("  Low:       ${:.2}", low);
    println!("  Change:    {:+.2}%", change_pct);
    Ok(())
}
