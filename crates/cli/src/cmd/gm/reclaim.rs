use eyre::Result;
use rwa_ondo::{solana, token_list};

use super::*;

pub async fn reclaim(token_filter: Option<&str>, json: bool, rpc_url: Option<&str>, selected: Option<&str>) -> Result<()> {
    let w = load_wallet(selected)?;
    let pubkey = w.pubkey();

    let mut empty = solana::get_empty_token_accounts(&pubkey, rpc_url).await?;

    // Filter by token if specified
    if let Some(filter) = token_filter {
        let filter_upper = filter.to_uppercase();
        let tokens = token_list::get_token_list();
        // Try to resolve symbol → mint
        let filter_mint = tokens
            .iter()
            .find(|t| {
                t.symbol.eq_ignore_ascii_case(&filter_upper)
                    || t.symbol
                        .strip_suffix("on")
                        .unwrap_or(t.symbol)
                        .eq_ignore_ascii_case(&filter_upper)
            })
            .and_then(|t| t.solana_address)
            .map(|s| s.to_string())
            .unwrap_or_else(|| filter.to_string());

        empty.retain(|a| a.mint == filter_mint);
    }

    if empty.is_empty() {
        if json {
            return json_out(&ReclaimJson {
                status: "success",
                accounts_closed: 0,
                sol_reclaimed: "0".to_string(),
                signatures: vec![],
            });
        }
        println!("No empty token accounts found — nothing to reclaim.");
        return Ok(());
    }

    let total_lamports: u64 = empty.iter().map(|a| a.lamports).sum();
    let sol_estimate = total_lamports as f64 / 1_000_000_000.0;

    if !json {
        println!(
            "Found {} empty token account(s) — ~{:.6} SOL reclaimable",
            empty.len(),
            sol_estimate
        );
    }

    let found_count = empty.len();
    let (signatures, reclaimed_lamports) =
        solana::close_empty_accounts(&w, &empty, rpc_url).await?;

    let sol_reclaimed = reclaimed_lamports as f64 / 1_000_000_000.0;
    if reclaimed_lamports > 0 {
        rwa_ondo::ledger::record(
            &pubkey,
            &rwa_ondo::ledger::LedgerEvent::now(
                signatures.first().cloned(),
                "reclaim",
                "SOL",
                &reclaimed_lamports.to_string(),
                None,
            ),
        );
    }

    // Every batch failed — surface it as an error so the exit code and the
    // central JSON envelope (status/error/error_kind) reflect the failure.
    if signatures.is_empty() && found_count > 0 {
        return Err(eyre::eyre!(
            "found {found_count} empty account(s) but all close attempts failed"
        ));
    }

    if json {
        return json_out(&ReclaimJson {
            status: "success",
            accounts_closed: signatures.len(),
            sol_reclaimed: format!("{sol_reclaimed:.9}"),
            signatures,
        });
    }

    println!(
        "Closed {} account(s), reclaimed {:.6} SOL",
        signatures.len(),
        sol_reclaimed
    );
    for sig in &signatures {
        println!("  Tx: {}", solscan_tx_url(sig));
    }
    Ok(())
}
