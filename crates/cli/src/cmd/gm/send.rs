use eyre::Result;
use rwa_ondo::{jupiter, solana, token_list};

use super::*;

pub async fn send(token: &str, amount: &str, to: &str, yes: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let w = load_wallet()?;
    let pubkey = w.pubkey();

    solana::validate_address(to)?;
    if to == pubkey {
        return Err(eyre::eyre!("Cannot send to yourself"));
    }

    let token_upper = token.to_uppercase();

    match token_upper.as_str() {
        "SOL" => send_sol(&w, amount, to, yes, json, rpc_url).await,
        "USDC" => send_usdc(&w, amount, to, yes, json, rpc_url).await,
        _ => send_gm_token(&w, &token_upper, amount, to, yes, json, rpc_url).await,
    }
}

async fn send_sol(w: &wallet::Wallet, amount: &str, to: &str, yes: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let pubkey = w.pubkey();

    let sol_bal = solana::get_sol_balance(&pubkey, rpc_url).await?;
    let is_all = amount == "all" || amount == "100%";

    let sol_f = if is_all {
        sol_bal
    } else if let Some(pct_str) = amount.strip_suffix('%') {
        let pct: f64 = pct_str.parse().map_err(|_| eyre::eyre!("Invalid percentage: {amount}"))?;
        if !(0.0..=100.0).contains(&pct) {
            return Err(eyre::eyre!("Percentage must be 0–100, got {pct}"));
        }
        sol_bal * pct / 100.0
    } else {
        amount.parse::<f64>().map_err(|_| eyre::eyre!("Invalid amount: {amount}"))?
    };

    if sol_f < MIN_SOL_FOR_GAS {
        return Err(eyre::eyre!("Amount too small — need to keep ≥{MIN_SOL_FOR_GAS} SOL for gas"));
    }

    let send_amount = if is_all {
        if sol_bal <= MIN_SOL_FOR_GAS {
            return Err(eyre::eyre!("Balance too low — need ≥{MIN_SOL_FOR_GAS} SOL for gas"));
        }
        sol_bal - MIN_SOL_FOR_GAS
    } else {
        sol_f
    };

    if !json {
        println!("Send {:.6} SOL → {to}", send_amount);
    }
    if !yes && !json && !confirm("Proceed?") {
        return Err(eyre::eyre!("Cancelled"));
    }

    let sig = solana::transfer_sol(w, to, send_amount, rpc_url).await?;

    if json {
        return json_out(&SendJson {
            status: "success",
            token: "SOL".into(),
            amount: format!("{send_amount:.6}"),
            recipient: to.into(),
            tx: format!("https://solscan.io/tx/{sig}"),
        });
    }
    println!("✓ Sent {:.6} SOL → {to}", send_amount);
    println!("  https://solscan.io/tx/{sig}");
    Ok(())
}

async fn send_usdc(w: &wallet::Wallet, amount: &str, to: &str, yes: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let pubkey = w.pubkey();

    let sol = solana::get_sol_balance(&pubkey, rpc_url).await?;
    if sol < MIN_SOL_FOR_GAS {
        return Err(eyre::eyre!("Insufficient SOL for gas (have {sol:.4}, need ≥{MIN_SOL_FOR_GAS})"));
    }

    let usdc_f = resolve_percent_amount(amount, || {
        let pk = pubkey.clone();
        let rpc = rpc_url.map(String::from);
        async move { solana::get_usdc_balance(&pk, rpc.as_deref()).await }
    }).await?;

    if usdc_f <= 0.0 {
        return Err(eyre::eyre!("USDC balance is 0"));
    }

    let raw_str = jupiter::usdc_to_raw(&format!("{usdc_f:.2}"))?;
    let raw: u64 = raw_str.parse().map_err(|_| eyre::eyre!("Invalid USDC amount"))?;

    if !json {
        println!("Send {usdc_f:.2} USDC → {to}");
    }
    if !yes && !json && !confirm("Proceed?") {
        return Err(eyre::eyre!("Cancelled"));
    }

    let sig = solana::transfer_spl(w, to, solana::USDC_MINT, raw, 6, false, rpc_url).await?;

    if json {
        return json_out(&SendJson {
            status: "success",
            token: "USDC".into(),
            amount: format!("{usdc_f:.2}"),
            recipient: to.into(),
            tx: format!("https://solscan.io/tx/{sig}"),
        });
    }
    println!("✓ Sent {usdc_f:.2} USDC → {to}");
    println!("  https://solscan.io/tx/{sig}");
    Ok(())
}

async fn send_gm_token(w: &wallet::Wallet, symbol: &str, amount: &str, to: &str, yes: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let pubkey = w.pubkey();
    let tokens = token_list::get_token_list();
    let (sym, gm_mint) = resolve_gm_mint(symbol, &tokens)?;

    let sol = solana::get_sol_balance(&pubkey, rpc_url).await?;
    if sol < MIN_SOL_FOR_GAS {
        return Err(eyre::eyre!("Insufficient SOL for gas (have {sol:.4}, need ≥{MIN_SOL_FOR_GAS})"));
    }

    let token_f = resolve_percent_amount(amount, || {
        let pk = pubkey.clone();
        let mint = gm_mint.clone();
        let rpc = rpc_url.map(String::from);
        async move {
            let b = solana::get_balance(&pk, &mint, rpc.as_deref()).await?;
            Ok(b.balance)
        }
    }).await?;

    if token_f <= 0.0 {
        return Err(eyre::eyre!("Balance is 0 — nothing to send"));
    }

    let token_str = format!("{:.9}", token_f);
    let raw_str = jupiter::token_to_raw(&token_str, jupiter::GM_SOL_DECIMALS)?;
    let raw: u64 = raw_str.parse().map_err(|_| eyre::eyre!("Invalid token amount"))?;

    if !json {
        println!("Send {token_f:.6} {sym} → {to}");
    }
    if !yes && !json && !confirm("Proceed?") {
        return Err(eyre::eyre!("Cancelled"));
    }

    let sig = solana::transfer_spl(w, to, &gm_mint, raw, 9, true, rpc_url).await?;

    if json {
        return json_out(&SendJson {
            status: "success",
            token: sym.clone(),
            amount: format!("{token_f:.6}"),
            recipient: to.into(),
            tx: format!("https://solscan.io/tx/{sig}"),
        });
    }
    println!("✓ Sent {token_f:.6} {sym} → {to}");
    println!("  https://solscan.io/tx/{sig}");
    Ok(())
}
