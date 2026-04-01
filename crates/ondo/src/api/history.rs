use eyre::{eyre, Result};
use serde::Deserialize;

use crate::HTTP;
use super::error::{OndoError, OndoErrorKind};

const ONDO_API_URL: &str = "https://app.ondo.finance/api/v2/assets";

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

    let url = format!("{ONDO_API_URL}/{sym}/history?range={range_param}");
    let resp = HTTP.get(&url).send().await.map_err(|e| {
        OndoError::new(
            OndoErrorKind::Network,
            "history",
            None,
            format!("request failed: {e}"),
        )
    })?;
    if !resp.status().is_success() {
        return Err(OndoError::new(
            OndoErrorKind::HttpStatus,
            "history",
            Some(resp.status()),
            format!("symbol={sym}"),
        )
        .into());
    }
    let data: HistoryResponse = resp.json().await.map_err(|e| {
        OndoError::new(
            OndoErrorKind::Decode,
            "history",
            None,
            format!("failed to decode response body: {e}"),
        )
    })?;
    Ok(data.primary_market_price)
}
