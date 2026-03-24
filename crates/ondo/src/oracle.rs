use alloy_primitives::Address;
use alloy_provider::Provider;
use eyre::Result;
use rwa_core::contracts::{ISyntheticSharesOracle, SYNTHETIC_SHARES_ORACLE};
use rwa_core::types::OracleData;

/// Fetch oracle data for a GM token from the SyntheticSharesOracle.
pub async fn get_oracle_data<P: Provider>(provider: &P, token: Address) -> Result<OracleData> {
    let oracle = ISyntheticSharesOracle::new(SYNTHETIC_SHARES_ORACLE, provider);
    let result = oracle.assetData(token).call().await?;

    Ok(OracleData {
        shares_per_token: result.sharesPerToken,
        last_update_timestamp: result.lastUpdateTimestamp.to::<u64>(),
        max_absolute_diff_bps: result.maxAbsoluteDiffBps.to::<u64>(),
        min_price_update_window: result.minPriceUpdateWindow.to::<u64>(),
    })
}
