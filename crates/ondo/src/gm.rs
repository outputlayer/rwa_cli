use eyre::{Result, eyre};

use crate::token_list::GmTokenEntry;

/// Resolve a GM token by symbol or Solana address (case-insensitive).
/// Accepts "TSLA", "TSLAon" formats, or a Solana base58 mint address.
pub fn resolve_token<'a>(symbol: &str, tokens: &'a [GmTokenEntry]) -> Result<&'a GmTokenEntry> {
    // Try Solana address lookup (base58, typically 32+ chars)
    if symbol.len() >= 32 {
        if let Some(entry) = tokens.iter().find(|t| t.solana_address == Some(symbol)) {
            return Ok(entry);
        }
    }

    let normalized = symbol.to_uppercase();
    let lookup = if normalized.ends_with("ON") {
        normalized
    } else {
        format!("{normalized}ON")
    };

    tokens
        .iter()
        .find(|t| t.symbol.to_uppercase() == lookup)
        .ok_or_else(|| eyre!("Unknown GM token: {symbol}"))
}
