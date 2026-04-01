use eyre::Result;
use serde::Deserialize;

use crate::HTTP;
use super::error::{OndoError, OndoErrorKind};

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
pub async fn fetch_session_limits(base_url: Option<&str>) -> Result<Vec<SessionLimits>> {
    let url = base_url.unwrap_or(ONDO_SESSION_URL);
    let resp = HTTP.get(url).send().await.map_err(|e| {
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn fetch_session_limits_parses_valid_response() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        server.mock_async(|when, then| {
            when.method(GET).path("/api/limits/session");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({
                    "limits": [
                        {"symbol": "TSLAon", "regular": {"tradable": true}},
                        {"symbol": "AAPLon", "regular": {"tradable": false}}
                    ]
                }));
        }).await;

        let url = format!("{}/api/limits/session", server.base_url());
        let limits = fetch_session_limits(Some(&url)).await.unwrap();

        assert_eq!(limits.len(), 2);
        assert_eq!(limits[0].symbol, "TSLAon");
        assert!(limits[0].is_tradable(Session::Regular));
        assert!(!limits[1].is_tradable(Session::Regular));
    }

    #[tokio::test]
    async fn fetch_session_limits_returns_err_on_http_500() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        server.mock_async(|when, then| {
            when.method(GET).path("/api/limits/session");
            then.status(500).body("Internal Server Error");
        }).await;

        let url = format!("{}/api/limits/session", server.base_url());
        assert!(fetch_session_limits(Some(&url)).await.is_err());
    }

    #[tokio::test]
    async fn fetch_session_limits_returns_err_on_malformed_json() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        server.mock_async(|when, then| {
            when.method(GET).path("/api/limits/session");
            then.status(200)
                .header("content-type", "application/json")
                .body("not json at all");
        }).await;

        let url = format!("{}/api/limits/session", server.base_url());
        assert!(fetch_session_limits(Some(&url)).await.is_err());
    }
}
