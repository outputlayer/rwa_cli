pub mod api;
pub mod amounts;
pub mod audit;
pub mod jupiter;
pub mod ledger;
pub mod solana;
pub mod spl;
pub mod symbol_resolve;
pub mod token_list;
pub mod types;
pub mod usecases;
pub mod wallet;

pub use types::{Mint, Symbol};

/// USDC mint address on Solana.
pub const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

/// True when `RWA_DEBUG` is set to a non-empty, non-`0` value. Gates verbose
/// diagnostics (raw simulation logs, per-backend quote failures) that are
/// useful for debugging but noise for everyday use.
pub(crate) fn debug_enabled() -> bool {
    std::env::var("RWA_DEBUG")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

/// Shared HTTP client — reuses connection pool and TLS sessions across all API calls.
pub(crate) static HTTP: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("rwa-cli/0.1")
        .build()
        .expect("failed to build HTTP client")
});
