use alloy_primitives::Address;
use alloy_provider::Provider;
use eyre::{Result, eyre};
use rwa_core::chainlink;
use rwa_core::contracts::{ISyntheticSharesOracle, SYNTHETIC_SHARES_ORACLE};
use rwa_core::types::{GmTokenPrice, OracleData};

/// Fetch oracle data for a GM token from the SyntheticSharesOracle (BSC).
pub async fn get_oracle_data<P: Provider>(provider: &P, token: Address) -> Result<OracleData> {
    let oracle = ISyntheticSharesOracle::new(SYNTHETIC_SHARES_ORACLE, provider);
    let result = oracle.assetData(token).call().await?;

    Ok(OracleData {
        shares_per_token: result.sharesPerToken,
    })
}

/// Fetch token price via Chainlink Tokenized Equity Feed on Ethereum Mainnet.
/// Single RPC call — feed returns token_price directly (stock_price × sValue).
/// `feed_override` allows specifying a custom feed address.
pub async fn get_token_price<P: Provider>(
    provider: &P,
    symbol: &str,
    feed_override: Option<Address>,
) -> Result<GmTokenPrice> {
    let feed_address = match feed_override {
        Some(addr) => addr,
        None => chainlink::resolve_feed(symbol)
            .ok_or_else(|| eyre!("No Chainlink Tokenized Equity Feed for '{symbol}'"))?,
    };

    let cl_price = chainlink::get_chainlink_price(provider, feed_address).await?;

    Ok(GmTokenPrice {
        token_price_usd: cl_price.price,
        feed_description: cl_price.description,
        price_updated_at: cl_price.updated_at,
    })
}
