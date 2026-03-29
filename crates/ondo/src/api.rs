use eyre::{eyre, Result};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::HTTP;

const ONDO_API_URL: &str = "https://app.ondo.finance/api/v2/assets";
const ONDO_SESSION_URL: &str = "https://status.ondo.finance/api/limits/session";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OndoErrorKind {
    Network,
    HttpStatus,
    Decode,
    MissingData,
    InvalidData,
}

#[derive(Debug)]
pub struct OndoError {
    pub kind: OndoErrorKind,
    pub endpoint: String,
    pub status: Option<StatusCode>,
    pub detail: String,
}

impl OndoError {
    fn new(
        kind: OndoErrorKind,
        endpoint: impl Into<String>,
        status: Option<StatusCode>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            endpoint: endpoint.into(),
            status,
            detail: detail.into(),
        }
    }

    #[must_use]
    pub fn is_retryable(&self) -> bool {
        match self.kind {
            OndoErrorKind::Network => true,
            OndoErrorKind::HttpStatus => self
                .status
                .map(|status| status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error())
                .unwrap_or(false),
            OndoErrorKind::Decode | OndoErrorKind::MissingData | OndoErrorKind::InvalidData => {
                false
            }
        }
    }
}

impl std::fmt::Display for OndoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(status) => write!(
                f,
                "Ondo {} error [{}] ({}): {}",
                self.endpoint, self.kind, status, self.detail
            ),
            None => write!(
                f,
                "Ondo {} error [{}]: {}",
                self.endpoint, self.kind, self.detail
            ),
        }
    }
}

impl std::error::Error for OndoError {}

impl std::fmt::Display for OndoErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::Network => "network",
            Self::HttpStatus => "http_status",
            Self::Decode => "decode",
            Self::MissingData => "missing_data",
            Self::InvalidData => "invalid_data",
        };
        f.write_str(label)
    }
}

// ─── app.ondo.finance/api/v2/assets (free, no auth) ────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OndoAssetTag {
    pub category_slug: String,
    pub tag_label: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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
        self.tags
            .iter()
            .find(|t| t.category_slug == "sector-industry")
            .map(|t| t.tag_label.as_str())
    }

    /// Extract instrument type from tags ("Stock" or "ETF").
    pub fn instrument_type(&self) -> Option<&str> {
        self.tags
            .iter()
            .find(|t| t.category_slug == "instrument-type")
            .map(|t| t.tag_label.as_str())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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
/// Caches result to `~/.config/rwa/.cache/assets.json` (5-min TTL).
/// On network failure, falls back to cached data if available.
pub async fn fetch_assets() -> Result<Vec<OndoAsset>> {
    let cache = assets_cache_path();

    // Try live fetch first
    match fetch_assets_live().await {
        Ok(assets) => {
            // Update cache in background (best-effort)
            if let Some(ref path) = cache {
                let _ = write_cache(path, &assets);
            }
            Ok(assets)
        }
        Err(live_err) => {
            // Live fetch failed — try disk cache
            if let Some(assets) = cache.as_deref().and_then(read_cache) {
                Ok(assets)
            } else {
                Err(live_err)
            }
        }
    }
}

async fn fetch_assets_live() -> Result<Vec<OndoAsset>> {
    let resp = HTTP.get(ONDO_API_URL).send().await.map_err(|e| {
        OndoError::new(
            OndoErrorKind::Network,
            "assets",
            None,
            format!("request failed: {e}"),
        )
    })?;
    if !resp.status().is_success() {
        return Err(OndoError::new(
            OndoErrorKind::HttpStatus,
            "assets",
            Some(resp.status()),
            "non-success response",
        )
        .into());
    }
    let wrapper: AssetsResponse = resp.json().await.map_err(|e| {
        OndoError::new(
            OndoErrorKind::Decode,
            "assets",
            None,
            format!("failed to decode response body: {e}"),
        )
    })?;
    Ok(wrapper.assets)
}

fn assets_cache_path() -> Option<PathBuf> {
    let dir = dirs::config_dir()?.join("rwa").join(".cache");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("assets.json"))
}

fn write_cache(path: &Path, assets: &[OndoAsset]) -> Option<()> {
    let data = serde_json::to_string(assets).ok()?;
    std::fs::write(path, data).ok()
}

fn read_cache(path: &Path) -> Option<Vec<OndoAsset>> {
    let meta = std::fs::metadata(path).ok()?;
    let age = meta.modified().ok()?.elapsed().ok()?;
    // Accept cache up to 1 hour as fallback (fresh TTL is 5 min)
    if age > Duration::from_secs(3600) {
        return None;
    }
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Find an asset by symbol (case-insensitive, accepts "TSLA" or "TSLAon").
#[must_use]
pub fn find_asset<'a>(symbol: &str, assets: &'a [OndoAsset]) -> Option<&'a OndoAsset> {
    let normalized = symbol.to_uppercase();
    let lookup = if normalized.ends_with("ON") {
        normalized
    } else {
        format!("{normalized}ON")
    };
    assets.iter().find(|a| a.symbol.to_uppercase() == lookup)
}

fn invalid_market_data(field: &str, detail: impl Into<String>) -> eyre::Report {
    OndoError::new(OndoErrorKind::InvalidData, field, None, detail).into()
}

fn missing_market_data(field: &str, detail: impl Into<String>) -> eyre::Report {
    OndoError::new(OndoErrorKind::MissingData, field, None, detail).into()
}

fn parse_market_number(field: &str, s: &str, allow_negative: bool) -> Result<f64> {
    let value = s
        .parse::<f64>()
        .map_err(|_| invalid_market_data(field, format!("invalid number {s:?}")))?;
    if !value.is_finite() {
        return Err(invalid_market_data(field, format!("{s:?} is not finite")));
    }
    if !allow_negative && value < 0.0 {
        return Err(invalid_market_data(
            field,
            format!("{s:?} must be non-negative"),
        ));
    }
    Ok(value)
}

/// Parse a price string from Ondo market data.
pub fn parse_price(s: &str) -> Result<f64> {
    parse_market_number("price", s, false)
}

/// Parse a 24h change percentage from Ondo market data.
pub fn parse_change_pct(s: &str) -> Result<f64> {
    parse_market_number("price_change_pct_24h", s, true)
}

/// Parse primary market snapshot for a tokenized asset.
pub fn market_snapshot(asset: &OndoAsset) -> Result<(f64, f64)> {
    let pm = asset.primary_market.as_ref().ok_or_else(|| {
        missing_market_data(
            "market_snapshot",
            format!("missing primary market data for {}", asset.symbol),
        )
    })?;
    let price = parse_price(&pm.price)?;
    let pct_24h = match pm.price_change_pct_24h.as_deref() {
        Some(pct) => parse_change_pct(pct)?,
        None => 0.0,
    };
    Ok((price, pct_24h))
}

/// Find an asset by symbol and parse its primary market snapshot.
pub fn market_snapshot_for_symbol(symbol: &str, assets: &[OndoAsset]) -> Result<(f64, f64)> {
    let asset = find_asset(symbol, assets).ok_or_else(|| {
        missing_market_data(
            "market_snapshot",
            format!("missing asset metadata for {symbol}"),
        )
    })?;
    market_snapshot(asset)
}

// ─── Trading sessions ─────────────────────────────────────────────────────

/// Trading session name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    PreMarket,
    Regular,
    PostMarket,
    Overnight,
    Closed,
}

impl Session {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Session::PreMarket => "Pre-Market",
            Session::Regular => "Regular Market",
            Session::PostMarket => "Post-Market",
            Session::Overnight => "Overnight",
            Session::Closed => "Closed",
        }
    }

    #[must_use]
    pub fn hours(self) -> &'static str {
        match self {
            Session::PreMarket => "4:00 AM – 9:29 AM ET",
            Session::Regular => "9:30 AM – 3:59 PM ET",
            Session::PostMarket => "4:00 PM – 7:59 PM ET",
            Session::Overnight => "8:00 PM – 3:59 AM ET",
            Session::Closed => "Sat – Sun 8 PM ET",
        }
    }
}

/// Determine the current trading session based on ET time.
///
/// Sessions (EDT/EST):
///   Pre-Market:  4:00 AM – 9:29:59 AM
///   Regular:     9:30 AM – 3:59:59 PM
///   Post-Market: 4:00 PM – 7:59:59 PM
///   Overnight:   8:00 PM – 3:59:59 AM
///   Closed:      Saturday all day, Sunday before 8 PM, Friday after 8 PM
#[must_use]
pub fn current_session() -> Session {
    use chrono::{Datelike, Timelike};
    use chrono_tz::US::Eastern;

    let now = chrono::Utc::now().with_timezone(&Eastern);
    let wd = now.weekday();
    let hour = now.hour();

    // Weekend: closed
    if matches!(wd, chrono::Weekday::Sat)
        || (wd == chrono::Weekday::Sun && hour < 20)
        || (wd == chrono::Weekday::Fri && hour >= 20)
    {
        return Session::Closed;
    }

    match hour {
        4..=8 => Session::PreMarket,
        9 if now.minute() < 30 => Session::PreMarket,
        9 => Session::Regular,
        10..=15 => Session::Regular,
        16..=19 => Session::PostMarket,
        20..=23 => Session::Overnight,
        0..=3 => Session::Overnight,
        _ => Session::Closed,
    }
}

/// Session limits for a single token from Ondo status API.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLimits {
    pub symbol: String,
    #[serde(default)]
    pub premarket: Option<SessionInfo>,
    #[serde(default)]
    pub regular: Option<SessionInfo>,
    #[serde(default)]
    pub postmarket: Option<SessionInfo>,
    #[serde(default)]
    pub overnight: Option<SessionInfo>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub tradable: bool,
    pub max_attestation_count: Option<String>,
    pub max_active_notional_value: Option<String>,
}

impl SessionLimits {
    /// Check if this token is tradable in the given session.
    #[must_use]
    pub fn is_tradable(&self, session: Session) -> bool {
        let info = match session {
            Session::PreMarket => self.premarket.as_ref(),
            Session::Regular => self.regular.as_ref(),
            Session::PostMarket => self.postmarket.as_ref(),
            Session::Overnight => self.overnight.as_ref(),
            Session::Closed => return false,
        };
        info.map(|i| i.tradable).unwrap_or(false)
    }

    /// Max notional value for the given session.
    pub fn max_notional(&self, session: Session) -> Option<f64> {
        let info = match session {
            Session::PreMarket => self.premarket.as_ref(),
            Session::Regular => self.regular.as_ref(),
            Session::PostMarket => self.postmarket.as_ref(),
            Session::Overnight => self.overnight.as_ref(),
            Session::Closed => return None,
        };
        info.and_then(|i| i.max_active_notional_value.as_deref())
            .and_then(|v| v.parse().ok())
    }
}

#[derive(Debug, Deserialize)]
struct SessionResponse {
    limits: Vec<SessionLimits>,
}

/// Fetch session limits from Ondo status API.
pub async fn fetch_session_limits() -> Result<Vec<SessionLimits>> {
    let resp = HTTP.get(ONDO_SESSION_URL).send().await.map_err(|e| {
        OndoError::new(
            OndoErrorKind::Network,
            "session_limits",
            None,
            format!("request failed: {e}"),
        )
    })?;
    if !resp.status().is_success() {
        return Err(OndoError::new(
            OndoErrorKind::HttpStatus,
            "session_limits",
            Some(resp.status()),
            "non-success response",
        )
        .into());
    }
    let data: SessionResponse = resp.json().await.map_err(|e| {
        OndoError::new(
            OndoErrorKind::Decode,
            "session_limits",
            None,
            format!("failed to decode response body: {e}"),
        )
    })?;
    Ok(data.limits)
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_price ───────────────────────────────────────────

    #[test]
    fn parse_valid_price() {
        assert!((parse_price("385.75").unwrap() - 385.75).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_integer_price() {
        assert!((parse_price("100").unwrap() - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_invalid_price_is_error() {
        assert!(parse_price("N/A").is_err());
        assert!(parse_price("").is_err());
        assert!(parse_price("$100").is_err());
    }

    #[test]
    fn parse_change_pct_allows_negative_values() {
        assert!((parse_change_pct("-5.25").unwrap() + 5.25).abs() < f64::EPSILON);
    }

    #[test]
    fn parse_price_rejects_negative_values() {
        assert!(parse_price("-1").is_err());
    }

    #[test]
    fn parse_market_numbers_reject_non_finite_values() {
        assert!(parse_price("inf").is_err());
        assert!(parse_change_pct("NaN").is_err());
    }

    // ── find_asset ────────────────────────────────────────────

    #[test]
    fn find_by_symbol_with_suffix() {
        let assets = vec![OndoAsset {
            symbol: "TSLAon".into(),
            asset_name: "Tesla".into(),
            tags: vec![],
            primary_market: None,
        }];
        assert!(find_asset("TSLAon", &assets).is_some());
    }

    #[test]
    fn find_by_symbol_without_suffix() {
        let assets = vec![OndoAsset {
            symbol: "TSLAon".into(),
            asset_name: "Tesla".into(),
            tags: vec![],
            primary_market: None,
        }];
        assert!(find_asset("TSLA", &assets).is_some());
    }

    #[test]
    fn find_case_insensitive() {
        let assets = vec![OndoAsset {
            symbol: "TSLAon".into(),
            asset_name: "Tesla".into(),
            tags: vec![],
            primary_market: None,
        }];
        assert!(find_asset("tsla", &assets).is_some());
    }

    #[test]
    fn find_missing_returns_none() {
        let assets = vec![OndoAsset {
            symbol: "TSLAon".into(),
            asset_name: "Tesla".into(),
            tags: vec![],
            primary_market: None,
        }];
        assert!(find_asset("AAPL", &assets).is_none());
    }

    #[test]
    fn market_snapshot_parses_price_and_change() {
        let asset = OndoAsset {
            symbol: "TSLAon".into(),
            asset_name: "Tesla".into(),
            tags: vec![],
            primary_market: Some(PrimaryMarket {
                price: "385.75".into(),
                price_change_24h: Some("4.56".into()),
                price_change_pct_24h: Some("1.20".into()),
            }),
        };

        let (price, pct) = market_snapshot(&asset).unwrap();
        assert!((price - 385.75).abs() < f64::EPSILON);
        assert!((pct - 1.20).abs() < f64::EPSILON);
    }

    #[test]
    fn market_snapshot_for_symbol_fails_loudly_on_invalid_price() {
        let assets = vec![OndoAsset {
            symbol: "TSLAon".into(),
            asset_name: "Tesla".into(),
            tags: vec![],
            primary_market: Some(PrimaryMarket {
                price: "N/A".into(),
                price_change_24h: None,
                price_change_pct_24h: Some("1.20".into()),
            }),
        }];

        let err = market_snapshot_for_symbol("TSLA", &assets).unwrap_err();
        let typed = err.downcast_ref::<OndoError>().expect("typed Ondo error");
        assert_eq!(typed.kind, OndoErrorKind::InvalidData);
        assert_eq!(typed.endpoint, "price");
    }

    #[test]
    fn ondo_error_retryable_only_for_network_and_retryable_http() {
        let network = OndoError::new(OndoErrorKind::Network, "assets", None, "boom");
        let rate_limited = OndoError::new(
            OndoErrorKind::HttpStatus,
            "assets",
            Some(StatusCode::TOO_MANY_REQUESTS),
            "429",
        );
        let invalid = OndoError::new(OndoErrorKind::InvalidData, "price", None, "bad data");

        assert!(network.is_retryable());
        assert!(rate_limited.is_retryable());
        assert!(!invalid.is_retryable());
    }

    // ── Session ───────────────────────────────────────────────

    #[test]
    fn session_labels_non_empty() {
        for s in [
            Session::PreMarket,
            Session::Regular,
            Session::PostMarket,
            Session::Overnight,
            Session::Closed,
        ] {
            assert!(!s.label().is_empty());
            assert!(!s.hours().is_empty());
        }
    }

    #[test]
    fn session_limits_tradable() {
        let limits = SessionLimits {
            symbol: "TSLAon".into(),
            premarket: Some(SessionInfo {
                tradable: true,
                max_attestation_count: None,
                max_active_notional_value: None,
            }),
            regular: Some(SessionInfo {
                tradable: true,
                max_attestation_count: None,
                max_active_notional_value: None,
            }),
            postmarket: Some(SessionInfo {
                tradable: false,
                max_attestation_count: None,
                max_active_notional_value: None,
            }),
            overnight: None,
        };
        assert!(limits.is_tradable(Session::PreMarket));
        assert!(limits.is_tradable(Session::Regular));
        assert!(!limits.is_tradable(Session::PostMarket));
        assert!(!limits.is_tradable(Session::Overnight));
        assert!(!limits.is_tradable(Session::Closed));
    }

    #[test]
    fn session_max_notional() {
        let limits = SessionLimits {
            symbol: "TSLAon".into(),
            premarket: None,
            regular: Some(SessionInfo {
                tradable: true,
                max_attestation_count: None,
                max_active_notional_value: Some("50000".into()),
            }),
            postmarket: None,
            overnight: None,
        };
        assert_eq!(limits.max_notional(Session::Regular), Some(50000.0));
        assert_eq!(limits.max_notional(Session::PreMarket), None);
    }

    // ── OndoAsset helpers ─────────────────────────────────────

    #[test]
    fn sector_extraction() {
        let asset = OndoAsset {
            symbol: "TSLAon".into(),
            asset_name: "Tesla".into(),
            tags: vec![
                OndoAssetTag {
                    category_slug: "sector-industry".into(),
                    tag_label: "Technology".into(),
                },
                OndoAssetTag {
                    category_slug: "instrument-type".into(),
                    tag_label: "Stock".into(),
                },
            ],
            primary_market: None,
        };
        assert_eq!(asset.sector(), Some("Technology"));
        assert_eq!(asset.instrument_type(), Some("Stock"));
    }

    #[test]
    fn missing_tags_return_none() {
        let asset = OndoAsset {
            symbol: "TSLAon".into(),
            asset_name: "Tesla".into(),
            tags: vec![],
            primary_market: None,
        };
        assert_eq!(asset.sector(), None);
        assert_eq!(asset.instrument_type(), None);
    }
}
