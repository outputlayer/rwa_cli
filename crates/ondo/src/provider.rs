use alloy_provider::ProviderBuilder;
use eyre::Result;

/// Create an alloy HTTP provider for any chain.
pub async fn create_provider(
    rpc_url: &str,
) -> Result<impl alloy_provider::Provider + Clone> {
    let provider = ProviderBuilder::new()
        .connect_http(rpc_url.parse()?);
    Ok(provider)
}
