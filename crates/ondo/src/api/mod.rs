mod assets;
mod error;
mod history;
mod session;

pub use assets::*;
pub use error::{OndoError, OndoErrorKind};
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
