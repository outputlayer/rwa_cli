use eyre::{eyre, Result};
use serde::Deserialize;

use super::ONDO_API_URL;

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
        other => {
            return Err(eyre!(
                "Invalid range: {other}. Use: 1D, 1W, 1M, 3M, 1Y, ALL"
            ))
        }
    };

    let normalized = symbol.to_lowercase();
    let sym = if normalized.ends_with("on") {
        normalized
    } else {
        format!("{normalized}on")
    };

    // `RWA_ONDO_API_URL` overrides the base (test seam — not user-facing),
    // mirroring the assets endpoint.
    let base = std::env::var("RWA_ONDO_API_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| ONDO_API_URL.to_string());
    let url = format!("{base}/{sym}/history?range={range_param}");
    super::retry_with_backoff(3, || fetch_history_attempt(&url)).await
}

async fn fetch_history_attempt(url: &str) -> Result<Vec<HistoryCandle>> {
    let data: HistoryResponse = super::get_json(url, "history").await?;
    Ok(data.primary_market_price)
}
