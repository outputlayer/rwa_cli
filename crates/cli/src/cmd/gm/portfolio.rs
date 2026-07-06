use eyre::Result;
use rwa_ondo::{api, solana, token_list, usecases};

use super::*;

pub async fn portfolio(wallet_addr: Option<&str>, json: bool, rpc_url: Option<&str>, selected: Option<&str>) -> Result<()> {
    let tokens = token_list::get_token_list();
    let pubkey = match wallet_addr {
        Some(w) => {
            solana::validate_address(w)?;
            w.to_string()
        }
        None => load_wallet(selected)?.pubkey(),
    };

    let (portfolio_bal, assets) = tokio::join!(
        usecases::gm::fetch_portfolio_balances(&pubkey, tokens, rpc_url),
        api::fetch_assets()
    );
    let portfolio_bal = portfolio_bal?;
    let assets = assets?;
    let balance_source = portfolio_bal.source;
    let sol_bal = portfolio_bal.sol;
    let usdc_bal = portfolio_bal.usdc;

    let summary = usecases::gm::compute_portfolio(&portfolio_bal.gm_tokens, &assets);
    if !json {
        for u in &summary.unavailable {
            eprintln!("  Skipping {} — {}", u.symbol, u.reason);
        }
    }
    let positions: Vec<PositionJson> = summary
        .positions
        .into_iter()
        .map(|p| PositionJson {
            token: p.token,
            balance: p.balance,
            price: p.price,
            value_usd: p.value_usd,
            gm_alloc_pct: p.gm_alloc_pct,
            change_pct_24h: p.change_pct_24h,
            shares_per_token: p.shares_per_token,
        })
        .collect();
    let unavailable: Vec<PortfolioUnavailableJson> = summary
        .unavailable
        .into_iter()
        .map(|u| PortfolioUnavailableJson {
            symbol: u.symbol,
            reason: u.reason,
        })
        .collect();
    let gm_positions_value = summary.value_usd;
    let gm_positions_change = summary.change_24h_usd;
    let gm_positions_change_pct = summary.change_24h_pct;

    let source = if balance_source == solana::BalanceSource::Jupiter {
        if !json {
            eprintln!("Note: Solana RPC unavailable — balances via Jupiter holdings.");
        }
        Some("jupiter")
    } else {
        None
    };

    if json {
        return json_out(&PortfolioJson {
            wallet: pubkey.clone(),
            cash: PortfolioCashJson {
                sol: sol_bal,
                usdc: usdc_bal,
            },
            gm_positions: PortfolioGmPositionsJson {
                positions,
                value_usd: gm_positions_value,
                change_24h_usd: gm_positions_change,
                change_24h_pct: gm_positions_change_pct,
            },
            unavailable,
            source,
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
        "TOKEN", "BALANCE", "PRICE", "VALUE", "GM %", "24h"
    );
    println!("{}", "-".repeat(64));

    for p in &positions {
        println!(
            "{:<10} {:>12.4} {:>10.2} {:>11.2} {:>7.1}% {:>+7.2}%",
            p.token, p.balance, p.price, p.value_usd, p.gm_alloc_pct, p.change_pct_24h
        );
    }

    println!("{}", "-".repeat(64));
    println!(
        "{:<10} {:>12} {:>10} {:>11.2} {:>7} {:>+7.2}%",
        "GM TOTAL", "", "", gm_positions_value, "", gm_positions_change_pct
    );
    println!("  Cash balances shown above are separate from GM position totals.");
    Ok(())
}

pub async fn history(symbol: &str, range: &str, json: bool) -> Result<()> {
    let candles = api::fetch_history(symbol, range).await?;
    if candles.is_empty() {
        return Err(eyre::eyre!(
            "No price history for {} (range: {})",
            symbol,
            range
        ));
    }

    let upper = symbol.to_uppercase();
    let sym = if upper.ends_with("ON") {
        // Preserve "on" suffix casing: "TSLAon" not "TSLAON"
        let base = &upper[..upper.len() - 2];
        format!("{base}on")
    } else {
        format!("{upper}on")
    };

    let first = candles
        .first()
        .ok_or_else(|| eyre::eyre!("Empty candle data"))?;
    let last = candles
        .last()
        .ok_or_else(|| eyre::eyre!("Empty candle data"))?;
    let high = candles
        .iter()
        .map(|c| c.high)
        .fold(f64::NEG_INFINITY, f64::max);
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
            first: HistoryCandleJson {
                timestamp: first.timestamp,
                price: first.open,
            },
            last: HistoryCandleJson {
                timestamp: last.timestamp,
                price: last.close,
            },
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
