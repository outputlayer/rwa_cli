use eyre::Result;
use rwa_ondo::{amounts, jupiter, solana, token_list, wallet};
use rwa_ondo::usecases::gm::{GmTradeError, GmTradeErrorKind};
use rwa_ondo::wallet::SendPolicy;

use super::*;

#[derive(Debug, PartialEq)]
enum RecipientVerdict {
    Allowed,
    NotListed,
}

/// Pure policy check: active only when a non-empty policy exists.
fn recipient_gate(policy: Option<&SendPolicy>, to: &str) -> RecipientVerdict {
    match policy {
        Some(p) if !p.allowed_recipients.is_empty() && !p.permits(to) => RecipientVerdict::NotListed,
        _ => RecipientVerdict::Allowed,
    }
}

/// Anti-clipboard-swap confirmation: the human types the address's last 6 chars.
fn suffix_matches(address: &str, typed: &str) -> bool {
    let suffix: String = address.chars().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
    typed.trim() == suffix
}

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

/// Shared tail of every `send_*` variant: the "Send X → to" line, the dry-run
/// branch, the consent gate, the transfer itself, the ledger record, and the
/// success/sent output. Identical across SOL/USDC/GM-token sends, so the
/// sequence — especially "record to the ledger only AFTER a successful
/// transfer" — cannot drift between them. The preambles (balance checks,
/// amount resolution) legitimately differ and stay in the callers.
#[allow(clippy::too_many_arguments)] // flat data bundle; single call shape shared by 3 senders
async fn send_epilogue<F, Fut>(
    pubkey: &str,
    token_label: &str,
    display_amount: &str,
    raw_str: &str,
    to: &str,
    opts: ExecOpts,
    gas_refuel: Option<GasRefuelJson>,
    transfer: F,
) -> Result<()>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<solana::TransactionResult>>,
{
    if !opts.json {
        println!("Send {display_amount} {token_label} → {to}");
    }

    if opts.dry_run {
        if opts.json {
            return json_out(&SendJson {
                gas_refuel: None,
                status: "dry_run",
                token: token_label.to_string(),
                amount: display_amount.to_string(),
                recipient: to.into(),
                tx: String::new(),
            });
        }
        println!("[DRY RUN] Transfer not executed.");
        return Ok(());
    }

    if !require_execution_consent(opts.yes, opts.json, "Proceed?")? {
        return Err(eyre::eyre!("Cancelled"));
    }

    let result = transfer().await?;
    let sig = &result.signature;
    rwa_ondo::ledger::record(
        pubkey,
        &rwa_ondo::ledger::LedgerEvent::now(Some(sig.clone()), "send_out", token_label, raw_str, None),
    );

    if opts.json {
        return json_out(&SendJson {
            gas_refuel,
            status: if result.confirmed { "success" } else { "sent" },
            token: token_label.to_string(),
            amount: display_amount.to_string(),
            recipient: to.into(),
            tx: solscan_tx_url(sig),
        });
    }
    println!("✓ Sent {display_amount} {token_label} → {to}");
    println!("  {}", solscan_tx_url(sig));
    if !result.confirmed {
        println!("  ⚠ Confirmation timed out — tx may still land. Check Solscan.");
    }
    Ok(())
}

pub async fn send(token: &str, amount: &str, to: &str, opts: ExecOpts, rpc_url: Option<&str>, selected: Option<&str>) -> Result<()> {
    let (w, policy) = load_wallet_with_policy(selected)?;
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

    // Allowed-recipient policy gate: opt-in (absent/empty policy = no-op),
    // runs BEFORE any network access (including --dry-run). Non-interactive
    // callers (-y, --json, or no TTY) are refused outright; a live human at a
    // TTY gets a suffix-confirmation escape hatch instead.
    if recipient_gate(policy.as_ref(), to) == RecipientVerdict::NotListed {
        use std::io::IsTerminal;
        let interactive = !opts.yes && !opts.json && std::io::stdin().is_terminal();
        if !interactive {
            return Err(GmTradeError::new(
                GmTradeErrorKind::RecipientNotAllowed,
                format!(
                    "Recipient {to} is not in this wallet's allowed list. \
                     A human can add it: rwa keys policy allow {to}"
                ),
            )
            .into());
        }
        // Live human at a TTY: confirm by typing the address suffix (defends
        // against clipboard-swapped addresses; y/N is too easy to reflex).
        use std::io::Write as _;
        print!("Recipient is NOT in your allowed list.\nType the LAST 6 characters of {to} to proceed: ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();
        if !suffix_matches(to, &line) {
            return Err(eyre::eyre!("Cancelled — suffix mismatch."));
        }
    }

    let token_upper = token.to_uppercase();

    // Auto-refuel SOL from USDC before a real transfer (transfers always pay
    // their own fees). Deliberately NOT for `send SOL`: buying more SOL right
    // before the user drains SOL would fight their intent.
    let (gas_refuel, _balances) = if opts.dry_run || token_upper == "SOL" {
        (None, None)
    } else if token_upper == "USDC" {
        // Auto-gas converts USDC → SOL, which silently shrinks a balance-relative
        // send. Only reserve+refuel for an exact USDC amount; `all`/`100%`/`NN%`
        // skip the refuel (and fail cleanly later if SOL can't cover the fee).
        match usdc_gas_reservation(amount) {
            Some(reserved) => auto_gas(&w, rpc_url, opts.yes, opts.json, reserved).await?,
            None => (None, None),
        }
    } else {
        // GM-token send spends no USDC — auto-gas may use all of it (reserve 0).
        auto_gas(&w, rpc_url, opts.yes, opts.json, 0).await?
    };

    match token_upper.as_str() {
        "SOL"  => send_sol(&w, amount, to, opts, rpc_url).await,
        "USDC" => send_usdc(&w, amount, to, opts, rpc_url, gas_refuel).await,
        _      => send_gm_token(&w, &token_upper, amount, to, opts, rpc_url, gas_refuel).await,
    }
}

async fn send_sol(w: &wallet::Wallet, amount: &str, to: &str, opts: ExecOpts, rpc_url: Option<&str>) -> Result<()> {
    let pubkey = w.pubkey();
    let balance_raw = solana::get_sol_balance_raw(&pubkey, rpc_url).await?;
    let tx_fee_lamports = solana::estimate_tx_fee_lamports(rpc_url).await;
    let raw_str = amounts::resolve_sol_send_amount(amount, &balance_raw, tx_fee_lamports).await?;
    let display_amount = amounts::format_amount(&raw_str, jupiter::GM_SOL_DECIMALS);
    let raw: u64 = raw_str.parse().map_err(|_| eyre::eyre!("Invalid SOL amount"))?;

    send_epilogue(&pubkey, "SOL", &display_amount, &raw_str, to, opts, None, || {
        solana::transfer_sol(w, to, raw, rpc_url)
    })
    .await
}

async fn send_usdc(w: &wallet::Wallet, amount: &str, to: &str, opts: ExecOpts, rpc_url: Option<&str>, gas_refuel: Option<GasRefuelJson>) -> Result<()> {
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

    send_epilogue(&pubkey, "USDC", &display_amount, &raw_str, to, opts, gas_refuel, || {
        solana::transfer_spl(w, to, solana::USDC_MINT, raw, 6, false, rpc_url)
    })
    .await
}

async fn send_gm_token(w: &wallet::Wallet, symbol: &str, amount: &str, to: &str, opts: ExecOpts, rpc_url: Option<&str>, gas_refuel: Option<GasRefuelJson>) -> Result<()> {
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

    send_epilogue(&pubkey, &sym, &token_display, &raw_str, to, opts, gas_refuel, || {
        solana::transfer_spl(w, to, &gm_mint, raw, 9, true, rpc_url)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use rwa_ondo::wallet::SendPolicy;

    fn policy_with(addr: &str) -> SendPolicy {
        let mut p = SendPolicy::default();
        p.allow(addr, None, "2026-07-16T00:00:00Z").unwrap();
        p
    }

    #[test]
    fn gate_inactive_without_policy_or_with_empty_list() {
        assert_eq!(recipient_gate(None, "AnyAddr"), RecipientVerdict::Allowed);
        assert_eq!(recipient_gate(Some(&SendPolicy::default()), "AnyAddr"), RecipientVerdict::Allowed);
    }

    #[test]
    fn gate_permits_listed_and_flags_unlisted() {
        let p = policy_with("GoodAddr");
        assert_eq!(recipient_gate(Some(&p), "GoodAddr"), RecipientVerdict::Allowed);
        assert_eq!(recipient_gate(Some(&p), "EvilAddr"), RecipientVerdict::NotListed);
    }

    #[test]
    fn suffix_match_is_case_sensitive_last_six() {
        assert!(suffix_matches("Dn9WuqLXnBu5N4nqhPE6DgPPXWFNqYtwaZFM77qmHnW1", "qmHnW1"));
        assert!(suffix_matches("Dn9WuqLXnBu5N4nqhPE6DgPPXWFNqYtwaZFM77qmHnW1", " qmHnW1 ")); // typed input is trimmed
        assert!(!suffix_matches("Dn9WuqLXnBu5N4nqhPE6DgPPXWFNqYtwaZFM77qmHnW1", "QMHNW1"));
        assert!(!suffix_matches("Dn9WuqLXnBu5N4nqhPE6DgPPXWFNqYtwaZFM77qmHnW1", "wrong6"));
    }

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
