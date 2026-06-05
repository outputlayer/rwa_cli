//! Jupiter aggregator client: quote (`/order`) and submit (`/execute`).
//!
//! Public entry points:
//! - [`get_order`] — fetch a swap quote, walking public Jupiter backends.
//! - [`execute_order`] — sign and submit the quoted order via the backend
//!   that produced it.
//!
//! Sub-modules:
//! - `types` — request/response structs and the `ExecuteFailure` taxonomy.
//! - `order` — `/order` flow with retries and Metis V1 fallback.
//! - `execute` — `/execute` flow per `OrderBackend`.

use tokio::sync::Semaphore;

mod execute;
mod order;
pub mod types;

pub use execute::execute_order;
pub use order::get_order;
pub use types::{
    ExecuteFailure, ExecuteFailureKind, ExecuteResponse, ExecuteRetryAction, OrderBackend,
    OrderResponse,
};

pub use crate::USDC_MINT;

/// Wrapped SOL on Solana
pub const SOL_MINT: &str = "So11111111111111111111111111111111111111112";
pub const USDC_DECIMALS: u8 = 6;
/// Ondo GM tokens on Solana use 9 decimals (Solana standard).
pub const GM_SOL_DECIMALS: u8 = 9;

// Jupiter is deprecating `lite-api.jup.ag` and throttling it to force migration;
// `api.jup.ag` serves the same paths. Const names keep the `_LITE_` suffix for
// import stability — they now point at api.jup.ag.
const SWAP_V2_LITE_API_BASE: &str = "https://api.jup.ag/swap/v2";
const ULTRA_API_BASE: &str = "https://ultra-api.jup.ag";
const ULTRA_LITE_API_BASE: &str = "https://api.jup.ag/ultra/v1";
const METIS_LITE_API_BASE: &str = "https://api.jup.ag/swap/v1";
const JUPITER_CLIENT_PLATFORM: &str = "jupiter.cli";
const JUPITER_API_KEY_ENV: &str = "RWA_JUPITER_API_KEY";

/// Jupiter API key from the environment (raises rate limits on api.jup.ag).
fn jupiter_api_key() -> Option<String> {
    std::env::var(JUPITER_API_KEY_ENV)
        .ok()
        .filter(|k| !k.trim().is_empty())
}

/// Apply the standard Jupiter headers, plus `x-api-key` when a key is provided.
/// The key is a parameter (not read from env here) so this is pure and testable.
fn apply_jupiter_headers(
    request: reqwest::RequestBuilder,
    api_key: Option<&str>,
) -> reqwest::RequestBuilder {
    let request = request.header("x-client-platform", JUPITER_CLIENT_PLATFORM);
    match api_key {
        Some(key) if !key.is_empty() => request.header("x-api-key", key.to_string()),
        _ => request,
    }
}

/// Concurrency limit for `/order` (quote) requests.
/// Jupiter routes orders per-wallet; too many concurrent `/order` calls from the
/// same taker wallet cause rejections (not rate-limit 429, but routing conflicts).
/// Tested: 4 concurrent fails, 2 concurrent reliable across 20-token baskets.
static ORDER_SEMAPHORE: Semaphore = Semaphore::const_new(2);

/// Concurrency limit for `/execute` (signed-tx submission) requests.
/// Execute uses pre-cached orders so wallet contention is lower.
static EXECUTE_SEMAPHORE: Semaphore = Semaphore::const_new(5);

fn with_jupiter_headers(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    apply_jupiter_headers(request, jupiter_api_key().as_deref())
}

#[cfg(test)]
mod base_url_tests {
    use super::{METIS_LITE_API_BASE, SWAP_V2_LITE_API_BASE, ULTRA_LITE_API_BASE};

    #[test]
    fn jupiter_bases_use_api_host_not_deprecated_lite() {
        for base in [SWAP_V2_LITE_API_BASE, ULTRA_LITE_API_BASE, METIS_LITE_API_BASE] {
            assert!(base.starts_with("https://api.jup.ag/"), "still on a non-api host: {base}");
            assert!(!base.contains("lite-api"), "still on deprecated lite-api: {base}");
        }
    }
}

#[cfg(test)]
mod header_tests {
    use super::apply_jupiter_headers;

    #[test]
    fn api_key_header_present_only_when_key_set() {
        let client = reqwest::Client::new();
        let with = apply_jupiter_headers(client.get("https://api.jup.ag/x"), Some("k123"))
            .build()
            .unwrap();
        assert_eq!(with.headers().get("x-api-key").unwrap(), "k123");
        assert_eq!(with.headers().get("x-client-platform").unwrap(), "jupiter.cli");

        let without = apply_jupiter_headers(client.get("https://api.jup.ag/x"), None)
            .build()
            .unwrap();
        assert!(without.headers().get("x-api-key").is_none());
        assert_eq!(without.headers().get("x-client-platform").unwrap(), "jupiter.cli");
    }
}
