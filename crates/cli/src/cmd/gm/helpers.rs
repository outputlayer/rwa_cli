//! Cross-cutting helpers shared by every gm subcommand.
//!
//! Includes wallet loading (with passphrase prompting and one-shot env-var
//! warning), JSON output, interactive y/N confirmation, mint resolution from
//! the static token list, the shared multi-item swap orchestrators used by
//! close-all and both baskets, and small string utilities used by the
//! list/search flows.

use eyre::Result;
use rwa_ondo::{usecases, wallet};
use serde::Serialize;
use std::future::Future;
use std::io::{self, Write};
use std::time::Duration;
use tokio::task::JoinSet;

use super::types::{CloseFailJson, GasRefuelJson};

/// SOL auto-refuel gate for money commands: when SOL is below the low-water
/// mark and USDC covers the operation plus 5 USDC, buy SOL first so a
/// USDC-only wallet never strands itself. Consent is an interactive prompt;
/// `-y`/`--json` auto-approve (agents opted into execution); `RWA_NO_AUTO_GAS`
/// disables entirely. Best-effort: never fails the caller's operation.
pub(super) async fn auto_gas(
    w: &wallet::Wallet,
    rpc_url: Option<&str>,
    yes: bool,
    json: bool,
    reserved_usdc_raw: u128,
) -> Result<(Option<GasRefuelJson>, Option<usecases::gm::BalanceSnapshot>)> {
    let disabled = std::env::var("RWA_NO_AUTO_GAS")
        .is_ok_and(|v| !v.trim().is_empty() && v.trim() != "0");
    if disabled {
        return Ok((None, None));
    }
    let (refuel, snapshot) = usecases::gm::ensure_gas(w, rpc_url, json, reserved_usdc_raw, |sol| {
        yes || json
            || confirm(&format!(
                "SOL is low ({sol:.6}) — buy 5 USDC of SOL for fees first?"
            ))
    })
    .await?;
    let refuel_json = refuel.map(|r| {
        let tx = solscan_tx_url(&r.signature);
        eprintln!("Gas refuel: {} USDC -> {} SOL  tx: {tx}", r.usdc_spent, r.sol_received);
        GasRefuelJson {
            usdc: r.usdc_spent,
            sol: r.sol_received,
            tx,
        }
    });
    Ok((refuel_json, snapshot))
}

/// Orchestrate multi-item swaps for close-all and both baskets — the single
/// place that owns sequential-vs-parallel semantics, so they can't drift
/// between commands.
///
/// Sequential (`parallel=false`) awaits each item with a 3-second inter-item
/// delay (Jupiter rate-limit conservatism) and prints `announce` before each.
/// Parallel (`parallel=true`) spawns a JoinSet and lets them race; swap-layer
/// concurrency stays bounded by the Jupiter semaphores inside `execute_*`.
///
/// `process` does fetch + execute for one item and returns the JSON entry plus
/// its USDC delta, or a `CloseFailJson`. Returns (succeeded, failed, total).
pub(super) async fn run_swap_items<T, J, F, Fut>(
    items: Vec<T>,
    parallel: bool,
    json: bool,
    parallel_noun: &str,
    announce: impl Fn(&T) -> String,
    process: F,
) -> (Vec<J>, Vec<CloseFailJson>, f64)
where
    T: Send + 'static,
    J: Send + 'static,
    F: Fn(T) -> Fut,
    Fut: Future<Output = std::result::Result<(J, f64), CloseFailJson>> + Send + 'static,
{
    let mut done: Vec<J> = Vec::new();
    let mut failed: Vec<CloseFailJson> = Vec::new();
    let mut total: f64 = 0.0;

    if parallel {
        if !json {
            println!("Processing {} {} in parallel...", items.len(), parallel_noun);
        }
        let mut joinset: JoinSet<std::result::Result<(J, f64), CloseFailJson>> = JoinSet::new();
        for item in items {
            joinset.spawn(process(item));
        }
        while let Some(res) = joinset.join_next().await {
            match res {
                Ok(Ok((item, value))) => {
                    done.push(item);
                    total += value;
                }
                Ok(Err(fail)) => failed.push(fail),
                Err(e) => {
                    if !json {
                        eprintln!("  ✗ join error: {}", e);
                    }
                }
            }
        }
    } else {
        for (i, item) in items.into_iter().enumerate() {
            if i > 0 {
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            if !json {
                println!("{}", announce(&item));
            }
            match process(item).await {
                Ok((item, value)) => {
                    done.push(item);
                    total += value;
                }
                Err(fail) => failed.push(fail),
            }
        }
    }

    (done, failed, total)
}

/// Fetch quotes for many items in parallel (dry-run phase shared by both
/// baskets): spawn one fetch per item, print `✓ SYM — <describe_ok>` /
/// `✗ SYM — <error>` lines, and split results into (ready, failed).
pub(super) async fn fetch_orders_parallel<I, T, F, Fut>(
    items: Vec<I>,
    json: bool,
    parallel_noun: &str,
    describe_ok: impl Fn(&T) -> String,
    fetch: F,
) -> (Vec<T>, Vec<CloseFailJson>)
where
    I: Send + 'static,
    T: Send + 'static,
    F: Fn(I) -> Fut,
    Fut: Future<Output = (String, Result<T>)> + Send + 'static,
{
    if !json {
        println!("Processing {} {} in parallel...", items.len(), parallel_noun);
    }

    let mut order_set: JoinSet<(String, Result<T>)> = JoinSet::new();
    for item in items {
        order_set.spawn(fetch(item));
    }

    let mut ready: Vec<T> = Vec::new();
    let mut failed: Vec<CloseFailJson> = Vec::new();
    while let Some(res) = order_set.join_next().await {
        match res {
            Ok((sym, Ok(order))) => {
                if !json {
                    println!("  ✓ {} — {}", sym, describe_ok(&order));
                }
                ready.push(order);
            }
            Ok((sym, Err(e))) => {
                if !json {
                    eprintln!("  ✗ {} — {}", sym, e);
                }
                failed.push(fail_json(sym, &e));
            }
            Err(e) => {
                if !json {
                    eprintln!("  ✗ join error: {}", e);
                }
            }
        }
    }

    (ready, failed)
}

pub(super) fn solscan_tx_url(sig: &str) -> String {
    format!("https://solscan.io/tx/{sig}")
}

/// Build a CloseFailJson from a token symbol and an eyre error.
/// Populates `error_kind` when the error is a known structured type.
pub(super) fn fail_json(token: String, err: &eyre::Error) -> CloseFailJson {
    CloseFailJson {
        token,
        error: err.to_string(),
        error_kind: usecases::gm::classify_error(err),
    }
}

pub(super) fn json_out(v: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(v)?);
    Ok(())
}

pub(super) fn confirm(msg: &str) -> bool {
    print!("{msg} [y/N] ");
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Load the wallet selected for this invocation (`--wallet`/`RWA_WALLET` >
/// active > legacy default). Single chokepoint for every signing command.
pub(super) fn load_wallet(selected: Option<&str>) -> Result<wallet::Wallet> {
    crate::wallets::load_selected(selected)
}

// One canonical symbol->mint resolution path for the whole workspace.
pub(super) use usecases::gm::resolve_gm_mint;

pub(super) fn clean_name(name: &str) -> String {
    name.replace(" (Ondo Tokenized)", "")
}

pub(super) fn token_type_from_name(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if n.contains("etf")
        || n.contains(" fund")
        || n.contains(" trust")
        || n.contains(" index")
        || n.contains(" shares")
    {
        "etf"
    } else {
        "stock"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_name_removes_suffix() {
        assert_eq!(clean_name("Tesla (Ondo Tokenized)"), "Tesla");
    }

    #[test]
    fn clean_name_no_suffix() {
        assert_eq!(clean_name("Apple"), "Apple");
    }

    #[test]
    fn detect_etf() {
        assert_eq!(token_type_from_name("SPDR S&P 500 ETF Trust"), "etf");
        assert_eq!(
            token_type_from_name("Vanguard Total Stock Market Index Fund"),
            "etf"
        );
    }

    #[test]
    fn detect_stock() {
        assert_eq!(token_type_from_name("Tesla"), "stock");
        assert_eq!(token_type_from_name("Apple Inc"), "stock");
    }
}
