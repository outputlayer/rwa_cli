use alloy_primitives::{Address, U256};
use serde::{Deserialize, Serialize};

/// A GM token (tokenized stock/ETF).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GmToken {
    pub symbol: String,
    pub name: String,
    pub address: Address,
    pub decimals: u8,
}

/// sValue data from SyntheticSharesOracle.
/// sValue = shares-per-token multiplier (18 decimals).
/// GM Token Price = Underlying Equity Market Price × sValue
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SValueData {
    /// Raw sValue as U256 (18 decimals).
    pub raw: U256,
    /// sValue as f64 (already divided by 1e18).
    pub value: f64,
}

/// Token balance info for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBalance {
    pub token: GmToken,
    pub balance: U256,
}
