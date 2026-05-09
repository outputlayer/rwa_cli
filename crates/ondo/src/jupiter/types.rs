//! Jupiter API request/response types and the structured failure taxonomy.
//!
//! Public types that callers of `crate::jupiter` rely on. The `OrderBackend`
//! enum tags which Jupiter API a given quote came from so the matching
//! `/execute` flavor can be picked. `ExecuteFailureKind` maps Jupiter's
//! numeric error codes to retry semantics.

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrderBackend {
    #[default]
    SwapV2Lite,
    Ultra,
    UltraLite,
    MetisV1Lite,
}

impl OrderBackend {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::SwapV2Lite => "swap-v2-lite",
            Self::Ultra => "ultra",
            Self::UltraLite => "ultra-lite",
            Self::MetisV1Lite => "metis-v1-lite",
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderResponse {
    pub request_id: String,
    pub in_amount: String,
    pub out_amount: String,
    pub in_usd_value: Option<f64>,
    pub out_usd_value: Option<f64>,
    /// Price impact as decimal (e.g. -0.001 = -0.1%).
    pub price_impact: Option<f64>,
    /// Price impact as string for precise display (deprecated in V2, use price_impact).
    pub price_impact_pct: Option<String>,
    /// Slippage in basis points.
    pub slippage_bps: Option<u32>,
    /// Jupiter fee in basis points.
    pub fee_bps: Option<u32>,
    pub transaction: Option<String>,
    pub error: Option<String>,
    pub error_message: Option<String>,
    /// Whether this swap is gasless (Jupiter/MM pays gas).
    pub gasless: Option<bool>,
    /// Which router won the quote: iris, jupiterz, dflow, okx.
    pub router: Option<String>,
    /// Rent fee in lamports for ATA creation (if needed).
    pub rent_fee_lamports: Option<u64>,
    /// Who pays the rent: "jupiter", "user", or null.
    pub rent_fee_payer: Option<String>,
    /// Signature fee in lamports.
    pub signature_fee_lamports: Option<u64>,
    /// Who pays signature fee.
    pub signature_fee_payer: Option<String>,
    /// Priority fee in lamports (includes Jito tips).
    pub prioritization_fee_lamports: Option<u64>,
    /// Who pays priority fee.
    pub prioritization_fee_payer: Option<String>,
    /// "ultra" or "manual".
    pub mode: Option<String>,
    /// Last valid block height for the transaction.
    pub last_valid_block_height: Option<String>,
    #[serde(skip)]
    pub backend: OrderBackend,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecuteResponse {
    pub status: Option<String>,
    pub signature: Option<String>,
    pub code: Option<i32>,
    pub error: Option<String>,
    pub error_message: Option<String>,
    pub input_amount_result: Option<String>,
    pub output_amount_result: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecuteRetryAction {
    None,
    RetrySameOrder,
    RefreshOrder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecuteFailureKind {
    MissingCachedOrder,
    InvalidSignedTransaction,
    InvalidMessageBytes,
    FailedToLand,
    UnknownAggregatorError,
    RfqFailedToLand,
    UnknownRfqError,
    InvalidPayload,
    QuoteExpired,
    SwapRejected,
    InternalError,
    Unknown,
}

impl ExecuteFailureKind {
    #[must_use]
    pub fn from_code(code: Option<i32>) -> Self {
        match code {
            Some(-1) => Self::MissingCachedOrder,
            Some(-2) => Self::InvalidSignedTransaction,
            Some(-3) => Self::InvalidMessageBytes,
            Some(-1000) => Self::FailedToLand,
            Some(-1001) => Self::UnknownAggregatorError,
            Some(-2000) => Self::RfqFailedToLand,
            Some(-2001) => Self::UnknownRfqError,
            Some(-2002) => Self::InvalidPayload,
            Some(-2003) => Self::QuoteExpired,
            Some(-2004) => Self::SwapRejected,
            Some(-2005) => Self::InternalError,
            _ => Self::Unknown,
        }
    }

    #[must_use]
    pub fn retry_action(self) -> ExecuteRetryAction {
        match self {
            Self::MissingCachedOrder | Self::QuoteExpired | Self::SwapRejected | Self::InternalError => ExecuteRetryAction::RefreshOrder,
            Self::FailedToLand | Self::RfqFailedToLand => ExecuteRetryAction::RetrySameOrder,
            Self::InvalidSignedTransaction
            | Self::InvalidMessageBytes
            | Self::UnknownAggregatorError
            | Self::UnknownRfqError
            | Self::InvalidPayload
            | Self::Unknown => ExecuteRetryAction::None,
        }
    }

    #[must_use]
    pub fn hint(self) -> &'static str {
        match self {
            Self::MissingCachedOrder => "missing cached order — retry",
            Self::InvalidSignedTransaction => "invalid signed transaction",
            Self::InvalidMessageBytes => "invalid message bytes",
            Self::FailedToLand => "failed to land — retry",
            Self::UnknownAggregatorError => "unknown aggregator error",
            Self::RfqFailedToLand => "RFQ failed to land — retry",
            Self::UnknownRfqError => "unknown RFQ error",
            Self::InvalidPayload => "invalid payload",
            Self::QuoteExpired => "quote expired — retry",
            Self::SwapRejected => "swap rejected",
            Self::InternalError => "internal error — retry",
            Self::Unknown => "",
        }
    }
}

#[derive(Debug)]
pub struct ExecuteFailure {
    pub kind: ExecuteFailureKind,
    pub code: Option<i32>,
    pub message: String,
}

impl std::fmt::Display for ExecuteFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let code = self
            .code
            .map(|c| format!(" (code {c})"))
            .unwrap_or_default();
        let hint = self.kind.hint();
        if hint.is_empty() {
            write!(f, "Swap failed{code}: {}", self.message)
        } else {
            write!(f, "Swap failed{code}: {} ({hint})", self.message)
        }
    }
}

impl std::error::Error for ExecuteFailure {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_failure_kind_maps_codes_to_retry_actions() {
        assert_eq!(
            ExecuteFailureKind::from_code(Some(-2003)).retry_action(),
            ExecuteRetryAction::RefreshOrder
        );
        assert_eq!(
            ExecuteFailureKind::from_code(Some(-1000)).retry_action(),
            ExecuteRetryAction::RetrySameOrder
        );
        assert_eq!(
            ExecuteFailureKind::from_code(Some(-2)).retry_action(),
            ExecuteRetryAction::None
        );
    }

    #[test]
    fn execute_failure_display_includes_hint() {
        let failure = ExecuteFailure {
            kind: ExecuteFailureKind::FailedToLand,
            code: Some(-1000),
            message: "landing failure".to_string(),
        };
        let msg = failure.to_string();
        assert!(msg.contains("code -1000"));
        assert!(msg.contains("failed to land"));
    }
}
