use eyre::Result;
use rwa_ondo::{solana, token_list};

use super::*;

pub async fn reclaim(token_filter: Option<&str>, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let w = load_wallet()?;
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

    if json {
        let status = if signatures.is_empty() && found_count > 0 {
            "error"
        } else {
            "success"
        };
        return json_out(&ReclaimJson {
            status,
            accounts_closed: signatures.len(),
            sol_reclaimed: format!("{sol_reclaimed:.9}"),
            signatures,
        });
    }

    if signatures.is_empty() && found_count > 0 {
        println!(
            "Found {} empty account(s) but all close attempts failed.",
            found_count
        );
    } else {
        println!(
            "Closed {} account(s), reclaimed {:.6} SOL",
            signatures.len(),
            sol_reclaimed
        );
    }
    for sig in &signatures {
        println!("  Tx: {}", solscan_tx_url(sig));
    }
    Ok(())
}
