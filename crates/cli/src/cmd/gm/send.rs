use eyre::Result;
use rwa_ondo::{amounts, jupiter, solana, token_list};

use super::*;

pub async fn send(token: &str, amount: &str, to: &str, yes: bool, dry_run: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let w = load_wallet()?;
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
    let raw_str = resolve_sol_send_amount(amount, &balance_raw, tx_fee_lamports).await?;
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
            tx: format!("https://solscan.io/tx/{sig}"),
        });
    }
    println!("✓ Sent {} SOL → {to}", display_amount);
    println!("  https://solscan.io/tx/{sig}");
    if !result.confirmed {
        println!("  ⚠ Confirmation timed out — tx may still land. Check Solscan.");
    }
    Ok(())
}

async fn resolve_sol_send_amount(amount: &str, balance_raw: &str, tx_fee_lamports: u64) -> Result<String> {
    let amount = amount.trim();
    let is_all = amount.eq_ignore_ascii_case("all") || amount == "100%";

    let balance_lamports: u64 = balance_raw
        .parse()
        .map_err(|_| eyre::eyre!("Invalid on-chain SOL amount: {balance_raw}"))?;
    if balance_lamports <= tx_fee_lamports {
        let sol_bal = balance_lamports as f64 / 1_000_000_000.0;
        let tx_fee = tx_fee_lamports as f64 / 1_000_000_000.0;
        return Err(eyre::eyre!(
            "Balance too low to cover tx fee ({sol_bal:.6} SOL, fee ~{tx_fee:.6})"
        ));
    }

    if is_all {
        return Ok((balance_lamports - tx_fee_lamports).to_string());
    }

    let raw_str = amounts::resolve_amount_to_raw(amount, jupiter::GM_SOL_DECIMALS, || {
        let balance_raw = balance_raw.to_string();
        async move { Ok(balance_raw) }
    }).await?;
    let raw_lamports: u64 = raw_str
        .parse()
        .map_err(|_| eyre::eyre!("Invalid SOL amount: {raw_str}"))?;
    let max_send_lamports = balance_lamports - tx_fee_lamports;
    if raw_lamports > max_send_lamports {
        let sol_bal = balance_lamports as f64 / 1_000_000_000.0;
        let send_sol = raw_lamports as f64 / 1_000_000_000.0;
        let tx_fee = tx_fee_lamports as f64 / 1_000_000_000.0;
        return Err(eyre::eyre!(
            "Insufficient SOL: have {sol_bal:.6}, sending {send_sol:.6} + fee ~{tx_fee:.6}"
        ));
    }
    Ok(raw_str)
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
            tx: format!("https://solscan.io/tx/{sig}"),
        });
    }
    println!("✓ Sent {} USDC → {to}", display_amount);
    println!("  https://solscan.io/tx/{sig}");
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
                token: sym.clone(),
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
            token: sym.clone(),
            amount: token_display.clone(),
            recipient: to.into(),
            tx: format!("https://solscan.io/tx/{sig}"),
        });
    }
    println!("✓ Sent {} {sym} → {to}", token_display);
    println!("  https://solscan.io/tx/{sig}");
    if !result.confirmed {
        println!("  ⚠ Confirmation timed out — tx may still land. Check Solscan.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn resolve_sol_send_amount_all_reserves_fee_exactly() {
        let raw = resolve_sol_send_amount("all", "1000000000", 5000).await.unwrap();
        assert_eq!(raw, "999995000");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolve_sol_send_amount_percentage_uses_raw_math() {
        let raw = resolve_sol_send_amount("50%", "1000000000", 5000).await.unwrap();
        assert_eq!(raw, "500000000");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolve_sol_send_amount_hundred_percent_matches_all() {
        let all = resolve_sol_send_amount("all", "1000000000", 5000).await.unwrap();
        let pct = resolve_sol_send_amount("100%", "1000000000", 5000).await.unwrap();
        assert_eq!(pct, all);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolve_sol_send_amount_rejects_over_spend() {
        let err = resolve_sol_send_amount("1", "1000000000", 5000).await.unwrap_err();
        assert!(err.to_string().contains("Insufficient SOL"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolve_sol_send_amount_rejects_balance_below_fee() {
        let err = resolve_sol_send_amount("all", "5000", 5000).await.unwrap_err();
        assert!(err.to_string().contains("Balance too low to cover tx fee"));
    }
}
