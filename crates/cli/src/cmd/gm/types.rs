use serde::Serialize;

// ── Rounded float serializers for token-efficient JSON ─────

/// Round to 2 decimal places (prices, USD values).
pub fn ser_f64_2<S: serde::Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64((v * 100.0).round() / 100.0)
}
/// Round to 4 decimal places (percentages, slippage).
pub fn ser_f64_4<S: serde::Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64((v * 10000.0).round() / 10000.0)
}
pub fn ser_opt_f64_4<S: serde::Serializer>(
    v: &Option<f64>,
    s: S,
) -> Result<S::Ok, S::Error> {
    match v {
        Some(val) => s.serialize_some(&((val * 10000.0).round() / 10000.0)),
        None => s.serialize_none(),
    }
}

// ── JSON output types ──────────────────────────────────────

#[derive(Serialize)]
pub struct HoursJson {
    pub status: &'static str,
    pub session: &'static str,
    pub session_hours: &'static str,
    pub now: String,
    pub countdown: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tradable_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tradable: Option<Vec<String>>,
}

#[derive(Serialize)]
pub struct TradeJson {
    pub status: &'static str,
    pub amount: String,
    pub token: String,
    pub counter_amount: String,
    pub counter_token: &'static str,
    pub tx: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gas_refuel: Option<GasRefuelJson>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_f64_4"
    )]
    pub slippage_pct: Option<f64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_f64_4"
    )]
    pub actual_slippage_pct: Option<f64>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_f64_4"
    )]
    pub price_impact_pct: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fee_bps: Option<u32>,
    /// Absent when Jupiter's quote didn't report it (typical for AMM routes
    /// via swap/v2) — absence means "unreported", not "false".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gasless: Option<bool>,
    /// Winning router/market-maker label; absent when Jupiter didn't report
    /// one. The Metis fallback path always fills both fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub router: Option<String>,
    /// Echo of `--limit-price` as entered; absent when the flag was not used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_price: Option<String>,
    /// Reference price per underlying SHARE (= quote price / shares_per_token).
    /// Informational; absent when the multiplier is unknown or 1.
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_f64_4")]
    pub share_price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_f64_4")]
    pub shares_per_token: Option<f64>,
}

#[derive(Serialize)]
pub struct PositionJson {
    pub token: String,
    #[serde(serialize_with = "ser_f64_4")]
    pub balance: f64,
    #[serde(serialize_with = "ser_f64_2")]
    pub price: f64,
    #[serde(serialize_with = "ser_f64_2")]
    pub value_usd: f64,
    #[serde(serialize_with = "ser_f64_2")]
    pub gm_alloc_pct: f64,
    #[serde(serialize_with = "ser_f64_2")]
    pub change_pct_24h: f64,
    /// Scaled-UI multiplier: underlying shares per raw token (dividend
    /// reinvestment accrual). Absent when 1 or unknown.
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_f64_4")]
    pub shares_per_token: Option<f64>,
}

#[derive(Serialize)]
pub struct PortfolioCashJson {
    #[serde(serialize_with = "ser_f64_4")]
    pub sol: f64,
    #[serde(serialize_with = "ser_f64_2")]
    pub usdc: f64,
}

#[derive(Serialize)]
pub struct PortfolioGmPositionsJson {
    pub positions: Vec<PositionJson>,
    #[serde(serialize_with = "ser_f64_2")]
    pub value_usd: f64,
    #[serde(serialize_with = "ser_f64_2")]
    pub change_24h_usd: f64,
    #[serde(serialize_with = "ser_f64_2")]
    pub change_24h_pct: f64,
}

#[derive(Serialize)]
pub struct PortfolioUnavailableJson {
    pub symbol: String,
    pub reason: String,
}

#[derive(Serialize)]
pub struct PortfolioJson {
    pub wallet: String,
    pub cash: PortfolioCashJson,
    pub gm_positions: PortfolioGmPositionsJson,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unavailable: Vec<PortfolioUnavailableJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<&'static str>,
}

#[derive(Serialize)]
pub struct HistoryJson {
    pub symbol: String,
    pub range: String,
    pub candles: usize,
    pub first: HistoryCandleJson,
    pub last: HistoryCandleJson,
    #[serde(serialize_with = "ser_f64_2")]
    pub high: f64,
    #[serde(serialize_with = "ser_f64_2")]
    pub low: f64,
    #[serde(serialize_with = "ser_f64_2")]
    pub change_pct: f64,
}

#[derive(Serialize)]
pub struct HistoryCandleJson {
    pub timestamp: u64,
    pub price: f64,
}

#[derive(Serialize)]
pub struct ListItemJson {
    pub symbol: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sector: Option<String>,
    /// Classification for tokens without a sector (mostly ETFs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    pub tradable: bool,
    /// Ondo has paused trading for this asset (dividend window). Only
    /// serialized when true.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub trading_paused: bool,
    /// Lowercased haystack of every Ondo tag label (sector, asset class,
    /// region, factor/risk) — used by `--tag`/`--search`, not serialized.
    #[serde(skip)]
    pub all_tags: String,
}

#[derive(Serialize)]
pub struct TradableResultJson {
    pub session: &'static str,
    pub count: usize,
    pub items: Vec<TradableItemJson>,
}

#[derive(Serialize)]
pub struct TradableItemJson {
    pub input: String,
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sector: Option<String>,
    pub tradable: bool,
    /// Ondo has paused trading for this asset (dividend window). Only
    /// serialized when true.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub trading_paused: bool,
}

#[derive(Serialize)]
pub struct SendJson {
    pub status: &'static str,
    pub token: String,
    pub amount: String,
    pub recipient: String,
    pub tx: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gas_refuel: Option<GasRefuelJson>,
}

/// Automatic SOL gas refuel executed before the main operation.
#[derive(Serialize)]
pub struct GasRefuelJson {
    pub usdc: String,
    pub sol: String,
    pub tx: String,
}

#[derive(Serialize)]
pub struct PnlTokenJson {
    pub token: String,
    /// Open position per the CLI trade ledger.
    pub qty: String,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_f64_4")]
    pub avg_cost: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_f64_4")]
    pub market_price: Option<f64>,
    #[serde(serialize_with = "ser_f64_2")]
    pub invested_usdc: f64,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_f64_4")]
    pub market_value_usdc: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_f64_4")]
    pub unrealized_usdc: Option<f64>,
    #[serde(serialize_with = "ser_f64_2")]
    pub realized_usdc: f64,
    /// Sold beyond what the ledger saw bought (acquired outside the CLI);
    /// excluded from realized P&L.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oversold_qty: Option<String>,
}

#[derive(Serialize)]
pub struct PnlTotalsJson {
    #[serde(serialize_with = "ser_f64_2")]
    pub invested_usdc: f64,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_f64_4")]
    pub market_value_usdc: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_f64_4")]
    pub unrealized_usdc: Option<f64>,
    #[serde(serialize_with = "ser_f64_2")]
    pub realized_usdc: f64,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_f64_4")]
    pub total_pnl_usdc: Option<f64>,
}

/// P&L built exclusively from the CLI's own buy/sell ledger.
#[derive(Serialize)]
pub struct PnlJson {
    pub wallet: String,
    pub trades_recorded: usize,
    pub tokens: Vec<PnlTokenJson>,
    pub totals: PnlTotalsJson,
}

#[derive(Serialize)]
pub struct CloseAllResultJson {
    pub status: &'static str,
    pub sold: Vec<CloseItemJson>,
    pub failed: Vec<CloseFailJson>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<CloseSkipJson>,
    pub total_usdc: String,
}

#[derive(Serialize)]
pub struct CloseSkipJson {
    pub token: String,
    #[serde(serialize_with = "ser_f64_2")]
    pub estimated_usd: f64,
    pub reason: &'static str,
}

#[derive(Serialize)]
pub struct CloseItemJson {
    pub token: String,
    pub amount: String,
    pub usdc: String,
    pub tx: String,
}

#[derive(Serialize)]
pub struct CloseFailJson {
    pub token: String,
    pub error: String,
    /// Stable kind label for the error (e.g. "slippage_too_high", "swap_rejected").
    /// Present when the underlying error is a known structured type; absent for
    /// opaque errors. Agents/scripts can branch on this to decide retry vs abort.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<&'static str>,
}

#[derive(Serialize)]
pub struct ReclaimJson {
    pub status: &'static str,
    pub accounts_closed: usize,
    pub sol_reclaimed: String,
    pub signatures: Vec<String>,
}

#[derive(Serialize)]
pub struct BuyBasketResultJson {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gas_refuel: Option<GasRefuelJson>,
    pub bought: Vec<BuyBasketItemJson>,
    pub failed: Vec<CloseFailJson>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<CloseSkipJson>,
    pub total_usdc_spent: String,
}

#[derive(Serialize)]
pub struct BuyBasketItemJson {
    pub token: String,
    pub received: String,
    pub usdc: String,
    pub tx: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_f64_4"
    )]
    pub slippage_pct: Option<f64>,
}

#[derive(Serialize)]
pub struct SellBasketResultJson {
    pub status: &'static str,
    pub sold: Vec<SellBasketItemJson>,
    pub failed: Vec<CloseFailJson>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<CloseSkipJson>,
    pub total_usdc_received: String,
}

#[derive(Serialize)]
pub struct SellBasketItemJson {
    pub token: String,
    pub amount: String,
    pub usdc: String,
    pub tx: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_f64_4"
    )]
    pub slippage_pct: Option<f64>,
}
