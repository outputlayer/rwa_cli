//! Headless Oracle signed market-state attestation.
//!
//! Verifies a fail-closed Ed25519-signed receipt from `headlessoracle.com`
//! that attests whether an exchange is open right now. Used as an optional
//! pre-trade gate to catch real-time HALTED conditions (regulatory halts,
//! LULD circuit breakers, exchange outages) that the local NYSE-calendar
//! schedule and Ondo's per-symbol `tradable` flag cannot detect.
//!
//! Opt-in via `RWA_VERIFY_HALT`; off by default. See `usecases::gm_internal`
//! for the soft-vs-strict policy applied at the call site.
//!
//! Canonical payload (per `https://headlessoracle.com/v5/keys`): every
//! signed field except `signature`, keys sorted alphabetically,
//! `serde_json` default minified output (no whitespace), UTF-8 encoded.
//! Wrapper fields `receipt` and `discovery_url` on the response are unsigned
//! and must be excluded from the canonical payload.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use eyre::{Result, eyre};
use serde::Deserialize;
use serde_json::Value;

use crate::HTTP;
use super::error::{OndoError, OndoErrorKind};

/// Production Headless Oracle endpoint. Override per-call via the `base_url`
/// argument to `verify_market_open` (used by httpmock-based tests).
const HEADLESS_ORACLE_BASE_URL: &str = "https://headlessoracle.com";

/// Active signing key — `key_2026_v1`. Hex-encoded 32-byte Ed25519 public key.
/// Sourced from `https://headlessoracle.com/v5/keys` on 2026-06-08.
const HEADLESS_ORACLE_PUBKEY_HEX: &str =
    "03dc27993a2c90856cdeb45e228ac065f18f69f0933c917b2336c1e75712f178";

/// Wrapper fields the HO response decorates the signed receipt with — they
/// MUST be stripped before canonicalization.
const UNSIGNED_WRAPPER_FIELDS: &[&str] = &["receipt", "discovery_url"];

/// Attested market state. Mirrors the `status` field of a verified receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltAttestation {
    /// Exchange is open per signed attestation. Trade may proceed.
    Open,
    /// Exchange is closed (scheduled close, weekend, holiday).
    Closed,
    /// Exchange is halted (regulatory halt, circuit breaker, exchange outage).
    Halted,
    /// Oracle could not determine state. Fail-closed: treat as not-open.
    Unknown,
}

impl HaltAttestation {
    fn from_status(status: &str) -> Self {
        match status {
            "OPEN" => HaltAttestation::Open,
            "CLOSED" => HaltAttestation::Closed,
            "HALTED" => HaltAttestation::Halted,
            _ => HaltAttestation::Unknown,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            HaltAttestation::Open => "OPEN",
            HaltAttestation::Closed => "CLOSED",
            HaltAttestation::Halted => "HALTED",
            HaltAttestation::Unknown => "UNKNOWN",
        }
    }
}

/// Parsed signed receipt. Captures every field referenced by the canonical
/// payload spec on 2026-06-08; additional fields the server adds in future
/// flow through `canonical_payload` via the dynamic `Value` reconstruction,
/// so this struct only needs to expose what callers actually read.
#[derive(Debug, Clone, Deserialize)]
pub struct VerifiedReceipt {
    pub mic: String,
    pub status: String,
    pub issued_at: String,
    pub expires_at: String,
    pub public_key_id: String,
}

/// Verify and parse a signed receipt JSON value against the production
/// Headless Oracle public key. Returns the parsed receipt on success.
///
/// Fails on: bad signature hex, bad pubkey hex, signature mismatch, missing
/// required fields. Does NOT check TTL — see [`check_ttl`].
pub fn verify_receipt_signature(value: &Value) -> Result<VerifiedReceipt> {
    verify_receipt_signature_with_key(value, HEADLESS_ORACLE_PUBKEY_HEX)
}

/// Verify against an arbitrary hex-encoded Ed25519 public key. `pub(crate)`
/// because only tests use this — production `verify_receipt_signature` is
/// pinned to the published HO key. Exposing it crate-wide lets sibling test
/// modules (e.g. `usecases::gm_internal`) inject an ephemeral key for
/// deterministic round-trip coverage without bypassing the production binding.
pub(crate) fn verify_receipt_signature_with_key(
    value: &Value,
    public_key_hex: &str,
) -> Result<VerifiedReceipt> {
    let obj = value
        .as_object()
        .ok_or_else(|| eyre!("receipt must be a JSON object"))?;

    let signature_hex = obj
        .get("signature")
        .and_then(Value::as_str)
        .ok_or_else(|| eyre!("receipt missing `signature` field"))?;

    let sig_bytes_vec = hex::decode(signature_hex)
        .map_err(|e| eyre!("signature is not valid hex: {e}"))?;
    let sig_bytes: [u8; 64] = sig_bytes_vec
        .as_slice()
        .try_into()
        .map_err(|_| eyre!("signature must be 64 bytes, got {}", sig_bytes_vec.len()))?;
    let signature = Signature::from_bytes(&sig_bytes);

    let pk_bytes_vec = hex::decode(public_key_hex)
        .map_err(|e| eyre!("public key is not valid hex: {e}"))?;
    let pk_bytes: [u8; 32] = pk_bytes_vec
        .as_slice()
        .try_into()
        .map_err(|_| eyre!("public key must be 32 bytes, got {}", pk_bytes_vec.len()))?;
    let verifying_key = VerifyingKey::from_bytes(&pk_bytes)
        .map_err(|e| eyre!("invalid Ed25519 public key bytes: {e}"))?;

    let canonical = canonical_payload(value)?;
    verifying_key
        .verify(canonical.as_bytes(), &signature)
        .map_err(|e| eyre!("Ed25519 signature verification failed: {e}"))?;

    let receipt: VerifiedReceipt = serde_json::from_value(value.clone())
        .map_err(|e| eyre!("verified receipt missing required field: {e}"))?;
    Ok(receipt)
}

/// Reconstruct the canonical signing payload: receipt JSON minus `signature`
/// and unsigned wrapper fields, keys sorted alphabetically, no whitespace.
fn canonical_payload(value: &Value) -> Result<String> {
    let obj = value
        .as_object()
        .ok_or_else(|| eyre!("receipt must be a JSON object"))?;
    let mut sorted: BTreeMap<&str, &Value> = BTreeMap::new();
    for (k, v) in obj {
        if k == "signature" {
            continue;
        }
        if UNSIGNED_WRAPPER_FIELDS.contains(&k.as_str()) {
            continue;
        }
        sorted.insert(k.as_str(), v);
    }
    serde_json::to_string(&sorted)
        .map_err(|e| eyre!("failed to encode canonical payload: {e}"))
}

/// Check that `now` is before `receipt.expires_at`. Returns `Err` if the
/// receipt is expired or `expires_at` is unparseable.
pub fn check_ttl(receipt: &VerifiedReceipt, now: DateTime<Utc>) -> Result<()> {
    let expires_at = DateTime::parse_from_rfc3339(&receipt.expires_at)
        .map_err(|e| eyre!("receipt `expires_at` is not a valid RFC3339 timestamp: {e}"))?
        .with_timezone(&Utc);
    if now >= expires_at {
        return Err(eyre!(
            "receipt expired at {} (current time: {})",
            receipt.expires_at,
            now.to_rfc3339()
        ));
    }
    Ok(())
}

/// Fetch a signed market-state receipt from Headless Oracle, verify the
/// signature, and check the TTL against current wall-clock time.
///
/// Endpoint selection: if `RWA_HEADLESS_KEY` is set (non-empty), hits the
/// authenticated production endpoint `/v5/status` with `X-Oracle-Key`.
/// Otherwise falls back to the keyless `/v5/demo` endpoint — zero-signup
/// eval path, same signed-receipt shape, identical verifier.
///
/// `base_url` overrides the production host (used by tests via `httpmock`).
/// Network and HTTP errors surface as retryable `OndoError` so the caller's
/// soft-vs-strict policy can distinguish them from a signed HALTED response.
pub async fn verify_market_open(mic: &str, base_url: Option<&str>) -> Result<HaltAttestation> {
    // Edge: read `RWA_HEADLESS_KEY` here and thread it down. Keeping the env
    // read out of `verify_market_open_with_key` lets tests drive the keyed
    // /v5/status branch deterministically without process-global env mutation.
    let api_key = std::env::var("RWA_HEADLESS_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty());
    verify_market_open_with_key(mic, base_url, HEADLESS_ORACLE_PUBKEY_HEX, api_key.as_deref()).await
}

/// Key-injected variant — `pub(crate)` so sibling test modules can drive the
/// MIC-mismatch, header/endpoint routing, and `status` enum-mapping branches
/// deterministically with an ephemeral signing key. Production always flows
/// through `verify_market_open` which pins the published HO public key.
///
/// `api_key`: `Some(k)` → hits `/v5/status` with `X-Oracle-Key: k`; `None` →
/// hits the keyless `/v5/demo` fallback.
pub(crate) async fn verify_market_open_with_key(
    mic: &str,
    base_url: Option<&str>,
    public_key_hex: &str,
    api_key: Option<&str>,
) -> Result<HaltAttestation> {
    let base = base_url.unwrap_or(HEADLESS_ORACLE_BASE_URL);
    // Production: `/v5/status` with X-Oracle-Key. Keyless eval fallback: `/v5/demo`.
    // Same signed-receipt shape on both, so the verifier path is identical.
    let (url, op_tag) = match api_key {
        Some(_) => (format!("{base}/v5/status?mic={mic}"), "headless_oracle/status"),
        None => (format!("{base}/v5/demo?mic={mic}"), "headless_oracle/demo"),
    };

    let mut req = HTTP.get(&url).timeout(Duration::from_secs(5));
    if let Some(key) = api_key {
        req = req.header("X-Oracle-Key", key);
    }

    let resp = req.send().await.map_err(|e| {
        OndoError::new(
            OndoErrorKind::Network,
            op_tag,
            None,
            format!("request failed: {e}"),
        )
    })?;

    if !resp.status().is_success() {
        return Err(OndoError::new(
            OndoErrorKind::HttpStatus,
            op_tag,
            Some(resp.status()),
            "non-success response",
        )
        .into());
    }

    let value: Value = resp.json().await.map_err(|e| {
        OndoError::new(
            OndoErrorKind::Decode,
            op_tag,
            None,
            format!("failed to decode response body: {e}"),
        )
    })?;

    let receipt = verify_receipt_signature_with_key(&value, public_key_hex)?;
    check_ttl(&receipt, Utc::now())?;

    if !receipt.mic.eq_ignore_ascii_case(mic) {
        return Err(eyre!(
            "receipt MIC mismatch: requested {mic}, got {}",
            receipt.mic
        ));
    }

    Ok(HaltAttestation::from_status(&receipt.status))
}

#[cfg(test)]
mod tests {
    use super::*;

    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;
    use serde_json::json;

    /// Live receipt captured from `https://headlessoracle.com/v5/demo?mic=XNYS`
    /// on 2026-06-08 11:19:09 UTC. Proves the canonicalization + Ed25519
    /// verification match the production signer for the current schema
    /// (`schema_version: v5.0`, 11 signed fields incl. `halt_detection`).
    /// TTL not checked — the receipt is long expired.
    const LIVE_RECEIPT_2026_06_08: &str = r#"{"receipt_id":"06894d3d-9ac7-41e1-9d5c-90867d195370","issued_at":"2026-06-08T11:19:09.432Z","expires_at":"2026-06-08T11:20:09.432Z","issuer":"headlessoracle.com","mic":"XNYS","status":"CLOSED","source":"SCHEDULE","halt_detection":"active","receipt_mode":"demo","schema_version":"v5.0","public_key_id":"key_2026_v1","signature":"7396c686c54d63b4738d95429ef4644ce0ba0477a2e77e0640ff960454adb65aba59191b8a256b5410ade9e7a57f3bf53dd75142fe98813061019f0730173e03","receipt":{"receipt_id":"06894d3d-9ac7-41e1-9d5c-90867d195370","issued_at":"2026-06-08T11:19:09.432Z","expires_at":"2026-06-08T11:20:09.432Z","issuer":"headlessoracle.com","mic":"XNYS","status":"CLOSED","source":"SCHEDULE","halt_detection":"active","receipt_mode":"demo","schema_version":"v5.0","public_key_id":"key_2026_v1","signature":"7396c686c54d63b4738d95429ef4644ce0ba0477a2e77e0640ff960454adb65aba59191b8a256b5410ade9e7a57f3bf53dd75142fe98813061019f0730173e03"},"discovery_url":"https://headlessoracle.com/.well-known/mcp/server-card.json"}"#;

    #[test]
    fn verifies_signature_on_real_production_receipt() {
        let value: Value = serde_json::from_str(LIVE_RECEIPT_2026_06_08).unwrap();
        let receipt = verify_receipt_signature(&value)
            .expect("signature on live receipt must verify");
        assert_eq!(receipt.mic, "XNYS");
        assert_eq!(receipt.status, "CLOSED");
        assert_eq!(receipt.public_key_id, "key_2026_v1");
    }

    #[test]
    fn canonical_payload_excludes_signature_and_wrapper_fields() {
        let value: Value = serde_json::from_str(LIVE_RECEIPT_2026_06_08).unwrap();
        let canonical = canonical_payload(&value).unwrap();
        assert!(!canonical.contains("\"signature\""));
        assert!(!canonical.contains("\"receipt\":{"));
        assert!(!canonical.contains("\"discovery_url\""));
        // Must contain every signed field listed in /v5/keys canonical_payload_spec.
        for field in [
            "expires_at",
            "halt_detection",
            "issued_at",
            "issuer",
            "mic",
            "public_key_id",
            "receipt_id",
            "receipt_mode",
            "schema_version",
            "source",
            "status",
        ] {
            assert!(canonical.contains(field), "canonical missing `{field}`");
        }
    }

    #[test]
    fn canonical_payload_sorts_keys_alphabetically() {
        let value = json!({
            "z_last": "z",
            "a_first": "a",
            "mic": "XNYS",
            "signature": "deadbeef",
        });
        let canonical = canonical_payload(&value).unwrap();
        let a_pos = canonical.find("a_first").unwrap();
        let mic_pos = canonical.find("mic").unwrap();
        let z_pos = canonical.find("z_last").unwrap();
        assert!(a_pos < mic_pos && mic_pos < z_pos, "keys not alphabetical: {canonical}");
    }

    #[test]
    fn tampered_payload_fails_verification() {
        let mut value: Value = serde_json::from_str(LIVE_RECEIPT_2026_06_08).unwrap();
        // Flip a single character in the status field — would let a HALTED be
        // read as OPEN if the signature didn't catch it.
        value.as_object_mut().unwrap().insert(
            "status".to_string(),
            Value::String("OPEN".to_string()),
        );
        let err = verify_receipt_signature(&value)
            .expect_err("tampered status must fail signature verification");
        assert!(err.to_string().contains("signature verification failed"));
    }

    #[test]
    fn tampered_signature_fails_verification() {
        let mut value: Value = serde_json::from_str(LIVE_RECEIPT_2026_06_08).unwrap();
        // Flip the leading byte of the signature hex (7 -> 8).
        let sig = value["signature"].as_str().unwrap().to_string();
        let mut bytes = sig.into_bytes();
        bytes[0] = b'8';
        value.as_object_mut().unwrap().insert(
            "signature".to_string(),
            Value::String(String::from_utf8(bytes).unwrap()),
        );
        let err = verify_receipt_signature(&value)
            .expect_err("tampered signature must fail");
        assert!(err.to_string().contains("signature verification failed"));
    }

    #[test]
    fn missing_signature_field_fails_fast() {
        let mut value: Value = serde_json::from_str(LIVE_RECEIPT_2026_06_08).unwrap();
        value.as_object_mut().unwrap().remove("signature");
        let err = verify_receipt_signature(&value).expect_err("missing signature must fail");
        assert!(err.to_string().contains("missing `signature`"));
    }

    #[test]
    fn round_trip_sign_and_verify_with_ephemeral_key() {
        // Sign with an ephemeral keypair, verify with our reconstruction.
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let verifying_key = signing_key.verifying_key();
        let pk_hex = hex::encode(verifying_key.to_bytes());

        let receipt_no_sig = json!({
            "expires_at": "2999-01-01T00:00:00.000Z",
            "halt_detection": "active",
            "issued_at": "2999-01-01T00:00:00.000Z",
            "issuer": "headlessoracle.com",
            "mic": "XNYS",
            "public_key_id": "test_key",
            "receipt_id": "00000000-0000-0000-0000-000000000000",
            "receipt_mode": "live",
            "schema_version": "v5.0",
            "source": "SCHEDULE",
            "status": "HALTED",
        });
        let canonical = canonical_payload(&receipt_no_sig).unwrap();
        let signature = signing_key.sign(canonical.as_bytes());
        let mut full = receipt_no_sig;
        full.as_object_mut().unwrap().insert(
            "signature".to_string(),
            Value::String(hex::encode(signature.to_bytes())),
        );

        let receipt = verify_receipt_signature_with_key(&full, &pk_hex)
            .expect("round-trip verification must succeed");
        assert_eq!(receipt.status, "HALTED");
        assert_eq!(receipt.mic, "XNYS");
    }

    #[test]
    fn check_ttl_rejects_expired_receipt() {
        let receipt = VerifiedReceipt {
            mic: "XNYS".into(),
            status: "OPEN".into(),
            issued_at: "2026-06-08T11:19:09.432Z".into(),
            expires_at: "2026-06-08T11:20:09.432Z".into(),
            public_key_id: "key_2026_v1".into(),
        };
        // Wall-clock now is well past 2026-06-08T11:20:09Z by construction.
        let later = DateTime::parse_from_rfc3339("2026-06-08T11:20:10.000Z")
            .unwrap()
            .with_timezone(&Utc);
        let err = check_ttl(&receipt, later).expect_err("expired receipt must be rejected");
        assert!(err.to_string().contains("expired"));
    }

    #[test]
    fn check_ttl_accepts_fresh_receipt() {
        let receipt = VerifiedReceipt {
            mic: "XNYS".into(),
            status: "OPEN".into(),
            issued_at: "2026-06-08T11:19:09.432Z".into(),
            expires_at: "2026-06-08T11:20:09.432Z".into(),
            public_key_id: "key_2026_v1".into(),
        };
        let within = DateTime::parse_from_rfc3339("2026-06-08T11:19:30.000Z")
            .unwrap()
            .with_timezone(&Utc);
        check_ttl(&receipt, within).expect("fresh receipt must pass TTL check");
    }

    // ── Fetch + verify via httpmock ────────────────────────────────────────

    #[tokio::test]
    async fn fetch_returns_ttl_error_for_expired_live_receipt() {
        use httpmock::prelude::*;
        // Serves the live production receipt (long expired). The signature
        // verifies against the pinned HO key — execution only reaches the
        // TTL check if signature verification passed. Asserts: full
        // fetch → JSON parse → signature verify → TTL check pipeline runs
        // end-to-end against the production key.
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/v5/demo");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(LIVE_RECEIPT_2026_06_08);
            })
            .await;

        let err = verify_market_open("XNYS", Some(&server.base_url()))
            .await
            .expect_err("TTL must reject the expired live receipt");
        let msg = err.to_string();
        assert!(msg.contains("expired"), "expected TTL error, got: {msg}");
    }

    #[tokio::test]
    async fn fetch_surfaces_network_error_as_retryable_ondo_error() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        server
            .mock_async(|when, then| {
                when.method(GET).path("/v5/demo");
                then.status(500).body("server is having a bad day");
            })
            .await;

        let err = verify_market_open("XNYS", Some(&server.base_url()))
            .await
            .expect_err("HTTP 500 must surface as an error");
        let ondo = err
            .chain()
            .find_map(|c| c.downcast_ref::<OndoError>())
            .expect("error chain must include an OndoError so the caller's soft-vs-strict policy can route it");
        assert!(ondo.is_retryable(), "5xx must classify as retryable");
    }

    #[tokio::test]
    async fn fetch_for_wrong_mic_returns_mic_mismatch_error() {
        use httpmock::prelude::*;
        // Drive the MIC-mismatch branch deterministically: sign a fresh
        // (non-expired) receipt for XNYS with an ephemeral key, then ask
        // verify_market_open_with_key to verify it for XNAS. Signature must
        // verify and TTL must pass, so the only remaining rejection path
        // is the MIC check at the end of verify_market_open_with_key.
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let pk_hex = hex::encode(signing_key.verifying_key().to_bytes());

        let receipt_no_sig = json!({
            "expires_at": "2999-01-01T00:00:00.000Z",
            "halt_detection": "active",
            "issued_at": "2999-01-01T00:00:00.000Z",
            "issuer": "headlessoracle.com",
            "mic": "XNYS",
            "public_key_id": "test_key",
            "receipt_id": "00000000-0000-0000-0000-000000000000",
            "receipt_mode": "live",
            "schema_version": "v5.0",
            "source": "SCHEDULE",
            "status": "OPEN",
        });
        let canonical = canonical_payload(&receipt_no_sig).unwrap();
        let signature = signing_key.sign(canonical.as_bytes());
        let mut full = receipt_no_sig;
        full.as_object_mut().unwrap().insert(
            "signature".to_string(),
            Value::String(hex::encode(signature.to_bytes())),
        );
        let body = serde_json::to_string(&full).unwrap();

        let server = MockServer::start_async().await;
        server
            .mock_async(move |when, then| {
                when.method(GET).path("/v5/demo");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(body.clone());
            })
            .await;

        let err = verify_market_open_with_key("XNAS", Some(&server.base_url()), &pk_hex, None)
            .await
            .expect_err("MIC mismatch must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("MIC mismatch"),
            "expected MIC mismatch error, got: {msg}"
        );
        assert!(
            msg.contains("XNAS") && msg.contains("XNYS"),
            "MIC error must name both requested and received MIC, got: {msg}"
        );
    }

    #[tokio::test]
    async fn keyed_path_uses_status_endpoint_and_sends_x_oracle_key_header() {
        use httpmock::prelude::*;
        // Asserts the new production routing end-to-end: when an API key is
        // provided, the request hits `/v5/status` (NOT `/v5/demo`) AND carries
        // `X-Oracle-Key: <key>`. The httpmock matcher only fires when BOTH
        // conditions hold, and `assert_async()` requires it to have fired
        // exactly once — so a single passing test pins URL + header together.
        // The signed OPEN receipt then flows through the full verifier so we
        // also confirm a successful keyed response maps to HaltAttestation::Open.
        let mut csprng = OsRng;
        let signing_key = SigningKey::generate(&mut csprng);
        let pk_hex = hex::encode(signing_key.verifying_key().to_bytes());

        let receipt_no_sig = json!({
            "expires_at": "2999-01-01T00:00:00.000Z",
            "halt_detection": "active",
            "issued_at": "2999-01-01T00:00:00.000Z",
            "issuer": "headlessoracle.com",
            "mic": "XNYS",
            "public_key_id": "test_key",
            "receipt_id": "00000000-0000-0000-0000-000000000000",
            "receipt_mode": "live",
            "schema_version": "v5.0",
            "source": "SCHEDULE",
            "status": "OPEN",
        });
        let canonical = canonical_payload(&receipt_no_sig).unwrap();
        let signature = signing_key.sign(canonical.as_bytes());
        let mut full = receipt_no_sig;
        full.as_object_mut().unwrap().insert(
            "signature".to_string(),
            Value::String(hex::encode(signature.to_bytes())),
        );
        let body = serde_json::to_string(&full).unwrap();

        let server = MockServer::start_async().await;
        let status_mock = server
            .mock_async(move |when, then| {
                when.method(GET)
                    .path("/v5/status")
                    .header("X-Oracle-Key", "ho_live_test_key_42");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(body.clone());
            })
            .await;

        let result = verify_market_open_with_key(
            "XNYS",
            Some(&server.base_url()),
            &pk_hex,
            Some("ho_live_test_key_42"),
        )
        .await
        .expect("keyed request must verify the signed OPEN receipt");
        assert_eq!(result, HaltAttestation::Open);
        status_mock.assert_async().await;
    }
}
