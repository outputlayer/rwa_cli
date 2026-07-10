use eyre::Result;
use rwa_ondo::{amounts, jupiter, solana, token_list, wallet};
use rwa_ondo::usecases::gm::{GmTradeError, GmTradeErrorKind};

use super::*;

/// USDC to reserve (keep untouched) for the pre-send auto-gas refuel. Only an
/// EXACT USDC amount leaves a well-defined remainder that auto-gas may convert
/// to SOL; a balance-relative amount (`all`/`100%`/`NN%`) has no fixed
/// remainder, so letting auto-gas divert USDC would silently shrink the send
/// (the recipient gets less than the requested share). Returns `None` for those
/// → the caller skips the refuel entirely. Pure so the decision is unit-tested.
fn usdc_gas_reservation(amount: &str) -> Option<u128> {
    // token_to_raw succeeds ONLY for an exact numeric amount; `all`/`100%`/
    // `NN%` and garbage all fail → None → skip the refuel.
    amounts::token_to_raw(amount, jupiter::USDC_DECIMALS)
        .ok()
        .and_then(|r| r.parse::<u128>().ok())
        .filter(|&r| r > 0)
}

#[allow(clippy::too_many_arguments)]
pub async fn send(token: &str, amount: &str, to: &str, yes: bool, dry_run: bool, json: bool, rpc_url: Option<&str>, selected: Option<&str>) -> Result<()> {
    let w = load_wallet(selected)?;
    let pubkey = w.pubkey();

    // Typed so agents can branch on error_kind instead of matching prose.
    solana::validate_address(to).map_err(|e| {
        eyre::Report::from(GmTradeError::new(GmTradeErrorKind::InvalidAddress, e.to_string()))
    })?;
    if to == pubkey {
        return Err(GmTradeError::new(
            GmTradeErrorKind::SelfSend,
            "Cannot send to yourself",
        )
        .into());
    }

    let token_upper = token.to_uppercase();

    // Auto-refuel SOL from USDC before a real transfer (transfers always pay
    // their own fees). Deliberately NOT for `send SOL`: buying more SOL right
    // before the user drains SOL would fight their intent.
    let (gas_refuel, _balances) = if dry_run || token_upper == "SOL" {
        (None, None)
    } else if token_upper == "USDC" {
        // Auto-gas converts USDC → SOL, which silently shrinks a balance-relative
        // send. Only reserve+refuel for an exact USDC amount; `all`/`100%`/`NN%`
        // skip the refuel (and fail cleanly later if SOL can't cover the fee).
        match usdc_gas_reservation(amount) {
            Some(reserved) => auto_gas(&w, rpc_url, yes, json, reserved).await?,
            None => (None, None),
        }
    } else {
        // GM-token send spends no USDC — auto-gas may use all of it (reserve 0).
        auto_gas(&w, rpc_url, yes, json, 0).await?
    };

    match token_upper.as_str() {
        "SOL"  => send_sol(&w, amount, to, yes, dry_run, json, rpc_url).await,
        "USDC" => send_usdc(&w, amount, to, yes, dry_run, json, rpc_url, gas_refuel).await,
        _      => send_gm_token(&w, &token_upper, amount, to, yes, dry_run, json, rpc_url, gas_refuel).await,
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
                gas_refuel: None,
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

    if !require_execution_consent(yes, json, "Proceed?")? {
        return Err(eyre::eyre!("Cancelled"));
    }

    let result = solana::transfer_sol(w, to, raw, rpc_url).await?;
    let sig = &result.signature;
    rwa_ondo::ledger::record(
        &pubkey,
        &rwa_ondo::ledger::LedgerEvent::now(Some(sig.clone()), "send_out", "SOL", &raw_str, None),
    );

    if json {
        return json_out(&SendJson {
            gas_refuel: None,
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

#[allow(clippy::too_many_arguments)]
async fn send_usdc(w: &wallet::Wallet, amount: &str, to: &str, yes: bool, dry_run: bool, json: bool, rpc_url: Option<&str>, gas_refuel: Option<GasRefuelJson>) -> Result<()> {
    let pubkey = w.pubkey();

    let sol = solana::get_sol_balance(&pubkey, rpc_url).await?;
    // USDC uses standard SPL Token — recipient may need ATA creation
    let min_sol = solana::estimate_gas_needed(true, false, rpc_url).await;
    if sol < min_sol {
        return Err(GmTradeError::new(
            GmTradeErrorKind::InsufficientFunds,
            format!("Insufficient SOL for gas (have {sol:.4}, need ≥{min_sol:.4})"),
        )
        .into());
    }

    let is_all = amount.trim().eq_ignore_ascii_case("all") || amount.trim() == "100%";
    // Fetch the balance once: serves `all`, the `NN%` resolver, and the exact
    // overshoot check below (one RPC call instead of the previous per-branch fetch).
    let (_, bal_raw_str) = solana::get_usdc_balance_raw(&pubkey, rpc_url).await?;

    let raw_str = if is_all {
        bal_raw_str.clone()
    } else {
        amounts::resolve_amount_to_raw(amount, jupiter::USDC_DECIMALS, || {
            let bal = bal_raw_str.clone();
            async move { Ok(bal) }
        }).await?
    };
    if raw_str == "0" {
        return Err(eyre::eyre!("USDC balance is 0"));
    }
    let display_amount = amounts::format_amount(&raw_str, jupiter::USDC_DECIMALS);
    let raw: u64 = raw_str.parse().map_err(|_| eyre::eyre!("Invalid USDC amount"))?;
    // Exact amounts can exceed the balance (`all`/`NN%` cannot) — fail fast with
    // a typed error instead of an opaque on-chain reject, matching send_sol/token.
    let bal_raw: u64 = bal_raw_str.parse().unwrap_or(0);
    if raw > bal_raw {
        return Err(GmTradeError::new(
            GmTradeErrorKind::InsufficientFunds,
            format!(
                "Insufficient USDC: {} USDC (need {})",
                amounts::format_amount(&bal_raw_str, jupiter::USDC_DECIMALS),
                display_amount
            ),
        )
        .into());
    }

    if !json {
        println!("Send {} USDC → {to}", display_amount);
    }

    if dry_run {
        if json {
            return json_out(&SendJson {
                gas_refuel: None,
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

    if !require_execution_consent(yes, json, "Proceed?")? {
        return Err(eyre::eyre!("Cancelled"));
    }

    let result = solana::transfer_spl(w, to, solana::USDC_MINT, raw, 6, false, rpc_url).await?;
    let sig = &result.signature;
    rwa_ondo::ledger::record(
        &pubkey,
        &rwa_ondo::ledger::LedgerEvent::now(Some(sig.clone()), "send_out", "USDC", &raw_str, None),
    );

    if json {
        return json_out(&SendJson {
            gas_refuel,
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
async fn send_gm_token(w: &wallet::Wallet, symbol: &str, amount: &str, to: &str, yes: bool, dry_run: bool, json: bool, rpc_url: Option<&str>, gas_refuel: Option<GasRefuelJson>) -> Result<()> {
    let pubkey = w.pubkey();
    let tokens = token_list::get_token_list();
    let (sym, gm_mint) = resolve_gm_mint(&rwa_ondo::types::Symbol::from(symbol), tokens)?;

    let sol = solana::get_sol_balance(&pubkey, rpc_url).await?;
    // GM tokens use Token-2022 — recipient may need ATA creation
    let min_sol = solana::estimate_gas_needed(true, true, rpc_url).await;
    if sol < min_sol {
        return Err(GmTradeError::new(
            GmTradeErrorKind::InsufficientFunds,
            format!("Insufficient SOL for gas (have {sol:.4}, need ≥{min_sol:.4})"),
        )
        .into());
    }

    // Fetched eagerly (not just for `all`/`NN%`) so an exact amount can be
    // checked against the balance locally — parity with `sell`'s
    // `resolve_sell_amount`, which names the wallet-displayed (Scaled-UI)
    // balance too when it would explain an overshoot, rather than letting
    // the transfer fail on-chain with an opaque error.
    let balance = solana::get_balance(&pubkey, &gm_mint, rpc_url).await?;
    let raw_str = amounts::resolve_amount_to_raw(amount, jupiter::GM_SOL_DECIMALS, || {
        let raw_amount = balance.raw_amount.clone();
        async move { Ok(raw_amount) }
    }).await?;

    if raw_str == "0" {
        return Err(eyre::eyre!("Balance is 0 — nothing to send"));
    }

    let raw: u64 = raw_str.parse().map_err(|_| eyre::eyre!("Invalid token amount"))?;
    let raw_balance: u64 = balance
        .raw_amount
        .parse()
        .map_err(|_| eyre::eyre!("Invalid on-chain amount: {}", balance.raw_amount))?;
    if raw > raw_balance {
        return Err(GmTradeError::new(
            GmTradeErrorKind::InsufficientFunds,
            insufficient_balance_message(
                &sym,
                &balance.raw_amount,
                jupiter::GM_SOL_DECIMALS,
                balance.ui_balance,
                &raw_str,
                "send",
            ),
        )
        .into());
    }

    let token_display = amounts::format_amount(&raw_str, jupiter::GM_SOL_DECIMALS);

    if !json {
        println!("Send {} {sym} → {to}", token_display);
    }

    if dry_run {
        if json {
            return json_out(&SendJson {
                gas_refuel: None,
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

    if !require_execution_consent(yes, json, "Proceed?")? {
        return Err(eyre::eyre!("Cancelled"));
    }

    let result = solana::transfer_spl(w, to, &gm_mint, raw, 9, true, rpc_url).await?;
    let sig = &result.signature;
    rwa_ondo::ledger::record(
        &pubkey,
        &rwa_ondo::ledger::LedgerEvent::now(Some(sig.clone()), "send_out", &sym, &raw_str, None),
    );

    if json {
        return json_out(&SendJson {
            gas_refuel,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usdc_gas_reservation_reserves_exact_but_skips_balance_relative() {
        // Exact amounts leave a well-defined remainder → reserve that raw so
        // auto-gas only spends the rest.
        assert_eq!(usdc_gas_reservation("10"), Some(10_000_000));
        assert_eq!(usdc_gas_reservation("5.5"), Some(5_500_000));
        // Balance-relative amounts have no fixed remainder — reserving would be
        // guesswork and letting auto-gas run silently shrinks the send, so skip.
        assert_eq!(usdc_gas_reservation("all"), None, "all must skip auto-gas");
        assert_eq!(usdc_gas_reservation("100%"), None, "100% must skip auto-gas");
        assert_eq!(usdc_gas_reservation("50%"), None, "NN% must skip auto-gas");
        // Zero / garbage are not exact spendable amounts → skip (send fails later).
        assert_eq!(usdc_gas_reservation("0"), None);
        assert_eq!(usdc_gas_reservation("abc"), None);
    }
}
