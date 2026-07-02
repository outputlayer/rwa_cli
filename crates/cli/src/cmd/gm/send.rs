use eyre::Result;
use rwa_ondo::{amounts, jupiter, solana, token_list, wallet};

use super::*;

#[allow(clippy::too_many_arguments)]
pub async fn send(token: &str, amount: &str, to: &str, yes: bool, dry_run: bool, json: bool, rpc_url: Option<&str>, selected: Option<&str>) -> Result<()> {
    let w = load_wallet(selected)?;
    let pubkey = w.pubkey();

    solana::validate_address(to)?;
    if to == pubkey {
        return Err(eyre::eyre!("Cannot send to yourself"));
    }

    let token_upper = token.to_uppercase();

    match token_upper.as_str() {
        "SOL"  => send_sol(&w, amount, to, yes, dry_run, json, rpc_url).await,
        "USDC" => send_usdc(&w, amount, to, yes, dry_run, json, rpc_url).await,
        _      => send_gm_token(&w, &token_upper, amount, to, yes, dry_run, json, rpc_url).await,
    }
}

async fn send_sol(w: &wallet::Wallet, amount: &str, to: &str, yes: bool, dry_run: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let pubkey = w.pubkey();
    let balance_raw = solana::get_sol_balance_raw(&pubkey, rpc_url).await?;
    let tx_fee_lamports = solana::estimate_tx_fee_lamports(rpc_url).await;
    let raw_str = amounts::resolve_sol_send_amount(amount, &balance_raw, tx_fee_lamports).await?;
    let display_amount = amounts::format_amount(&raw_str, jupiter::GM_SOL_DECIMALS);
    let raw: u64 = raw_str.parse().map_err(|_| eyre::eyre!("Invalid SOL amount"))?;

    if !json {
        println!("Send {} SOL → {to}", display_amount);
    }

    if dry_run {
        if json {
            return json_out(&SendJson {
                status: "dry_run",
                token: "SOL".into(),
                amount: display_amount.clone(),
                recipient: to.into(),
                tx: String::new(),
            });
        }
        println!("[DRY RUN] Transfer not executed.");
        return Ok(());
    }

    if !yes && !json && !confirm("Proceed?") {
        return Err(eyre::eyre!("Cancelled"));
    }

    let result = solana::transfer_sol(w, to, raw, rpc_url).await?;
    let sig = &result.signature;

    if json {
        return json_out(&SendJson {
            status: if result.confirmed { "success" } else { "sent" },
            token: "SOL".into(),
            amount: display_amount.clone(),
            recipient: to.into(),
            tx: solscan_tx_url(sig),
        });
    }
    println!("✓ Sent {} SOL → {to}", display_amount);
    println!("  {}", solscan_tx_url(sig));
    if !result.confirmed {
        println!("  ⚠ Confirmation timed out — tx may still land. Check Solscan.");
    }
    Ok(())
}

async fn send_usdc(w: &wallet::Wallet, amount: &str, to: &str, yes: bool, dry_run: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let pubkey = w.pubkey();

    let sol = solana::get_sol_balance(&pubkey, rpc_url).await?;
    // USDC uses standard SPL Token — recipient may need ATA creation
    let min_sol = solana::estimate_gas_needed(true, false, rpc_url).await;
    if sol < min_sol {
        return Err(eyre::eyre!("Insufficient SOL for gas (have {sol:.4}, need ≥{min_sol:.4})"));
    }

    let is_all = amount.trim().eq_ignore_ascii_case("all") || amount.trim() == "100%";

    let raw_str = if is_all {
        let (_, raw) = solana::get_usdc_balance_raw(&pubkey, rpc_url).await?;
        raw
    } else {
        amounts::resolve_amount_to_raw(amount, jupiter::USDC_DECIMALS, || {
            let pk = pubkey.clone();
            let rpc = rpc_url.map(String::from);
            async move {
                let (_, raw) = solana::get_usdc_balance_raw(&pk, rpc.as_deref()).await?;
                Ok(raw)
            }
        }).await?
    };
    if raw_str == "0" {
        return Err(eyre::eyre!("USDC balance is 0"));
    }
    let display_amount = amounts::format_amount(&raw_str, jupiter::USDC_DECIMALS);
    let raw: u64 = raw_str.parse().map_err(|_| eyre::eyre!("Invalid USDC amount"))?;

    if !json {
        println!("Send {} USDC → {to}", display_amount);
    }

    if dry_run {
        if json {
            return json_out(&SendJson {
                status: "dry_run",
                token: "USDC".into(),
                amount: display_amount.clone(),
                recipient: to.into(),
                tx: String::new(),
            });
        }
        println!("[DRY RUN] Transfer not executed.");
        return Ok(());
    }

    if !yes && !json && !confirm("Proceed?") {
        return Err(eyre::eyre!("Cancelled"));
    }

    let result = solana::transfer_spl(w, to, solana::USDC_MINT, raw, 6, false, rpc_url).await?;
    let sig = &result.signature;

    if json {
        return json_out(&SendJson {
            status: if result.confirmed { "success" } else { "sent" },
            token: "USDC".into(),
            amount: display_amount.clone(),
            recipient: to.into(),
            tx: solscan_tx_url(sig),
        });
    }
    println!("✓ Sent {} USDC → {to}", display_amount);
    println!("  {}", solscan_tx_url(sig));
    if !result.confirmed {
        println!("  ⚠ Confirmation timed out — tx may still land. Check Solscan.");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn send_gm_token(w: &wallet::Wallet, symbol: &str, amount: &str, to: &str, yes: bool, dry_run: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let pubkey = w.pubkey();
    let tokens = token_list::get_token_list();
    let (sym, gm_mint) = resolve_gm_mint(symbol, tokens)?;

    let sol = solana::get_sol_balance(&pubkey, rpc_url).await?;
    // GM tokens use Token-2022 — recipient may need ATA creation
    let min_sol = solana::estimate_gas_needed(true, true, rpc_url).await;
    if sol < min_sol {
        return Err(eyre::eyre!("Insufficient SOL for gas (have {sol:.4}, need ≥{min_sol:.4})"));
    }

    let raw_str = amounts::resolve_amount_to_raw(amount, jupiter::GM_SOL_DECIMALS, || {
        let pk = pubkey.clone();
        let mint = gm_mint.clone();
        let rpc = rpc_url.map(String::from);
        async move {
            let b = solana::get_balance(&pk, &mint, rpc.as_deref()).await?;
            Ok(b.raw_amount)
        }
    }).await?;

    if raw_str == "0" {
        return Err(eyre::eyre!("Balance is 0 — nothing to send"));
    }

    let token_display = amounts::format_amount(&raw_str, jupiter::GM_SOL_DECIMALS);
    let raw: u64 = raw_str.parse().map_err(|_| eyre::eyre!("Invalid token amount"))?;

    if !json {
        println!("Send {} {sym} → {to}", token_display);
    }

    if dry_run {
        if json {
            return json_out(&SendJson {
                status: "dry_run",
                token: sym.to_string(),
                amount: token_display.clone(),
                recipient: to.into(),
                tx: String::new(),
            });
        }
        println!("[DRY RUN] Transfer not executed.");
        return Ok(());
    }

    if !yes && !json && !confirm("Proceed?") {
        return Err(eyre::eyre!("Cancelled"));
    }

    let result = solana::transfer_spl(w, to, &gm_mint, raw, 9, true, rpc_url).await?;
    let sig = &result.signature;

    if json {
        return json_out(&SendJson {
            status: if result.confirmed { "success" } else { "sent" },
            token: sym.to_string(),
            amount: token_display.clone(),
            recipient: to.into(),
            tx: solscan_tx_url(sig),
        });
    }
    println!("✓ Sent {} {sym} → {to}", token_display);
    println!("  {}", solscan_tx_url(sig));
    if !result.confirmed {
        println!("  ⚠ Confirmation timed out — tx may still land. Check Solscan.");
    }
    Ok(())
}
