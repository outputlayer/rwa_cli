use eyre::{Result, eyre};
use serde::Deserialize;

const ONDO_API_URL: &str = "https://app.ondo.finance/api/v2/assets";

// ─── app.ondo.finance/api/v2/assets (free, no auth) ────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OndoAssetTag {
    pub category_slug: String,
    pub tag_label: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OndoAsset {
    pub symbol: String,
    pub asset_name: String,
    #[serde(default)]
    pub tags: Vec<OndoAssetTag>,
    pub primary_market: Option<PrimaryMarket>,
}

impl OndoAsset {
    /// Extract sector from tags (e.g. "Technology", "Healthcare").
    pub fn sector(&self) -> Option<&str> {
        self.tags.iter()
            .find(|t| t.category_slug == "sector-industry")
            .map(|t| t.tag_label.as_str())
    }

    /// Extract instrument type from tags ("Stock" or "ETF").
    pub fn instrument_type(&self) -> Option<&str> {
        self.tags.iter()
            .find(|t| t.category_slug == "instrument-type")
            .map(|t| t.tag_label.as_str())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimaryMarket {
    pub price: String,
    #[serde(default)]
    pub price_change_24h: Option<String>,
    #[serde(default)]
    pub price_change_pct_24h: Option<String>,
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

// ─── Price history ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryCandle {
    pub timestamp: u64,
    pub value: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryResponse {
    primary_market_price: Vec<HistoryCandle>,
}

/// Valid ranges: "1D", "1W", "1M", "3M", "1Y", "ALL"
pub async fn fetch_history(symbol: &str, range: &str) -> Result<Vec<HistoryCandle>> {
    let range_param = match range.to_uppercase().as_str() {
        "1D" => "1day",
        "1W" => "1week",
        "1M" => "1month",
        "3M" => "3months",
        "1Y" => "1year",
        "ALL" => "all",
        other => return Err(eyre!("Invalid range: {other}. Use: 1D, 1W, 1M, 3M, 1Y, ALL")),
    };

    let normalized = symbol.to_lowercase();
    let sym = if normalized.ends_with("on") {
        normalized
    } else {
        format!("{normalized}on")
    };

    let url = format!("{ONDO_API_URL}/{sym}/history?range={range_param}");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        return Err(eyre!("Ondo history API returned status {}", resp.status()));
    }
    let data: HistoryResponse = resp.json().await?;
    Ok(data.primary_market_price)
}
