//! Trade orchestration: `gm` is the public facade; the `gm_*` siblings split
//! the pipeline into order fetch and pre-trade gates (`gm_order`), live
//! execute-and-retry (`gm_execute`), shared preflight internals (`gm_internal`),
//! `--limit-price` framing (`gm_limit`), auto gas refuel (`gm_gas`), and
//! portfolio / P&L views (`gm_positions`, `gm_pnl`).

pub mod gm;
pub(crate) mod gm_execute;
pub(crate) mod gm_internal;
pub(crate) mod gm_limit;
pub(crate) mod gm_order;
pub(crate) mod gm_gas;
pub(crate) mod gm_pnl;
pub(crate) mod gm_positions;
