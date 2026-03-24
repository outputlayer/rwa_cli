use alloy_primitives::{address, Address, I256, U256};
use alloy_provider::Provider;
use eyre::{Result, eyre};

use crate::contracts::IAggregatorV3;
use crate::types::ChainlinkPrice;

// ─── Ondo GM Tokenized Equity Feeds on Ethereum Mainnet ─────────────────────
// Source: Chainlink RDD (reference-data-directory.vercel.app/feeds-mainnet.json)
// Docs:   https://docs.chain.link/data-feeds/tokenized-equity-feeds/ondo
//
// These feeds return the GM token price directly (stock_price × sValue).
// One call instead of two — no separate SyntheticSharesOracle needed.
// All feeds: AggregatorV3Interface, 8 decimals, 0.5% deviation, 86400s heartbeat.

/// Tokenized equity feeds on Ethereum Mainnet.
/// Return token_price directly (includes sValue multiplier).
/// "Calculated" variant used when available; "Ondo API" as fallback.
const TOKENIZED_EQUITY_FEEDS: &[(&str, Address)] = &[
    ("TSLA", address!("89904B6fcF8dAD1e5DA47dFdF69fC38Ad6be0bd5")), // TSLAon-USD (Calculated)
    ("SPY",  address!("d16cC387E87d37350f57421DaDF811968441C1a5")), // SPYon-USD  (Calculated)
    ("QQQ",  address!("2098C245Fe4C80cdA93cF85Cff0718328D4eEa85")), // QQQon-USD  (Calculated)
    ("CRCL", address!("C0457F67cac4eb0567A208955C332897a597A207")), // CRCLon-USD (Ondo API)
];

/// Resolve a Chainlink Tokenized Equity Feed for a GM token symbol.
/// Accepts "TSLA", "TSLAon", "tsla" etc.
pub fn resolve_feed(symbol: &str) -> Option<Address> {
    let normalized = symbol.to_uppercase();
    let base = if normalized.ends_with("ON") {
        &normalized[..normalized.len() - 2]
    } else {
        &normalized
    };

    TOKENIZED_EQUITY_FEEDS
        .iter()
        .find(|(sym, _)| *sym == base)
        .map(|&(_, addr)| addr)
}

/// Fetch price from a Chainlink AggregatorV3 feed.
pub async fn get_chainlink_price<P: Provider>(
    provider: &P,
    feed_address: Address,
) -> Result<ChainlinkPrice> {
    let feed = IAggregatorV3::new(feed_address, provider);

    let round_data = feed.latestRoundData().call().await?;
    let decimals = feed.decimals().call().await?;
    let description = feed.description().call().await?;

    let answer = round_data.answer;
    if answer <= I256::ZERO {
        return Err(eyre!("Chainlink feed returned non-positive price: {answer}"));
    }
    if round_data.updatedAt == U256::ZERO {
        return Err(eyre!("Chainlink feed has not been updated (updatedAt = 0)"));
    }

    let divisor = 10f64.powi(decimals as i32);
    let price = answer.to_string().parse::<f64>()? / divisor;

    Ok(ChainlinkPrice {
        price,
        decimals,
        updated_at: round_data.updatedAt,
        description,
    })
}
