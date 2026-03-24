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

/// Chainlink price data from AggregatorV3.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainlinkPrice {
    /// Price as f64 (already divided by 10^decimals).
    pub price: f64,
    /// Number of decimals in the raw answer.
    pub decimals: u8,
    /// Timestamp of last update.
    pub updated_at: U256,
    /// Feed description (e.g. "TSLA / USD").
    pub description: String,
}

/// Combined GM token price from Chainlink Tokenized Equity Feed.
/// Single call — feed returns token_price directly (stock × sValue).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GmTokenPrice {
    /// Token price in USD (already includes sValue multiplier).
    pub token_price_usd: f64,
    /// Chainlink feed description.
    pub feed_description: String,
    /// Chainlink price last updated timestamp.
    pub price_updated_at: U256,
}

/// Token balance info for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBalance {
    pub token: GmToken,
    pub balance: U256,
}
