//! Structured Solana RPC error type.
//!
//! Captures enough context — error kind, method, URL, HTTP status, RPC code,
//! detail — to make failures actionable. `is_retryable()` classifies which
//! kinds the orchestrator should keep trying on.

use reqwest::StatusCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SolanaRpcErrorKind {
    Network,
    RateLimited,
    HttpStatus,
    Decode,
    RpcResponse,
    EmptyResult,
    BatchShape,
    Unavailable,
}

#[derive(Debug)]
pub(crate) struct SolanaRpcError {
    pub kind: SolanaRpcErrorKind,
    pub method: Option<String>,
    pub url: Option<String>,
    pub status: Option<StatusCode>,
    pub code: Option<i64>,
    pub detail: String,
}

impl SolanaRpcError {
    pub(in crate::solana) fn new(
        kind: SolanaRpcErrorKind,
        method: Option<&str>,
        url: Option<&str>,
        status: Option<StatusCode>,
        code: Option<i64>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            method: method.map(str::to_string),
            url: url.map(str::to_string),
            status,
            code,
            detail: detail.into(),
        }
    }

    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.kind,
            SolanaRpcErrorKind::Network
                | SolanaRpcErrorKind::RateLimited
                | SolanaRpcErrorKind::Decode
                | SolanaRpcErrorKind::EmptyResult
                | SolanaRpcErrorKind::BatchShape
                | SolanaRpcErrorKind::Unavailable
        )
    }
}

impl SolanaRpcError {
    /// Whether the failure condemns the endpoint (transient), not the request:
    /// retryable kinds, a server-side 5xx, and JSON-RPC error codes that are
    /// node-side and transient (behind/unavailable/internal/rate-limit:
    /// -32005, -32004, -32007, -32603, -32429). Client errors (4xx such as
    /// 401/403) and request-level RPC errors (-32601 method not found, -32602
    /// invalid params, …) are request problems and are not transient.
    /// Shared by the URL-rotation loop and `classify_error` (`rpc_unavailable`).
    #[must_use]
    pub fn is_endpoint_transient(&self) -> bool {
        self.is_retryable()
            || (self.kind == SolanaRpcErrorKind::HttpStatus
                && self.status.is_some_and(|s| s.is_server_error()))
            || (self.kind == SolanaRpcErrorKind::RpcResponse
                && self
                    .code
                    .is_some_and(|c| matches!(c, -32005 | -32004 | -32007 | -32603 | -32429)))
    }
}

impl std::fmt::Display for SolanaRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let method = self.method.as_deref().unwrap_or("batch");
        let url = self.url.as_deref().unwrap_or("<unknown>");
        match (self.status, self.code) {
            (Some(status), Some(code)) => write!(
                f,
                "Solana RPC error [{}] method={} url={} status={} code={}: {}",
                self.kind, method, url, status, code, self.detail
            ),
            (Some(status), None) => write!(
                f,
                "Solana RPC error [{}] method={} url={} status={}: {}",
                self.kind, method, url, status, self.detail
            ),
            (None, Some(code)) => write!(
                f,
                "Solana RPC error [{}] method={} url={} code={}: {}",
                self.kind, method, url, code, self.detail
            ),
            (None, None) => write!(
                f,
                "Solana RPC error [{}] method={} url={}: {}",
                self.kind, method, url, self.detail
            ),
        }
    }
}

impl std::fmt::Display for SolanaRpcErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::Network => "network",
            Self::RateLimited => "rate_limited",
            Self::HttpStatus => "http_status",
            Self::Decode => "decode",
            Self::RpcResponse => "rpc_response",
            Self::EmptyResult => "empty_result",
            Self::BatchShape => "batch_shape",
            Self::Unavailable => "unavailable",
        };
        f.write_str(label)
    }
}

impl std::error::Error for SolanaRpcError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_rpc_error_only_transient_is_rpc_unavailable() {
        use crate::usecases::gm::classify_error as c;
        use StatusCode as S;
        use SolanaRpcErrorKind as K;
        let mk = |k, st: Option<S>| -> eyre::Error { SolanaRpcError::new(k, None, None, st, None, "x").into() };
        assert_eq!(c(&mk(K::HttpStatus, Some(S::UNAUTHORIZED))), None);
        assert_eq!(c(&mk(K::HttpStatus, Some(S::FORBIDDEN))), None);
        assert_eq!(c(&mk(K::HttpStatus, Some(S::SERVICE_UNAVAILABLE))), Some("rpc_unavailable"));
        assert_eq!(c(&mk(K::Network, None)), Some("rpc_unavailable"));
        assert_eq!(c(&mk(K::RpcResponse, None)), None);
    }

    #[test]
    fn rpc_response_codes_split_transient_from_request_errors() {
        use crate::usecases::gm::classify_error as c;
        let mk = |code: i64| -> SolanaRpcError {
            SolanaRpcError::new(SolanaRpcErrorKind::RpcResponse, Some("getBalance"), None, None, Some(code), "x")
        };
        for code in [-32005, -32004, -32007, -32603, -32429] {
            assert!(mk(code).is_endpoint_transient(), "{code}");
            assert_eq!(c(&mk(code).into()), Some("rpc_unavailable"), "{code}");
        }
        for code in [-32601, -32602, -32600, -32002] {
            assert!(!mk(code).is_endpoint_transient(), "{code}");
            assert_eq!(c(&mk(code).into()), None, "{code}");
        }
    }

    #[test]
    fn solana_rpc_error_rate_limit_is_retryable() {
        let err = SolanaRpcError::new(
            SolanaRpcErrorKind::RateLimited,
            Some("getBalance"),
            Some("https://rpc.example"),
            Some(StatusCode::TOO_MANY_REQUESTS),
            None,
            "HTTP 429 from RPC endpoint",
        );
        assert!(err.is_retryable());
        assert!(err.to_string().contains("rate_limited"));
    }

    #[test]
    fn solana_rpc_error_client_http_is_not_retryable() {
        let err = SolanaRpcError::new(
            SolanaRpcErrorKind::HttpStatus,
            Some("getBalance"),
            Some("https://rpc.example"),
            Some(StatusCode::UNAUTHORIZED),
            None,
            "client-side RPC failure",
        );
        assert!(!err.is_retryable());
    }
}
