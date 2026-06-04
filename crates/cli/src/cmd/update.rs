//! `rwa update` — in-place self-update from the latest GitHub Release.

use eyre::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

#[derive(Serialize)]
struct UpdateJson {
    status: &'static str,
    current: String,
    latest: String,
    target: &'static str,
}

const API_BASE: &str = "https://api.github.com";
const REPO_PATH: &str = "/repos/outputlayer/rwa_cli/releases/latest";

/// Build the HTTP client. GitHub's API requires a User-Agent.
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("rwa-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("failed to build HTTP client")
}

/// GET the latest release tag. `api_base` is injectable for tests.
async fn fetch_latest_tag(client: &reqwest::Client, api_base: &str) -> Result<String> {
    let url = format!("{api_base}{REPO_PATH}");
    let resp = client.get(url).send().await.map_err(|e| {
        UpdateError::new(UpdateErrorKind::Network, format!("GitHub API request failed: {e}"))
    })?;
    let status = resp.status();

    // GitHub signals primary rate limiting with 429, or with 403 plus an
    // `x-ratelimit-remaining: 0` header. A bare 403 (no such header) is a
    // generic access failure, not rate limiting.
    let rate_limited = status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || (status == reqwest::StatusCode::FORBIDDEN
            && resp
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok())
                == Some("0"));
    if rate_limited {
        return Err(UpdateError::new(
            UpdateErrorKind::RateLimited,
            "GitHub API rate limit hit; try again in a few minutes",
        )
        .into());
    }
    if !status.is_success() {
        return Err(UpdateError::new(
            UpdateErrorKind::Network,
            format!("GitHub API returned HTTP {status}"),
        )
        .into());
    }
    let body = resp.text().await.map_err(|e| {
        UpdateError::new(UpdateErrorKind::Network, format!("failed to read API response: {e}"))
    })?;
    parse_latest_tag(&body).map_err(|e| {
        UpdateError::new(UpdateErrorKind::Network, format!("unexpected GitHub API response: {e}"))
            .into()
    })
}

/// Download the archive + SHA256SUMS.txt from `download_base`, verify the
/// checksum, and extract the inner binary into `workdir`. Returns the path to
/// the extracted binary. Checksum is verified BEFORE extraction (fail-closed).
async fn download_verify_extract(
    client: &reqwest::Client,
    download_base: &str,
    spec: &AssetSpec,
    workdir: &Path,
) -> Result<PathBuf> {
    let archive_bytes = http_get_bytes(client, &format!("{download_base}/{}", spec.archive)).await?;
    let manifest = http_get_text(client, &format!("{download_base}/SHA256SUMS.txt")).await?;

    verify_sha256(&archive_bytes, &manifest, &spec.archive)?;

    std::fs::create_dir_all(workdir)?;
    let archive_path = workdir.join(&spec.archive);
    std::fs::write(&archive_path, &archive_bytes)?;
    extract_binary(&archive_path, workdir, spec.inner_bin)
}

/// GET `url` and return the response body bytes, mapping failures to `UpdateErrorKind::Network`.
async fn http_get_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let resp = client.get(url).send().await.map_err(|e| {
        UpdateError::new(UpdateErrorKind::Network, format!("download failed: {e}"))
    })?;
    if !resp.status().is_success() {
        return Err(UpdateError::new(
            UpdateErrorKind::Network,
            format!("download {url} returned HTTP {}", resp.status()),
        ).into());
    }
    Ok(resp.bytes().await.map_err(|e| {
        UpdateError::new(UpdateErrorKind::Network, format!("read body failed: {e}"))
    })?.to_vec())
}

/// GET `url` and return the response body as a UTF-8 (lossy) string.
async fn http_get_text(client: &reqwest::Client, url: &str) -> Result<String> {
    Ok(String::from_utf8_lossy(&http_get_bytes(client, url).await?).into_owned())
}

// Archive extraction: both `tar::Archive::unpack` and `zip::ZipArchive::extract`
// reject entries that resolve outside `dest` (tar/zip-slip protection) in their
// current resolved versions. This is backstopped by SHA-256 verification of the
// archive against the signed release manifest before we ever write to disk, and
// by the archives coming only from this repo's GitHub Releases over HTTPS.
#[cfg(unix)]
fn extract_binary(archive: &Path, dest: &Path, inner: &str) -> Result<PathBuf> {
    let file = std::fs::File::open(archive)?;
    let dec = flate2::read::GzDecoder::new(file);
    let mut ar = tar::Archive::new(dec);
    ar.unpack(dest)?;
    Ok(dest.join(inner))
}

#[cfg(windows)]
fn extract_binary(archive: &Path, dest: &Path, inner: &str) -> Result<PathBuf> {
    let file = std::fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)?;
    zip.extract(dest)?;
    Ok(dest.join(inner))
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

    #[test]
    fn update_json_shape_is_stable() {
        use serde_json::Value;
        let j = serde_json::to_value(UpdateJson {
            status: "update_available",
            current: "0.2.8".into(),
            latest: "0.2.9".into(),
            target: env!("RWA_TARGET"),
        })
        .unwrap();
        assert_eq!(j.pointer("/status"), Some(&Value::from("update_available")));
        assert_eq!(j.pointer("/current"), Some(&Value::from("0.2.8")));
        assert_eq!(j.pointer("/latest"), Some(&Value::from("0.2.9")));
        assert!(j.get("target").is_some());
    }

    #[tokio::test]
    async fn fetch_latest_tag_reads_tag_name_from_server() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let _m = server.mock_async(|when, then| {
            when.method(GET).path("/repos/outputlayer/rwa_cli/releases/latest");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({"tag_name": "v0.9.9"}));
        }).await;

        let client = http_client();
        let tag = fetch_latest_tag(&client, &server.base_url()).await.unwrap();
        assert_eq!(tag, "v0.9.9");
    }

    #[tokio::test]
    async fn fetch_latest_tag_maps_rate_limit_to_kind() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let _m = server.mock_async(|when, then| {
            when.method(GET).path("/repos/outputlayer/rwa_cli/releases/latest");
            then.status(403)
                .header("x-ratelimit-remaining", "0")
                .body("rate limited");
        }).await;

        let client = http_client();
        let err = fetch_latest_tag(&client, &server.base_url()).await.unwrap_err();
        let ue = err.downcast_ref::<UpdateError>().expect("typed update error");
        assert_eq!(ue.kind, UpdateErrorKind::RateLimited);
    }

    #[tokio::test]
    async fn fetch_latest_tag_maps_server_error_to_network() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let _m = server.mock_async(|when, then| {
            when.method(GET).path("/repos/outputlayer/rwa_cli/releases/latest");
            then.status(500).body("boom");
        }).await;

        let client = http_client();
        let err = fetch_latest_tag(&client, &server.base_url()).await.unwrap_err();
        let ue = err.downcast_ref::<UpdateError>().expect("typed update error");
        assert_eq!(ue.kind, UpdateErrorKind::Network);
    }

    #[tokio::test]
    async fn fetch_latest_tag_maps_bad_body_to_network() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let _m = server.mock_async(|when, then| {
            when.method(GET).path("/repos/outputlayer/rwa_cli/releases/latest");
            then.status(200).json_body(serde_json::json!({}));
        }).await;

        let client = http_client();
        let err = fetch_latest_tag(&client, &server.base_url()).await.unwrap_err();
        let ue = err.downcast_ref::<UpdateError>().expect("typed update error");
        assert_eq!(ue.kind, UpdateErrorKind::Network);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn download_verify_extract_round_trip() {
        use httpmock::prelude::*;
        use sha2::{Digest, Sha256};

        let mut tar_buf = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut tar_buf, flate2::Compression::default());
            let mut builder = tar::Builder::new(enc);
            let payload = b"#!/bin/sh\necho updated\n";
            let mut header = tar::Header::new_gnu();
            header.set_path("rwa").unwrap();
            header.set_size(payload.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append(&header, &payload[..]).unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        let digest = {
            let mut h = Sha256::new();
            h.update(&tar_buf);
            hex::encode(h.finalize())
        };
        let archive_name = "rwa-x86_64-unknown-linux-gnu.tar.gz";
        let manifest = format!("{digest}  {archive_name}\n");

        let server = MockServer::start_async().await;
        let tb = tar_buf.clone();
        let _a = server.mock_async(|when, then| {
            when.method(GET).path(format!("/{archive_name}"));
            then.status(200).body(tb);
        }).await;
        let _s = server.mock_async(|when, then| {
            when.method(GET).path("/SHA256SUMS.txt");
            then.status(200).body(manifest);
        }).await;

        let client = http_client();
        let spec = AssetSpec { archive: archive_name.to_string(), inner_bin: "rwa" };
        let workdir = std::env::temp_dir().join(format!("rwa-update-test-{}", std::process::id()));
        let bin = download_verify_extract(&client, &server.base_url(), &spec, &workdir).await.unwrap();

        let extracted = std::fs::read(&bin).unwrap();
        assert_eq!(extracted, b"#!/bin/sh\necho updated\n");
        std::fs::remove_dir_all(&workdir).ok();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn download_verify_extract_aborts_on_bad_checksum() {
        use httpmock::prelude::*;
        let archive_name = "rwa-x86_64-unknown-linux-gnu.tar.gz";
        let server = MockServer::start_async().await;
        let _a = server.mock_async(|when, then| {
            when.method(GET).path(format!("/{archive_name}"));
            then.status(200).body(b"not a real archive".to_vec());
        }).await;
        let bad_hash = "0".repeat(64);
        let _s = server.mock_async(|when, then| {
            when.method(GET).path("/SHA256SUMS.txt");
            then.status(200).body(format!("{bad_hash}  {archive_name}\n"));
        }).await;

        let client = http_client();
        let spec = AssetSpec { archive: archive_name.to_string(), inner_bin: "rwa" };
        let workdir = std::env::temp_dir().join(format!("rwa-update-bad-{}", std::process::id()));
        let err = download_verify_extract(&client, &server.base_url(), &spec, &workdir).await.unwrap_err();
        let ue = err.downcast_ref::<UpdateError>().expect("typed");
        assert_eq!(ue.kind, UpdateErrorKind::ChecksumMismatch);
        assert!(!workdir.join("rwa").exists(), "must not extract on checksum failure");
        std::fs::remove_dir_all(&workdir).ok();
    }
}
