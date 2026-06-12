use chrono::Datelike;
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
            Session::Closed => "Weekend / NYSE holiday",
        }
    }
}

pub fn now_eastern() -> chrono::DateTime<chrono_tz::Tz> {
    chrono::Utc::now().with_timezone(&chrono_tz::US::Eastern)
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
    session_at(now_eastern())
}

pub fn next_session_start() -> chrono::DateTime<chrono_tz::Tz> {
    next_session_start_after(now_eastern())
}

fn session_at(now: chrono::DateTime<chrono_tz::Tz>) -> Session {
    use chrono::{Days, Timelike};

    let hour = now.hour();
    let minute = now.minute();

    let candidate = match hour {
        0..=3 => Some(Session::Overnight),
        4..=8 => Some(Session::PreMarket),
        9 if minute < 30 => Some(Session::PreMarket),
        9..=15 => Some(Session::Regular),
        16..=19 => Some(Session::PostMarket),
        20..=23 => Some(Session::Overnight),
        _ => None,
    };

    let Some(session) = candidate else {
        return Session::Closed;
    };

    let current_date = now.date_naive();
    let trade_date = if session == Session::Overnight && hour >= 20 {
        current_date
            .checked_add_days(Days::new(1))
            .unwrap_or(current_date)
    } else {
        current_date
    };

    if is_nyse_trading_day(trade_date) {
        session
    } else {
        Session::Closed
    }
}

fn next_session_start_after(
    now: chrono::DateTime<chrono_tz::Tz>,
) -> chrono::DateTime<chrono_tz::Tz> {
    use chrono::Days;
    use chrono_tz::US::Eastern;

    let base_date = now.date_naive();
    for offset in 0..14 {
        let trade_date = base_date
            .checked_add_days(Days::new(offset))
            .unwrap_or(base_date);
        if !is_nyse_trading_day(trade_date) {
            continue;
        }

        let previous_date = trade_date
            .checked_sub_days(Days::new(1))
            .unwrap_or(trade_date);
        let candidates = [
            eastern_datetime(Eastern, previous_date, 20, 0),
            eastern_datetime(Eastern, trade_date, 4, 0),
            eastern_datetime(Eastern, trade_date, 9, 30),
            eastern_datetime(Eastern, trade_date, 16, 0),
        ];

        if let Some(next_start) = candidates.into_iter().find(|dt| *dt > now) {
            return next_start;
        }
    }

    now
}

fn eastern_datetime(
    tz: chrono_tz::Tz,
    date: chrono::NaiveDate,
    hour: u32,
    minute: u32,
) -> chrono::DateTime<chrono_tz::Tz> {
    use chrono::TimeZone;

    let naive = date.and_hms_opt(hour, minute, 0).expect("valid eastern time");
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(dt) => dt,
        chrono::LocalResult::Ambiguous(early, _) => early,
        chrono::LocalResult::None => tz.from_utc_datetime(&naive),
    }
}

fn is_nyse_trading_day(date: chrono::NaiveDate) -> bool {
    !matches!(date.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun)
        && !is_nyse_holiday(date)
}

fn is_nyse_holiday(date: chrono::NaiveDate) -> bool {
    let year = date.year();

    date == observed_new_years_day(year)
        || date == nth_weekday_of_month(year, 1, chrono::Weekday::Mon, 3)
        || date == nth_weekday_of_month(year, 2, chrono::Weekday::Mon, 3)
        || date == good_friday(year)
        || date == last_weekday_of_month(year, 5, chrono::Weekday::Mon)
        || date == observed_fixed_holiday(year, 6, 19)
        || date == observed_fixed_holiday(year, 7, 4)
        || date == nth_weekday_of_month(year, 9, chrono::Weekday::Mon, 1)
        || date == nth_weekday_of_month(year, 11, chrono::Weekday::Thu, 4)
        || date == observed_fixed_holiday(year, 12, 25)
}

fn observed_new_years_day(year: i32) -> chrono::NaiveDate {
    let jan1 = chrono::NaiveDate::from_ymd_opt(year, 1, 1).expect("valid date");
    match jan1.weekday() {
        chrono::Weekday::Sun => jan1.succ_opt().expect("next day"),
        chrono::Weekday::Sat => jan1,
        _ => jan1,
    }
}

fn observed_fixed_holiday(year: i32, month: u32, day: u32) -> chrono::NaiveDate {
    let holiday = chrono::NaiveDate::from_ymd_opt(year, month, day).expect("valid date");
    match holiday.weekday() {
        chrono::Weekday::Sat => holiday.pred_opt().expect("previous day"),
        chrono::Weekday::Sun => holiday.succ_opt().expect("next day"),
        _ => holiday,
    }
}

fn nth_weekday_of_month(
    year: i32,
    month: u32,
    weekday: chrono::Weekday,
    nth: u8,
) -> chrono::NaiveDate {
    let first = chrono::NaiveDate::from_ymd_opt(year, month, 1).expect("valid date");
    let delta = (7 + weekday.num_days_from_monday() as i64
        - first.weekday().num_days_from_monday() as i64)
        % 7;
    first
        .checked_add_days(chrono::Days::new(delta as u64 + 7 * u64::from(nth - 1)))
        .expect("weekday in month")
}

fn last_weekday_of_month(
    year: i32,
    month: u32,
    weekday: chrono::Weekday,
) -> chrono::NaiveDate {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let first_of_next = chrono::NaiveDate::from_ymd_opt(next_year, next_month, 1).expect("valid date");
    let last = first_of_next.pred_opt().expect("previous day");
    let delta = (7 + last.weekday().num_days_from_monday() as i64
        - weekday.num_days_from_monday() as i64)
        % 7;
    last.checked_sub_days(chrono::Days::new(delta as u64))
        .expect("weekday in month")
}

fn good_friday(year: i32) -> chrono::NaiveDate {
    easter_sunday(year)
        .checked_sub_days(chrono::Days::new(2))
        .expect("good friday")
}

fn easter_sunday(year: i32) -> chrono::NaiveDate {
    let a = year % 19;
    let b = year / 100;
    let c = year % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let month = (h + l - 7 * m + 114) / 31;
    let day = ((h + l - 7 * m + 114) % 31) + 1;
    chrono::NaiveDate::from_ymd_opt(year, month as u32, day as u32).expect("valid easter")
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
/// `RWA_ONDO_SESSION_URL` overrides the endpoint (test seam — not user-facing).
pub async fn fetch_session_limits(base_url: Option<&str>) -> Result<Vec<SessionLimits>> {
    let env_url = std::env::var("RWA_ONDO_SESSION_URL")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let url = base_url.or(env_url.as_deref()).unwrap_or(ONDO_SESSION_URL);
    super::retry_with_backoff(3, || fetch_session_limits_attempt(url)).await
}

async fn fetch_session_limits_attempt(url: &str) -> Result<Vec<SessionLimits>> {
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

    #[test]
    fn good_friday_is_closed_all_day() {
        use chrono::TimeZone;
        use chrono_tz::US::Eastern;

        let morning = Eastern.with_ymd_and_hms(2026, 4, 3, 10, 0, 0).single().unwrap();
        assert_eq!(session_at(morning), Session::Closed);
    }

    #[test]
    fn overnight_before_good_friday_is_closed() {
        use chrono::TimeZone;
        use chrono_tz::US::Eastern;

        let holiday_eve_overnight = Eastern.with_ymd_and_hms(2026, 4, 2, 20, 30, 0).single().unwrap();
        assert_eq!(session_at(holiday_eve_overnight), Session::Closed);
    }

    #[test]
    fn sunday_evening_before_trading_day_opens_overnight() {
        use chrono::TimeZone;
        use chrono_tz::US::Eastern;

        let sunday_evening = Eastern.with_ymd_and_hms(2026, 4, 5, 20, 1, 0).single().unwrap();
        assert_eq!(session_at(sunday_evening), Session::Overnight);
    }

    #[test]
    fn next_session_start_skips_good_friday_to_sunday_evening() {
        use chrono::TimeZone;
        use chrono_tz::US::Eastern;

        let holiday_eve = Eastern.with_ymd_and_hms(2026, 4, 2, 20, 53, 0).single().unwrap();
        let next = next_session_start_after(holiday_eve);
        assert_eq!(next, Eastern.with_ymd_and_hms(2026, 4, 5, 20, 0, 0).single().unwrap());
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
