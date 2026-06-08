mod assets;
mod error;
mod halt_attestation;
mod history;
mod session;

pub use assets::*;
pub use error::{OndoError, OndoErrorKind};
pub use halt_attestation::*;
pub use history::*;
pub use session::*;

use eyre::Result;

// ─── Shared helpers used by submodules ──────────────────────────────────────

pub(crate) fn invalid_market_data(field: &str, detail: impl Into<String>) -> eyre::Report {
    OndoError::new(OndoErrorKind::InvalidData, field, None, detail).into()
}

pub(crate) fn missing_market_data(field: &str, detail: impl Into<String>) -> eyre::Report {
    OndoError::new(OndoErrorKind::MissingData, field, None, detail).into()
}

pub(crate) fn parse_market_number(field: &str, s: &str, allow_negative: bool) -> Result<f64> {
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

/// Retry a fallible Ondo HTTP call with exponential backoff.
/// Retries only on errors that downcast to `OndoError` and pass `is_retryable()`
/// (network errors, 5xx, 429). Non-retryable errors (4xx, decode, opaque)
/// return immediately. Backoff: 500ms, 1s, 2s.
pub(crate) async fn retry_with_backoff<T, F, Fut>(max_attempts: u32, op: F) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut last_err: Option<eyre::Report> = None;
    for attempt in 0..max_attempts {
        if attempt > 0 {
            let backoff = std::time::Duration::from_millis(500u64 << (attempt - 1));
            tokio::time::sleep(backoff).await;
        }
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                let retryable = e
                    .downcast_ref::<OndoError>()
                    .map(OndoError::is_retryable)
                    .unwrap_or(false);
                if !retryable {
                    return Err(e);
                }
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| eyre::eyre!("retry attempts exhausted")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn retry_eventually_succeeds_after_transient_failure() {
        let calls = Arc::new(AtomicU32::new(0));
        let calls_in = calls.clone();
        let result: Result<u32> = retry_with_backoff(3, || {
            let calls = calls_in.clone();
            async move {
                let n = calls.fetch_add(1, Ordering::SeqCst) + 1;
                if n < 2 {
                    Err(OndoError::new(
                        OndoErrorKind::Network,
                        "test",
                        None,
                        "transient",
                    )
                    .into())
                } else {
                    Ok(42u32)
                }
            }
        })
        .await;
        assert_eq!(result.unwrap(), 42);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "should retry once");
    }

    #[tokio::test]
    async fn retry_does_not_retry_non_retryable_error() {
        let calls = Arc::new(AtomicU32::new(0));
        let calls_in = calls.clone();
        let result: Result<u32> = retry_with_backoff(3, || {
            let calls = calls_in.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(OndoError::new(
                    OndoErrorKind::InvalidData,
                    "test",
                    None,
                    "bad data",
                )
                .into())
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "should not retry");
    }

    #[tokio::test]
    async fn retry_exhausts_after_max_attempts() {
        let calls = Arc::new(AtomicU32::new(0));
        let calls_in = calls.clone();
        let result: Result<u32> = retry_with_backoff(3, || {
            let calls = calls_in.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(OndoError::new(
                    OndoErrorKind::Network,
                    "test",
                    None,
                    "transient",
                )
                .into())
            }
        })
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 3, "should attempt 3 times");
    }
}
