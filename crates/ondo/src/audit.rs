//! Persistent audit log for swap operations.
//!
//! One JSON line per swap attempt, written to
//! `<config_dir>/rwa/logs/swaps-YYYY-MM-DD.jsonl`. Both successful and failed
//! swaps are recorded so the user has post-mortem visibility on every
//! state-changing operation. Read-only operations (portfolio, list, history)
//! are not logged here.
//!
//! Log writes are best-effort: a swap never fails because the log file is
//! unwritable. Errors are silently swallowed.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use serde::Serialize;

/// One JSONL record per swap attempt.
#[derive(Debug, Serialize)]
pub struct SwapAuditRecord {
    /// ISO 8601 UTC timestamp.
    pub timestamp: String,
    /// "buy" or "sell".
    pub op: &'static str,
    /// GM token symbol (e.g. "AAPLon").
    pub symbol: String,
    /// Raw on-chain amount (smallest units of the input token).
    pub raw_amount: String,
    /// Taker wallet pubkey.
    pub taker: String,
    /// Slippage tolerance in basis points (None = backend default).
    pub slippage_bps: Option<u32>,
    /// Jupiter backend that quoted the order ("swap-v2-lite", "ultra", etc).
    pub backend: String,
    /// "ok" or "error".
    pub status: &'static str,
    /// Solana transaction signature on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Formatted output amount on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_amount: Option<String>,
    /// Error message on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Stable error kind label on failure (matches CloseFailJson.error_kind).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<&'static str>,
}

/// Best-effort: append the record to today's log file. Errors are silently
/// dropped so a swap never fails because the log is unwritable.
pub fn record_swap(record: &SwapAuditRecord) {
    if let Err(_e) = try_record_swap(record) {
        // Intentionally swallow — audit log failures must not propagate.
        // If users want to debug log-write issues, surface via a debug
        // tracing layer in a future change.
    }
}

fn try_record_swap(record: &SwapAuditRecord) -> std::io::Result<()> {
    let dir = log_dir()?;
    std::fs::create_dir_all(&dir)?;

    let date = chrono::Utc::now().format("%Y-%m-%d");
    let path = dir.join(format!("swaps-{date}.jsonl"));

    let line = serde_json::to_string(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

fn log_dir() -> std::io::Result<PathBuf> {
    dirs::config_dir()
        .map(|d| d.join("rwa").join("logs"))
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no config dir"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_serializes_to_single_line_json() {
        let record = SwapAuditRecord {
            timestamp: "2026-05-09T12:34:56Z".to_string(),
            op: "buy",
            symbol: "AAPLon".to_string(),
            raw_amount: "15000000".to_string(),
            taker: "Dn9EqxugBePrno7gzCjbGeYxY3VJE9RB2WE2FH7t7qmH".to_string(),
            slippage_bps: Some(100),
            backend: "swap-v2-lite".to_string(),
            status: "ok",
            signature: Some("5xGy".to_string()),
            output_amount: Some("0.061".to_string()),
            error: None,
            error_kind: None,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains('\n'), "JSON line must not contain newlines");
        assert!(json.contains("\"op\":\"buy\""));
        assert!(json.contains("\"signature\":\"5xGy\""));
        // skip_serializing_if drops null fields
        assert!(!json.contains("\"error\":"));
        assert!(!json.contains("\"error_kind\":"));
    }

    #[test]
    fn error_record_omits_signature_and_output_amount() {
        let record = SwapAuditRecord {
            timestamp: "2026-05-09T12:34:56Z".to_string(),
            op: "sell",
            symbol: "TSLAon".to_string(),
            raw_amount: "1000000".to_string(),
            taker: "FakeWallet".to_string(),
            slippage_bps: None,
            backend: "ultra".to_string(),
            status: "error",
            signature: None,
            output_amount: None,
            error: Some("quote expired".to_string()),
            error_kind: Some("quote_expired"),
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("\"signature\":"));
        assert!(!json.contains("\"output_amount\":"));
        assert!(json.contains("\"error_kind\":\"quote_expired\""));
    }

    // record_swap directory-creation behavior is exercised at runtime via the
    // execute_*_from_order paths (covered by integration tests on a live wallet).
    // Avoid env-hijacking dirs::config_dir() in unit tests — it requires
    // unsafe std::env::set_var with thread-safety caveats and provides little
    // value over trusting std::fs::create_dir_all to do its job.
}
