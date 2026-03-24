use alloy_primitives::Address;
use alloy_provider::Provider;
use eyre::Result;
use rwa_core::contracts::{ISyntheticSharesOracle, SYNTHETIC_SHARES_ORACLE};
use rwa_core::types::SValueData;

/// Fetch oracle data via `assetData(address)`.
pub async fn get_oracle_data<P: Provider>(
    provider: &P,
    token: Address,
) -> Result<SValueData> {
    let oracle = ISyntheticSharesOracle::new(SYNTHETIC_SHARES_ORACLE, provider);
    let result = oracle.assetData(token).call().await?;
    let value = result.sharesPerToken.to_string().parse::<f64>()? / 1e18;
    Ok(SValueData {
        raw: result.sharesPerToken,
        value,
    })
}
