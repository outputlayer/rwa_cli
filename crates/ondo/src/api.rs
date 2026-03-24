use eyre::{Result, eyre};
use serde::Deserialize;

const ONDO_API_URL: &str = "https://app.ondo.finance/api/v2/assets";

// ─── app.ondo.finance/api/v2/assets (free, no auth) ────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OndoAsset {
    pub symbol: String,
    #[serde(default)]
    pub ticker: Option<String>,
    pub asset_name: String,
    #[serde(default)]
    pub tags: Vec<Tag>,
    pub primary_market: Option<PrimaryMarket>,
    pub underlying_market: Option<UnderlyingMarket>,
    pub timestamp: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tag {
    pub category_label: String,
    pub tag_label: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimaryMarket {
    pub symbol: String,
    pub price: String,
    #[serde(default)]
    pub price_change_24h: Option<String>,
    #[serde(default)]
    pub price_change_pct_24h: Option<String>,
    #[serde(default)]
    pub total_holders: Option<u64>,
    #[serde(default)]
    pub shares_multiplier: Option<String>,
    #[serde(default)]
    pub tradable_sessions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnderlyingMarket {
    pub ticker: String,
    pub name: String,
    #[serde(default)]
    pub price_high_52w: Option<String>,
    #[serde(default)]
    pub price_low_52w: Option<String>,
    #[serde(default)]
    pub volume: Option<String>,
    #[serde(default)]
    pub average_volume: Option<String>,
    #[serde(default)]
    pub shares_outstanding: Option<String>,
    #[serde(default)]
    pub market_cap: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssetsResponse {
    assets: Vec<OndoAsset>,
}

/// Fetch all assets from the official Ondo API (free, no auth required).
pub async fn fetch_assets() -> Result<Vec<OndoAsset>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let resp = client.get(ONDO_API_URL).send().await?;
    if !resp.status().is_success() {
        return Err(eyre!("Ondo API returned status {}", resp.status()));
    }
    let wrapper: AssetsResponse = resp.json().await?;
    Ok(wrapper.assets)
}

/// Find an asset by symbol (case-insensitive, accepts "TSLA" or "TSLAon").
pub fn find_asset<'a>(symbol: &str, assets: &'a [OndoAsset]) -> Option<&'a OndoAsset> {
    let normalized = symbol.to_uppercase();
    let lookup = if normalized.ends_with("ON") {
        normalized
    } else {
        format!("{normalized}ON")
    };
    assets.iter().find(|a| a.symbol.to_uppercase() == lookup)
}

/// Parse price string to f64, returning 0.0 on failure.
pub fn parse_price(s: &str) -> f64 {
    s.parse::<f64>().unwrap_or(0.0)
}
