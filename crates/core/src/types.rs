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
    /// Dividends are reinvested, so this value grows over time.
    pub shares_per_token: U256,
}

/// Token balance info for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBalance {
    pub token: GmToken,
    pub balance: U256,
}
