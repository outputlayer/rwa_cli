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

// ─── Trading sessions ─────────────────────────────────────────────────────

const ONDO_SESSION_URL: &str = "https://status.ondo.finance/api/limits/session";

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
    pub fn label(self) -> &'static str {
        match self {
            Session::PreMarket => "Pre-Market",
            Session::Regular => "Regular Market",
            Session::PostMarket => "Post-Market",
            Session::Overnight => "Overnight",
            Session::Closed => "Closed",
        }
    }

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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let resp = client.get(ONDO_SESSION_URL).send().await?;
    if !resp.status().is_success() {
        return Err(eyre!("Ondo session API returned status {}", resp.status()));
    }
    let data: SessionResponse = resp.json().await?;
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
