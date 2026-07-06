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
use sha2::{Digest, Sha256};

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
    /// Tamper-evidence link: lowercase hex of the first 16 bytes of
    /// SHA-256 over the raw previous line (no trailing newline). Absent for
    /// the first line ever written, or for any pre-chain (legacy) entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev: Option<String>,
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
            prev: None,
        }
    }
}

/// Tamper-evidence status of a wallet's ledger hash chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainStatus {
    /// Every entry that can carry a `prev` link has one, and it matches.
    Ok,
    /// The chain never breaks, but at least one entry predates chaining
    /// (no `prev` field) — cannot be proven back to genesis.
    Legacy,
    /// A link mismatch was found; the payload is the 1-based line number of
    /// the first line whose `prev` doesn't match the hash of its predecessor.
    BrokenAt(usize),
}

/// First 16 bytes of SHA-256 over `bytes`, lowercase hex.
fn hash16_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex::encode(&digest[..16])
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
    let path = dir.join(format!("{taker}.jsonl"));

    // Chain link: hash of the raw previous line, or of empty bytes when
    // this is the first entry ever written for this wallet — every entry
    // written by chain-aware code carries a `prev`, which is what lets the
    // verifier tell a true genesis apart from a pre-chain legacy entry
    // (which has no `prev` key at all). Best-effort: any read error here
    // (missing file, unreadable) just falls back to the empty-bytes case —
    // the ledger append itself must never fail because of this.
    let last_line = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| text.lines().next_back().map(str::to_owned))
        .unwrap_or_default();

    let mut event = event.clone();
    event.prev = Some(hash16_hex(last_line.as_bytes()));
    let line = serde_json::to_string(&event)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

/// Tamper-evidence check for wallet `taker`'s ledger under the default
/// ledger directory. See [`verify_chain_at`] for the algorithm.
#[must_use]
pub fn verify_chain(taker: &str) -> ChainStatus {
    ledger_dir()
        .map(|dir| verify_chain_at(&dir, taker))
        .unwrap_or(ChainStatus::Ok)
}

/// Verify the hash chain of wallet `taker`'s ledger file under `dir`.
///
/// Reads RAW lines (not parsed events), because a tampered line may still be
/// syntactically valid JSON — the chain link is the only thing that catches
/// a content edit. For every line i (0-based):
/// - if it fails to parse, or parses but has no `prev`, that's a pre-chain
///   legacy entry (or corruption `record_at` never produces) — noted, but
///   only turns into the final `Legacy` verdict if no break is found.
/// - if it parses and carries `prev` (i>0), compare it to the hash of the
///   raw *previous* line (whether or not that line itself parses — a
///   corrupted predecessor changes its own hash, which is exactly what
///   catches it); a mismatch is `BrokenAt(i+1)` (1-based), reported
///   against the successor since that's the first line that provably
///   disagrees with what's on disk.
///
/// Missing file reads as `Ok` (nothing to verify yet).
#[must_use]
pub(crate) fn verify_chain_at(dir: &Path, taker: &str) -> ChainStatus {
    let Ok(text) = std::fs::read_to_string(dir.join(format!("{taker}.jsonl"))) else {
        return ChainStatus::Ok;
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut legacy = false;

    for (i, line) in lines.iter().enumerate() {
        let prev = serde_json::from_str::<LedgerEvent>(line).ok().and_then(|e| e.prev);
        match prev {
            Some(p) => {
                if i > 0 && p != hash16_hex(lines[i - 1].as_bytes()) {
                    return ChainStatus::BrokenAt(i + 1);
                }
            }
            None => legacy = true,
        }
    }

    if legacy {
        ChainStatus::Legacy
    } else {
        ChainStatus::Ok
    }
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
        assert_eq!(events[0].token, "TSLAon");
        assert_eq!(events[0].qty_raw, "11786465");
        assert_eq!(events[0].usdc_raw.as_deref(), Some("5000000"));
        assert_eq!(events[0].sig.as_deref(), Some("sig1"));
        // Audit fix: the sell leg's amounts must round-trip too — this file
        // feeds all P&L, so a mangled qty/usdc on read would misprice trades.
        assert_eq!(events[1].kind, "sell");
        assert_eq!(events[1].qty_raw, "11786465");
        assert_eq!(events[1].usdc_raw.as_deref(), Some("4991915"));
        assert_eq!(events[1].sig.as_deref(), Some("sig2"));

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
        // The survivor must be the parseable line, intact — not a half-parsed
        // hybrid of the two.
        assert_eq!(events[0].kind, "buy");
        assert_eq!(events[0].token, "TSLAon");
        assert_eq!(events[0].qty_raw, "1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn hash16_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(bytes);
        hex::encode(&digest[..16])
    }

    #[test]
    fn chain_links_and_verifies_ok() {
        let dir = std::env::temp_dir().join(format!("rwa-ledger-chain-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let taker = "ChainOkWallet";

        let e1 = LedgerEvent::now(Some("sig1".into()), "buy", "TSLAon", "1", None);
        let e2 = LedgerEvent::now(Some("sig2".into()), "sell", "TSLAon", "1", None);
        record_at(&dir, taker, &e1).unwrap();
        record_at(&dir, taker, &e2).unwrap();

        let text = std::fs::read_to_string(dir.join(format!("{taker}.jsonl"))).unwrap();
        let mut lines = text.lines();
        let line1 = lines.next().unwrap();
        let line2 = lines.next().unwrap();

        // The second line's `prev` must equal hex(sha256(line1)[..16]),
        // computed here independently of the production hashing helper.
        let expected_prev = hash16_hex(line1.as_bytes());
        let parsed2: LedgerEvent = serde_json::from_str(line2).unwrap();
        assert_eq!(parsed2.prev.as_deref(), Some(expected_prev.as_str()));

        assert!(matches!(verify_chain_at(&dir, taker), ChainStatus::Ok));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_lines_without_prev_are_legacy_not_broken() {
        let dir = std::env::temp_dir().join(format!("rwa-ledger-legacy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let taker = "LegacyWallet";

        // Hand-write one pre-chain line (no `prev` field at all).
        std::fs::write(
            dir.join(format!("{taker}.jsonl")),
            "{\"ts\":\"t\",\"kind\":\"buy\",\"token\":\"TSLAon\",\"qty_raw\":\"1\"}\n",
        )
        .unwrap();

        // Then record one chained event on top of it.
        let e2 = LedgerEvent::now(Some("sig2".into()), "sell", "TSLAon", "1", None);
        record_at(&dir, taker, &e2).unwrap();

        assert!(matches!(verify_chain_at(&dir, taker), ChainStatus::Legacy));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tampered_line_breaks_chain_at_its_successor() {
        let dir = std::env::temp_dir().join(format!("rwa-ledger-tamper-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let taker = "TamperWallet";

        let e1 = LedgerEvent::now(Some("sig1".into()), "buy", "TSLAon", "1", None);
        let e2 = LedgerEvent::now(Some("sig2".into()), "sell", "TSLAon", "1", None);
        let e3 = LedgerEvent::now(Some("sig3".into()), "buy", "NVDAon", "2", None);
        record_at(&dir, taker, &e1).unwrap();
        record_at(&dir, taker, &e2).unwrap();
        record_at(&dir, taker, &e3).unwrap();

        // Flip a byte in line 2 on disk (tamper with an existing entry).
        let path = dir.join(format!("{taker}.jsonl"));
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = text.lines().map(String::from).collect();
        assert_eq!(lines.len(), 3);
        lines[1] = lines[1].replace("sell", "SELL");
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();

        assert!(matches!(verify_chain_at(&dir, taker), ChainStatus::BrokenAt(3)));

        // read_all still returns parseable events (pnl keeps working) — the
        // tampered line 2 still parses fine (only its content field changed),
        // so all three events remain readable.
        let events = read_all_at(&dir, taker);
        assert_eq!(events.len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
