//! Per-wallet trade ledger — the local source for cost basis and P&L.
//!
//! One JSON line per completed money operation, appended to
//! `<config_dir>/rwa/ledger/<pubkey>.jsonl` — one file per wallet so named
//! wallets never mix. Only operations the CLI itself executed are recorded
//! (successful ones; failures live in the audit log). Amounts are raw
//! on-chain units so the P&L math never touches floats.
//!
//! Writes are best-effort: a trade never fails because the ledger is
//! unwritable. Reads skip corrupt lines instead of failing the command.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One completed money operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEvent {
    /// ISO 8601 UTC timestamp.
    pub ts: String,
    /// Solana transaction signature (dedup key for a future on-chain sync).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
    /// "buy" | "sell" | "gas_refuel" | "send_out" | "reclaim".
    pub kind: String,
    /// Token symbol the event is about ("TSLAon", "USDC", "SOL").
    pub token: String,
    /// Raw on-chain amount of `token` that moved (bought/sold/sent/reclaimed).
    pub qty_raw: String,
    /// Counter-leg in raw USDC for swaps (spent on buys, received on sells);
    /// absent for transfers/reclaims.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usdc_raw: Option<String>,
}

impl LedgerEvent {
    pub fn now(
        sig: Option<String>,
        kind: &str,
        token: &str,
        qty_raw: &str,
        usdc_raw: Option<String>,
    ) -> Self {
        Self {
            ts: chrono::Utc::now().to_rfc3339(),
            sig,
            kind: kind.to_string(),
            token: token.to_string(),
            qty_raw: qty_raw.to_string(),
            usdc_raw,
        }
    }
}

/// Best-effort append for wallet `taker`. Errors are swallowed — the ledger
/// must never fail the operation it records.
pub fn record(taker: &str, event: &LedgerEvent) {
    if let Some(dir) = ledger_dir() {
        let _ = record_at(&dir, taker, event);
    }
}

/// All recorded events for wallet `taker`, oldest first. Missing file reads
/// as empty; corrupt lines are skipped.
#[must_use]
pub fn read_all(taker: &str) -> Vec<LedgerEvent> {
    ledger_dir()
        .map(|dir| read_all_at(&dir, taker))
        .unwrap_or_default()
}

pub(crate) fn record_at(dir: &Path, taker: &str, event: &LedgerEvent) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let line = serde_json::to_string(event)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{taker}.jsonl")))?;
    writeln!(file, "{line}")?;
    Ok(())
}

#[must_use]
pub(crate) fn read_all_at(dir: &Path, taker: &str) -> Vec<LedgerEvent> {
    let Ok(text) = std::fs::read_to_string(dir.join(format!("{taker}.jsonl"))) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn ledger_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("rwa").join("ledger"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_appends_and_reads_in_order() {
        let dir = std::env::temp_dir().join(format!("rwa-ledger-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let taker = "WalletAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

        let e1 = LedgerEvent::now(Some("sig1".into()), "buy", "TSLAon", "11786465", Some("5000000".into()));
        let e2 = LedgerEvent::now(Some("sig2".into()), "sell", "TSLAon", "11786465", Some("4991915".into()));
        record_at(&dir, taker, &e1).unwrap();
        record_at(&dir, taker, &e2).unwrap();

        let events = read_all_at(&dir, taker);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "buy");
        assert_eq!(events[0].usdc_raw.as_deref(), Some("5000000"));
        assert_eq!(events[1].kind, "sell");

        // Other wallets don't see these events.
        assert!(read_all_at(&dir, "OtherWallet").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_lines_are_skipped_not_fatal() {
        let dir = std::env::temp_dir().join(format!("rwa-ledger-corrupt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let taker = "W";
        std::fs::write(
            dir.join("W.jsonl"),
            "{\"ts\":\"t\",\"kind\":\"buy\",\"token\":\"TSLAon\",\"qty_raw\":\"1\"}\nGARBAGE\n",
        )
        .unwrap();
        let events = read_all_at(&dir, taker);
        assert_eq!(events.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
