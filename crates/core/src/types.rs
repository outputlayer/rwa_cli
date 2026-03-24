use alloy_primitives::{Address, U256};
use serde::{Deserialize, Serialize};

/// A GM token (tokenized stock/ETF) on BNB Chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GmToken {
    pub symbol: String,
    pub name: String,
    pub address: Address,
    pub decimals: u8,
}

/// Oracle data for a GM token from SyntheticSharesOracle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleData {
    /// How many underlying shares one token represents (18 decimals).
    pub shares_per_token: U256,
    /// When the oracle was last updated (unix timestamp).
    pub last_update_timestamp: u64,
    /// Maximum absolute price change allowed (basis points).
    pub max_absolute_diff_bps: u64,
    /// Minimum seconds between oracle updates.
    pub min_price_update_window: u64,
}

/// Token balance info for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBalance {
    pub token: GmToken,
    pub balance: U256,
}
