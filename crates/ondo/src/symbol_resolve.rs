use eyre::Result;

use crate::token_list::GmTokenEntry;
use crate::usecases::gm::{GmTradeError, GmTradeErrorKind};

/// Resolve a GM token by symbol or Solana address (case-insensitive).
/// Accepts "TSLA", "TSLAon" formats, or a Solana base58 mint address.
pub fn resolve_token<'a>(symbol: &str, tokens: &'a [GmTokenEntry]) -> Result<&'a GmTokenEntry> {
    // Try Solana address lookup (base58, typically 32+ chars)
    if symbol.len() >= 32
        && let Some(entry) = tokens.iter().find(|t| t.solana_address == Some(symbol))
    {
        return Ok(entry);
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
        .ok_or_else(|| {
            // Typed so agents get `error_kind: "unknown_token"` instead of null;
            // point humans at the discovery command.
            GmTradeError::new(
                GmTradeErrorKind::UnknownToken,
                format!("Unknown GM token: {symbol}. Browse symbols with `rwa gm search --search <keyword>`"),
            )
            .into()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_tokens() -> Vec<GmTokenEntry> {
        vec![
            GmTokenEntry { symbol: "TSLAon", solana_address: Some("FakeAddr123456789012345678901234567890abc") },
            GmTokenEntry { symbol: "AAPLon", solana_address: Some("FakeAddr223456789012345678901234567890abc") },
            GmTokenEntry { symbol: "SPYon", solana_address: None },
        ]
    }

    #[test]
    fn resolve_with_on_suffix() {
        let tokens = test_tokens();
        let entry = resolve_token("TSLAon", &tokens).unwrap();
        assert_eq!(entry.symbol, "TSLAon");
    }

    #[test]
    fn resolve_without_suffix() {
        let tokens = test_tokens();
        let entry = resolve_token("TSLA", &tokens).unwrap();
        assert_eq!(entry.symbol, "TSLAon");
    }

    #[test]
    fn resolve_case_insensitive() {
        let tokens = test_tokens();
        let entry = resolve_token("tsla", &tokens).unwrap();
        assert_eq!(entry.symbol, "TSLAon");
    }

    #[test]
    fn resolve_by_mint_address() {
        let tokens = test_tokens();
        let entry = resolve_token("FakeAddr123456789012345678901234567890abc", &tokens).unwrap();
        assert_eq!(entry.symbol, "TSLAon");
    }

    #[test]
    fn resolve_unknown_fails() {
        let tokens = test_tokens();
        assert!(resolve_token("NOPE", &tokens).is_err());
    }

    #[test]
    fn resolve_no_solana_address() {
        let tokens = test_tokens();
        let entry = resolve_token("SPY", &tokens).unwrap();
        assert_eq!(entry.symbol, "SPYon");
        assert!(entry.solana_address.is_none());
    }
}
