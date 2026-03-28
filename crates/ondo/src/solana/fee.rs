use std::sync::Mutex;
use std::time::{Duration, Instant};

use eyre::Result;
use serde::Deserialize;

use super::rpc::rpc_call_simple;

/// Base fee per signature (5000 lamports, protocol constant).
const BASE_FEE_LAMPORTS: u64 = 5_000;

/// SPL Token account data size (bytes).
const SPL_TOKEN_ACCOUNT_SIZE: u64 = 165;
/// Token-2022 account data size (bytes) — base size without extensions.
const TOKEN_2022_ACCOUNT_SIZE: u64 = 182;

/// Fallback rent values if RPC is unreachable.
const ATA_RENT_LAMPORTS_FALLBACK: u64 = 2_039_280;
const ATA_RENT_LAMPORTS_2022_FALLBACK: u64 = 2_165_280;

/// Fee cache TTL — reuse cached fee for this duration.
const FEE_CACHE_TTL: Duration = Duration::from_secs(10);

/// Cache TTL for rent-exempt values — 5 minutes (changes only via governance).
const RENT_CACHE_TTL: Duration = Duration::from_secs(300);

/// Cached priority fee: (fee_lamports, timestamp).
static FEE_CACHE: std::sync::LazyLock<Mutex<(u64, Instant)>> =
    std::sync::LazyLock::new(|| Mutex::new((BASE_FEE_LAMPORTS, Instant::now())));

/// Rent-exempt cache: (lamports_per_byte, timestamp).
/// Cached because rent rate changes only via Solana governance vote (extremely rare).
static RENT_CACHE: std::sync::LazyLock<Mutex<(Option<u64>, Instant)>> =
    std::sync::LazyLock::new(|| Mutex::new((None, Instant::now())));

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PriorityFeeEntry {
    prioritization_fee: u64,
    #[expect(dead_code)]
    slot: Option<u64>,
}

/// Fetch median recent priority fee from RPC.
/// Passes writable account addresses for more accurate fee estimates (per Solana docs).
async fn fetch_priority_fee(writable_accounts: &[&str], rpc_url: Option<&str>) -> Result<u64> {
    let params = if writable_accounts.is_empty() {
        serde_json::json!([])
    } else {
        serde_json::json!([writable_accounts])
    };
    let entries: Vec<PriorityFeeEntry> = rpc_call_simple(
        "getRecentPrioritizationFees",
        params,
        rpc_url,
    ).await.unwrap_or_default();
    if entries.is_empty() {
        return Ok(0);
    }
    let mut fees: Vec<u64> = entries.iter().map(|e| e.prioritization_fee).collect();
    fees.sort_unstable();
    Ok(fees[fees.len() / 2]) // median
}

/// Estimate the transaction fee in SOL for a simple transfer (1 signature).
/// Fetches recent priority fees from RPC with caching (10s TTL), adds 30% buffer.
pub async fn estimate_tx_fee(rpc_url: Option<&str>) -> f64 {
    let priority = get_priority_fee_cached(rpc_url).await;
    let total = BASE_FEE_LAMPORTS + priority;
    let with_buffer = (total as f64 * 1.3) as u64;
    with_buffer as f64 / 1_000_000_000.0
}

/// Estimate SOL needed for gas, optionally including ATA creation rent.
/// Uses `getMinimumBalanceForRentExemption` RPC call (cached 5 min) as Solana recommends.
///
/// - `needs_ata`: if true, includes rent-exempt minimum for an SPL token ATA.
/// - `is_token_2022`: if true, uses Token-2022 ATA size (182 bytes) for rent.
/// - Returns SOL amount with 30% safety buffer.
pub async fn estimate_gas_needed(needs_ata: bool, is_token_2022: bool, rpc_url: Option<&str>) -> f64 {
    let priority = get_priority_fee_cached(rpc_url).await;
    let mut total_lamports = BASE_FEE_LAMPORTS + priority;
    if needs_ata {
        let size = if is_token_2022 { TOKEN_2022_ACCOUNT_SIZE } else { SPL_TOKEN_ACCOUNT_SIZE };
        total_lamports += get_rent_exempt_cached(size, rpc_url).await;
    }
    // 30% buffer on total
    let with_buffer = (total_lamports as f64 * 1.3) as u64;
    with_buffer as f64 / 1_000_000_000.0
}

/// Get cached priority fee, refreshing from RPC if stale.
async fn get_priority_fee_cached(rpc_url: Option<&str>) -> u64 {
    if let Ok(cache) = FEE_CACHE.lock() {
        if cache.1.elapsed() < FEE_CACHE_TTL {
            return cache.0;
        }
    }
    let fee = fetch_priority_fee(&[], rpc_url).await.unwrap_or(0);
    if let Ok(mut cache) = FEE_CACHE.lock() {
        *cache = (fee, Instant::now());
    }
    fee
}

/// Get priority fee with specific writable accounts (bypasses cache for accuracy).
pub(super) async fn get_priority_fee_for_accounts(accounts: &[&str], rpc_url: Option<&str>) -> u64 {
    fetch_priority_fee(accounts, rpc_url).await.unwrap_or(0)
}

/// Fetch rent-exempt minimum from Solana RPC via `getMinimumBalanceForRentExemption`.
/// Caches the result for 5 minutes. Falls back to known values if RPC fails.
async fn get_rent_exempt_cached(data_size: u64, rpc_url: Option<&str>) -> u64 {
    // Check cache first
    if let Ok(cache) = RENT_CACHE.lock() {
        if let (Some(lamports), ts) = &*cache {
            if ts.elapsed() < RENT_CACHE_TTL && data_size == SPL_TOKEN_ACCOUNT_SIZE {
                return *lamports;
            }
        }
    }

    // Fetch from RPC
    if let Ok(lamports) = rpc_call_simple::<u64>(
        "getMinimumBalanceForRentExemption",
        serde_json::json!([data_size]),
        rpc_url,
    ).await {
        if data_size == SPL_TOKEN_ACCOUNT_SIZE {
            if let Ok(mut cache) = RENT_CACHE.lock() {
                *cache = (Some(lamports), Instant::now());
            }
        }
        return lamports;
    }

    // Fallback to known values
    match data_size {
        TOKEN_2022_ACCOUNT_SIZE => ATA_RENT_LAMPORTS_2022_FALLBACK,
        _ => ATA_RENT_LAMPORTS_FALLBACK,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_fee_constant() {
        assert_eq!(BASE_FEE_LAMPORTS, 5_000);
    }

    #[test]
    fn priority_fee_entry_deserialize() {
        let json = r#"{"slot":123,"prioritizationFee":1000}"#;
        let entry: PriorityFeeEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.prioritization_fee, 1000);
    }

    #[test]
    fn fee_cache_ttl_is_10s() {
        assert_eq!(FEE_CACHE_TTL, Duration::from_secs(10));
    }

    #[test]
    fn spl_token_account_size_is_165() {
        assert_eq!(SPL_TOKEN_ACCOUNT_SIZE, 165);
    }

    #[test]
    fn token_2022_account_size_is_182() {
        assert_eq!(TOKEN_2022_ACCOUNT_SIZE, 182);
    }

    #[test]
    fn rent_fallback_spl_is_known_value() {
        // 2_039_280 lamports = rent-exempt min for 165-byte SPL Token account (epoch 0 rate)
        assert_eq!(ATA_RENT_LAMPORTS_FALLBACK, 2_039_280);
    }

    #[test]
    fn rent_fallback_2022_is_known_value() {
        assert_eq!(ATA_RENT_LAMPORTS_2022_FALLBACK, 2_165_280);
    }

    #[test]
    fn rent_cache_ttl_is_5_minutes() {
        assert_eq!(RENT_CACHE_TTL, Duration::from_secs(300));
    }

    #[tokio::test]
    async fn estimate_gas_no_ata_returns_small_value() {
        // Without ATA creation, should only be base fee + priority + buffer
        let est = estimate_gas_needed(false, false, None).await;
        // Must be less than 0.001 SOL (just fees, no rent)
        assert!(est > 0.0 && est < 0.001, "estimate without ATA: {est}");
    }

    #[tokio::test]
    async fn estimate_gas_with_ata_includes_rent() {
        let without = estimate_gas_needed(false, false, None).await;
        let with_spl = estimate_gas_needed(true, false, None).await;
        let with_2022 = estimate_gas_needed(true, true, None).await;
        // With ATA must be significantly more than without
        assert!(with_spl > without + 0.001, "SPL ATA estimate should include rent");
        assert!(with_2022 > without + 0.001, "Token-2022 ATA estimate should include rent");
        // Token-2022 should be >= SPL (more bytes)
        assert!(with_2022 >= with_spl, "Token-2022 rent >= SPL rent");
    }
}
