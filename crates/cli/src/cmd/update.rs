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

/// Parse a `MAJOR.MINOR.PATCH` version, tolerating a leading `v`.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().strip_prefix('v').unwrap_or(s.trim());
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// True when `latest` is strictly newer than `current`. Unparseable inputs
/// are treated as "not newer" (fail safe — never offer a bogus update).
fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// The release asset name + inner binary name for a target triple.
#[derive(Debug)]
struct AssetSpec {
    archive: String,
    inner_bin: &'static str,
}

/// Supported release targets (must match `.github/workflows/release.yml`).
const SUPPORTED_TARGETS: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
];

fn asset_spec(target: &str) -> std::result::Result<AssetSpec, UpdateError> {
    if !SUPPORTED_TARGETS.contains(&target) {
        return Err(UpdateError::new(
            UpdateErrorKind::NoReleaseAsset,
            format!("no pre-built release for {target}; build from source via `cargo install --git https://github.com/outputlayer/rwa_cli`"),
        ));
    }
    let windows = target.contains("windows");
    Ok(AssetSpec {
        archive: format!("rwa-{target}.{}", if windows { "zip" } else { "tar.gz" }),
        inner_bin: if windows { "rwa.exe" } else { "rwa" },
    })
}

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

    #[test]
    fn parse_version_strips_v_prefix_and_parses_triple() {
        assert_eq!(parse_version("v0.2.9"), Some((0, 2, 9)));
        assert_eq!(parse_version("0.2.9"), Some((0, 2, 9)));
        assert_eq!(parse_version("1.20.300"), Some((1, 20, 300)));
        assert_eq!(parse_version("garbage"), None);
        assert_eq!(parse_version("0.2"), None);
    }

    #[test]
    fn is_newer_compares_numerically_not_lexically() {
        assert!(is_newer("0.2.10", "0.2.9"));
        assert!(is_newer("0.3.0", "0.2.99"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.2.9", "0.2.9"));
        assert!(!is_newer("0.2.8", "0.2.9"));
    }

    #[test]
    fn asset_spec_maps_supported_triples() {
        let mac = asset_spec("aarch64-apple-darwin").unwrap();
        assert_eq!(mac.archive, "rwa-aarch64-apple-darwin.tar.gz");
        assert_eq!(mac.inner_bin, "rwa");

        let win = asset_spec("x86_64-pc-windows-msvc").unwrap();
        assert_eq!(win.archive, "rwa-x86_64-pc-windows-msvc.zip");
        assert_eq!(win.inner_bin, "rwa.exe");

        let lin = asset_spec("x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(lin.archive, "rwa-x86_64-unknown-linux-gnu.tar.gz");
    }

    #[test]
    fn asset_spec_rejects_unsupported_triple() {
        let err = asset_spec("aarch64-pc-windows-msvc").unwrap_err();
        assert_eq!(err.kind, UpdateErrorKind::NoReleaseAsset);
    }
}
