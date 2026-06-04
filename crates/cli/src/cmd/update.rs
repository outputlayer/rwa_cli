//! `rwa update` — in-place self-update from the latest GitHub Release.

use eyre::Result;

/// Stable, machine-readable failure kinds surfaced in `--json` output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateErrorKind {
    ChecksumMismatch,
    NoReleaseAsset,
    NotWritable,
    Network,
    RateLimited,
}

impl UpdateErrorKind {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::ChecksumMismatch => "checksum_mismatch",
            Self::NoReleaseAsset => "no_release_asset",
            Self::NotWritable => "not_writable",
            Self::Network => "network",
            Self::RateLimited => "rate_limited",
        }
    }
}

#[derive(Debug)]
pub struct UpdateError {
    pub kind: UpdateErrorKind,
    pub detail: String,
}

impl UpdateError {
    pub fn new(kind: UpdateErrorKind, detail: impl Into<String>) -> Self {
        Self { kind, detail: detail.into() }
    }
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "update error [{}]: {}", self.kind.label(), self.detail)
    }
}

impl std::error::Error for UpdateError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_kind_labels_are_stable() {
        assert_eq!(UpdateErrorKind::ChecksumMismatch.label(), "checksum_mismatch");
        assert_eq!(UpdateErrorKind::NoReleaseAsset.label(), "no_release_asset");
        assert_eq!(UpdateErrorKind::NotWritable.label(), "not_writable");
        assert_eq!(UpdateErrorKind::Network.label(), "network");
        assert_eq!(UpdateErrorKind::RateLimited.label(), "rate_limited");
    }
}
