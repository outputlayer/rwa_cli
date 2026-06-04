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

/// Extract `tag_name` from a GitHub `releases/latest` response body.
fn parse_latest_tag(body: &str) -> Result<String> {
    let v: serde_json::Value = serde_json::from_str(body)?;
    v.get("tag_name")
        .and_then(|t| t.as_str())
        .map(str::to_string)
        .ok_or_else(|| eyre::eyre!("GitHub API response had no tag_name"))
}

/// Verify `data`'s SHA-256 against the entry for `archive_name` in a
/// `sha256sum`-format manifest. Fail-closed: a missing entry or any mismatch
/// is an error — never a silent pass.
fn verify_sha256(data: &[u8], manifest: &str, archive_name: &str) -> std::result::Result<(), UpdateError> {
    use sha2::{Digest, Sha256};

    let expected = manifest
        .lines()
        .find_map(|line| {
            let (hash, name) = line.split_once("  ")?;
            (name.trim() == archive_name).then(|| hash.trim().to_lowercase())
        })
        .ok_or_else(|| UpdateError::new(
            UpdateErrorKind::ChecksumMismatch,
            format!("no checksum entry for {archive_name} in SHA256SUMS.txt"),
        ))?;

    let mut hasher = Sha256::new();
    hasher.update(data);
    let actual = hex::encode(hasher.finalize());

    if actual != expected {
        return Err(UpdateError::new(
            UpdateErrorKind::ChecksumMismatch,
            format!("checksum mismatch for {archive_name}: expected {expected}, got {actual}"),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateAction {
    UpToDate,
    ReportAvailable,
    Confirm,
    Perform,
}

/// Pure decision: given versions and flags, what should `run` do?
/// `check` = report only; `yes` = skip prompt; `json` = non-interactive.
fn decide(latest: &str, current: &str, check: bool, yes: bool, json: bool) -> UpdateAction {
    if !is_newer(latest, current) {
        return UpdateAction::UpToDate;
    }
    if check {
        return UpdateAction::ReportAvailable;
    }
    if yes {
        return UpdateAction::Perform;
    }
    if json {
        // No interactive prompt in JSON mode; require -y to actually replace.
        return UpdateAction::ReportAvailable;
    }
    UpdateAction::Confirm
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

    #[test]
    fn parse_latest_tag_extracts_tag_name() {
        let body = r#"{"tag_name":"v0.2.9","name":"v0.2.9","draft":false}"#;
        assert_eq!(parse_latest_tag(body).unwrap(), "v0.2.9");
    }

    #[test]
    fn parse_latest_tag_errors_without_tag_name() {
        assert!(parse_latest_tag(r#"{"message":"Not Found"}"#).is_err());
    }

    #[test]
    fn verify_sha256_accepts_matching_digest() {
        let data = b"hello rwa";
        let digest = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(data);
            hex::encode(h.finalize())
        };
        let manifest = format!("{digest}  rwa-x86_64-apple-darwin.tar.gz\n");
        assert!(verify_sha256(data, &manifest, "rwa-x86_64-apple-darwin.tar.gz").is_ok());
    }

    #[test]
    fn verify_sha256_rejects_tampered_bytes() {
        let manifest = "deadbeef  rwa-x86_64-apple-darwin.tar.gz\n";
        let err = verify_sha256(b"tampered", manifest, "rwa-x86_64-apple-darwin.tar.gz").unwrap_err();
        assert_eq!(err.kind, UpdateErrorKind::ChecksumMismatch);
    }

    #[test]
    fn verify_sha256_rejects_missing_manifest_entry() {
        let manifest = "deadbeef  some-other-file.tar.gz\n";
        let err = verify_sha256(b"x", manifest, "rwa-x86_64-apple-darwin.tar.gz").unwrap_err();
        assert_eq!(err.kind, UpdateErrorKind::ChecksumMismatch);
    }

    #[test]
    fn decide_up_to_date_when_not_newer() {
        assert_eq!(decide("0.2.9", "0.2.9", false, false, false), UpdateAction::UpToDate);
        assert_eq!(decide("0.2.8", "0.2.9", false, true, true), UpdateAction::UpToDate);
    }

    #[test]
    fn decide_check_only_reports() {
        assert_eq!(decide("0.3.0", "0.2.9", true, false, false), UpdateAction::ReportAvailable);
    }

    #[test]
    fn decide_json_without_yes_reports_only() {
        assert_eq!(decide("0.3.0", "0.2.9", false, false, true), UpdateAction::ReportAvailable);
    }

    #[test]
    fn decide_yes_performs() {
        assert_eq!(decide("0.3.0", "0.2.9", false, true, false), UpdateAction::Perform);
        assert_eq!(decide("0.3.0", "0.2.9", false, true, true), UpdateAction::Perform);
    }

    #[test]
    fn decide_human_without_yes_asks_to_confirm() {
        assert_eq!(decide("0.3.0", "0.2.9", false, false, false), UpdateAction::Confirm);
    }
}
