use serde::{Deserialize, Serialize};
use std::ops::Deref;

/// A canonicalized GM token symbol, e.g. "TSLAon", "AAPLon".
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Symbol(String);

impl Deref for Symbol {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Symbol {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for Symbol {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Symbol {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// A Solana mint address (base58, 32 bytes decoded).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Mint(String);

impl Deref for Mint {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Mint {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Mint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for Mint {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Mint {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_display_and_deref() {
        let s = Symbol::from("TSLAon");
        assert_eq!(s.to_string(), "TSLAon");
        assert_eq!(&*s, "TSLAon");
    }

    #[test]
    fn mint_display_and_deref() {
        let m = Mint::from("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
        assert_eq!(m.to_string(), "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
        assert_eq!(&*m, "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
    }

    #[test]
    fn symbol_from_string_and_str() {
        let a = Symbol::from("TSLA");
        let b = Symbol::from("TSLA".to_string());
        assert_eq!(a, b);
    }

    #[test]
    fn mint_ne_symbol_at_type_level() {
        let sym = Symbol::from("TSLAon");
        let mint = Mint::from("TSLAon");
        assert_eq!(&*sym, &*mint);
    }

    #[test]
    fn symbol_serializes_as_plain_string() {
        let s = Symbol::from("TSLAon");
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "\"TSLAon\"");
    }

    #[test]
    fn mint_serializes_as_plain_string() {
        let m = Mint::from("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(json, "\"EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v\"");
    }
}
