use reqwest::StatusCode;

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
    pub(crate) fn new(
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

impl OndoError {
    /// Whether this failure looks like a temporary Ondo outage worth retrying
    /// later: everything `is_retryable()` covers PLUS HTTP 403 — these
    /// endpoints are public/unauthenticated, so a 403 is a WAF/rate block that
    /// lifts after a few minutes. Kept separate from `is_retryable()` so the
    /// in-process backoff loop doesn't hammer a blocked endpoint.
    #[must_use]
    pub fn is_outage(&self) -> bool {
        self.is_retryable()
            || (self.kind == OndoErrorKind::HttpStatus && self.status == Some(StatusCode::FORBIDDEN))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_only_for_network_and_retryable_http() {
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

    #[test]
    fn outage_adds_403_but_not_other_4xx_or_decode() {
        let mk = |kind, status: Option<StatusCode>| OndoError::new(kind, "assets", status, "x");
        assert!(mk(OndoErrorKind::HttpStatus, Some(StatusCode::FORBIDDEN)).is_outage());
        assert!(!mk(OndoErrorKind::HttpStatus, Some(StatusCode::FORBIDDEN)).is_retryable());
        assert!(mk(OndoErrorKind::Network, None).is_outage());
        assert!(!mk(OndoErrorKind::HttpStatus, Some(StatusCode::NOT_FOUND)).is_outage());
        assert!(!mk(OndoErrorKind::Decode, None).is_outage());
    }
}
