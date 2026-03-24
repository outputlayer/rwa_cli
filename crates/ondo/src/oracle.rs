use alloy_primitives::{Address, U256};
use alloy_provider::Provider;
use eyre::Result;
use rwa_core::contracts::{ISyntheticSharesOracle, SYNTHETIC_SHARES_ORACLE};
use rwa_core::types::SValueData;

/// SyntheticSharesOracle addresses per chain.
pub const ORACLE_BSC: Address = SYNTHETIC_SHARES_ORACLE;

/// Ethereum Mainnet SyntheticSharesOracle.
pub const ORACLE_ETH: Address =
    alloy_primitives::address!("9BC39DB6fbB44B91a48b8D5A6C208B82B1741bE6");

/// Fetch sValue for a single GM token via `getSValue(address)`.
pub async fn get_svalue<P: Provider>(provider: &P, token: Address) -> Result<SValueData> {
    let oracle = ISyntheticSharesOracle::new(SYNTHETIC_SHARES_ORACLE, provider);
    let raw = oracle.getSValue(token).call().await?;
    let value = raw as f64 / 1e18;
    Ok(SValueData {
        raw: U256::from(raw),
        value,
    })
}

/// Batch fetch sValue for multiple GM tokens via `getSValueBatch(address[])`.
/// Returns (token_address, SValueData) pairs for tokens that succeeded.
pub async fn get_svalue_batch<P: Provider>(
    provider: &P,
    tokens: &[Address],
) -> Result<Vec<(Address, SValueData)>> {
    let oracle = ISyntheticSharesOracle::new(SYNTHETIC_SHARES_ORACLE, provider);
    let values = oracle.getSValueBatch(tokens.to_vec()).call().await?;

    let results: Vec<(Address, SValueData)> = tokens
        .iter()
        .zip(values.iter())
        .filter(|(_, &v)| v > 0)
        .map(|(&addr, &v)| {
            let value = v as f64 / 1e18;
            (addr, SValueData {
                raw: U256::from(v),
                value,
            })
        })
        .collect();

    Ok(results)
}

/// Legacy: fetch oracle data via `assetData(address)` (backward compat).
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
