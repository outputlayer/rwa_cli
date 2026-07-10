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
            // suggest the nearest symbol (typos are the most common input
            // error) and point humans at the discovery command.
            let suggestion = closest_symbol(symbol, tokens)
                .map(|s| format!(" Did you mean {s}?"))
                .unwrap_or_default();
            GmTradeError::new(
                GmTradeErrorKind::UnknownToken,
                format!("Unknown GM token: {symbol}.{suggestion} Browse symbols with `rwa gm search --search <keyword>`"),
            )
            .into()
        })
}

/// Nearest known symbol by edit distance ≤ 2 against both the bare ticker
/// (`AAPL`) and the on-suffixed form (`AAPLON`), uppercased. Ties resolve to
/// the first (alphabetical) entry; `None` when nothing is close enough —
/// suggesting a far-off symbol would be worse than no suggestion.
fn closest_symbol<'a>(input: &str, tokens: &'a [GmTokenEntry]) -> Option<&'a str> {
    let needle = input.to_uppercase();
    let mut best: Option<(usize, &str)> = None;
    for t in tokens {
        let full = t.symbol.to_uppercase();
        let bare = full.strip_suffix("ON").unwrap_or(&full);
        let d = edit_distance(&needle, bare).min(edit_distance(&needle, &full));
        if d <= 2 && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, t.symbol));
        }
    }
    best.map(|(_, s)| s)
}

/// Classic Levenshtein distance, two-row DP — inputs are short tickers, so
/// no need for anything cleverer (and no new dependency).
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
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

#[cfg(test)]
mod suggestion_tests {
    use super::*;
    use crate::token_list::get_token_list;

    #[test]
    fn typo_gets_a_did_you_mean_suggestion() {
        let tokens = get_token_list();
        // The classic transposition typo: APPL → AAPL(on).
        let err = resolve_token("APPL", tokens).unwrap_err().to_string();
        assert!(err.contains("Did you mean AAPLon?"), "got: {err}");
        // One extra char: TSLAA → TSLAon.
        let err = resolve_token("TSLAA", tokens).unwrap_err().to_string();
        assert!(err.contains("Did you mean TSLAon?"), "got: {err}");
    }

    #[test]
    fn garbage_gets_no_suggestion_but_keeps_the_search_hint() {
        let tokens = get_token_list();
        let err = resolve_token("ZZZQQQXX", tokens).unwrap_err().to_string();
        assert!(!err.contains("Did you mean"), "far-off input must not suggest: {err}");
        assert!(err.contains("gm search"), "search hint stays: {err}");
    }

    #[test]
    fn edit_distance_basics() {
        assert_eq!(edit_distance("AAPL", "AAPL"), 0);
        assert_eq!(edit_distance("APPL", "AAPL"), 1); // one substitution (P→A at index 1)
        assert_eq!(edit_distance("", "ABC"), 3);
        assert_eq!(edit_distance("KITTEN", "SITTING"), 3);
    }
}
