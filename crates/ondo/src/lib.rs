pub mod api;
pub mod gm;
pub mod jupiter;
pub mod solana;
pub mod token_list;
pub mod usecases;
pub mod wallet;

/// USDC mint address on Solana.
pub const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

/// Shared HTTP client — reuses connection pool and TLS sessions across all API calls.
pub(crate) static HTTP: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("rwa-cli/0.1")
        .build()
        .expect("failed to build HTTP client")
});
