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
pub fn ser_opt_f64_2<S: serde::Serializer>(
    v: &Option<f64>,
    s: S,
) -> Result<S::Ok, S::Error> {
    match v {
        Some(val) => s.serialize_some(&((val * 100.0).round() / 100.0)),
        None => s.serialize_none(),
    }
}

// ── Command option bundles ─────────────────────────────────

/// Execution-mode flags shared by every money-moving command: consent (`-y`),
/// preview (`--dry-run`), and output mode (`--json`). One struct instead of
/// three adjacent positional bools, so a transposed argument can no longer
/// compile silently on a money path.
#[derive(Clone, Copy)]
pub struct ExecOpts {
    pub yes: bool,
    pub dry_run: bool,
    pub json: bool,
}

/// Quote tuning shared by trade/basket/close-all: slippage tolerance and the
/// all-in cost ceiling. Bundled for the same transposition-safety reason as
/// [`ExecOpts`] (two adjacent `Option<u32>`s are as swappable as two bools).
#[derive(Clone, Copy)]
pub struct TradeTuning {
    pub slippage: Option<u32>,
    pub max_bps: Option<u32>,
}

// ── JSON output types ──────────────────────────────────────

#[derive(Serialize)]
pub struct HoursJson {
    pub status: &'static str,
    pub session: &'static str,
    pub session_hours: &'static str,
    pub now: String,
    pub countdown: String,
    /// Unix epoch seconds when the next session starts (= when the current one
    /// ends). Machine-readable companion to the English `countdown`/`now`
    /// prose, so agents schedule against a number instead of parsing text.
    pub next_session_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tradable_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tradable: Option<Vec<String>>,
    /// Assets currently dividend-paused (`is_trading_paused`). Absent when
    /// asset metadata couldn't be fetched (fail-open).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused_count: Option<usize>,
    /// Flagship assets that trade 24/7 via Ondo's offhours session
    /// (`is_offhours_tradable`). Absent when asset metadata couldn't be
    /// fetched (fail-open).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offhours_tradable_count: Option<usize>,
    /// The flagship symbols themselves; only populated with `--tradable`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offhours_tradable: Option<Vec<String>>,
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

#[derive(Serialize, Clone)]
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
    /// Ondo tags. Present on every portfolio response (not just `--view`) so
    /// scripts never need a join against `gm search`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sector: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Every tag label across all categories — what `--view` matches against.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Share of the enclosing group's value. Set only inside `groups[]`.
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_f64_2")]
    pub group_alloc_pct: Option<f64>,
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
    /// Present only when `--view` was given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<ViewJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub groups: Option<Vec<GroupJson>>,
}

#[derive(Serialize)]
pub struct ViewFiltersJson {
    pub tags: Vec<String>,
    pub tokens: Vec<String>,
}

#[derive(Serialize)]
pub struct ViewJson {
    /// Terms exactly as typed — the echo that keeps a polymorphic flag honest.
    pub terms: Vec<String>,
    pub filters: ViewFiltersJson,
    /// Our category word (`sector`/`region`/`class`/`type`/`factor`), not the
    /// Ondo slug. Null when the view only filters.
    pub split_by: Option<String>,
    /// True when a position can appear in more than one group — groups then do
    /// NOT sum to `value_usd`. Read this before summing.
    pub overlapping: bool,
    pub matched_positions: usize,
    #[serde(serialize_with = "ser_f64_2")]
    pub matched_value_usd: f64,
    #[serde(serialize_with = "ser_f64_2")]
    pub matched_pct_of_gm: f64,
}

#[derive(Serialize)]
pub struct GroupJson {
    /// Null when the view has no split (a single anonymous group).
    pub group: Option<String>,
    #[serde(serialize_with = "ser_f64_2")]
    pub value_usd: f64,
    #[serde(serialize_with = "ser_f64_2")]
    pub gm_alloc_pct: f64,
    #[serde(serialize_with = "ser_f64_2")]
    pub change_24h_pct: f64,
    pub positions: Vec<PositionJson>,
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
    /// Trade ledger tamper-evidence: "ok" | "legacy" | "broken@line N".
    /// Always present — chain problems never fail `pnl`, they're surfaced
    /// here (and warned to stderr in human output) so callers can decide.
    pub ledger_integrity: String,
    /// Absolute path of the wallet's ledger file — the single source of the
    /// numbers above (CLI trades only). Delete the file to reset history.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger_path: Option<String>,
}

/// One wallet's row in the `pnl --all` comparison.
#[derive(Serialize)]
pub struct PnlWalletSummaryJson {
    pub wallet: String,
    pub trades_recorded: usize,
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
    pub ledger_integrity: String,
}

/// `pnl --all`: every wallet that ever traded through the CLI (one row per
/// ledger file, no key material required), sorted by total P&L descending.
#[derive(Serialize)]
pub struct PnlAllJson {
    pub wallets: Vec<PnlWalletSummaryJson>,
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
    /// True when a repeat run would sell this position (pause/session/API),
    /// false for dust. Drives `status` and the exit code.
    pub retryable: bool,
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

/// Weighted-allocation echo for `buy-basket --total`.
#[derive(Serialize)]
pub struct AllocationJson {
    /// `--total` as entered (USDC).
    pub total: String,
    /// Symbol → weight percent as entered, without the `%` sign.
    pub weights: std::collections::BTreeMap<String, String>,
}

#[derive(Serialize)]
pub struct BuyBasketResultJson {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gas_refuel: Option<GasRefuelJson>,
    /// Present only for `--total` runs: the requested weighted allocation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allocation: Option<AllocationJson>,
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

#[cfg(test)]
mod tests {
    use super::*;

    // Wrapper newtypes exercise each serializer exactly as the real JSON
    // structs do (via `#[serde(serialize_with = "...")]`), pinning the
    // rounded token text rather than re-deriving the formula in-test.
    #[derive(Serialize)]
    struct Wrap2(#[serde(serialize_with = "ser_f64_2")] f64);
    #[derive(Serialize)]
    struct Wrap4(#[serde(serialize_with = "ser_f64_4")] f64);
    #[derive(Serialize)]
    struct WrapOpt4(#[serde(serialize_with = "ser_opt_f64_4")] Option<f64>);

    #[test]
    fn ser_f64_2_rounds_half_away_from_zero_to_cents() {
        // 19.99666 * 100 = 1999.666 -> round -> 2000 -> / 100 = 20.0.
        // A mutant that drops the rounding (serializes v verbatim) would
        // print "19.99666" instead of "20.0".
        assert_eq!(serde_json::to_string(&Wrap2(19.996_66)).unwrap(), "20.0");
        // 19.994 * 100 = 1999.4 -> round -> 1999 -> / 100 = 19.99 (rounds down).
        assert_eq!(serde_json::to_string(&Wrap2(19.994)).unwrap(), "19.99");
        // Exact halfway case: 0.125 * 100 = 12.5 -> round-half-away-from-zero
        // -> 13 -> / 100 = 0.13. A mutant using `10000.0` here (the 4-decimal
        // constant) would instead print "0.125" unrounded.
        assert_eq!(serde_json::to_string(&Wrap2(0.125)).unwrap(), "0.13");
        // Sign must survive: -1.005 * 100 = -100.49999999999999 (f64) ->
        // round -> -100 -> / 100 = -1.0. A mutant dropping the sign or using
        // `.floor()`/`.ceil()` instead of `.round()` would print -1.01 or -0.99.
        assert_eq!(serde_json::to_string(&Wrap2(-1.005)).unwrap(), "-1.0");
    }

    #[test]
    fn ser_f64_4_rounds_to_four_decimals() {
        // 12.345678 * 10000 = 123456.78 -> round -> 123457 -> / 10000 = 12.3457.
        // A mutant swapping in the 2-decimal constant (100.0) would instead
        // print "12.35".
        assert_eq!(serde_json::to_string(&Wrap4(12.345_678)).unwrap(), "12.3457");
        // 0.00005 * 10000 = 0.5 -> round -> 1 -> / 10000 = 0.0001 (rounds up
        // from the halfway point, not truncated to 0.0).
        assert_eq!(serde_json::to_string(&Wrap4(0.000_05)).unwrap(), "0.0001");
    }

    #[test]
    fn hours_json_omits_new_optional_fields_when_none() {
        let json = serde_json::to_value(HoursJson {
            status: "open",
            session: "Regular",
            session_hours: "9:30 AM - 3:59 PM ET",
            now: "Monday 10:00 AM ET".into(),
            countdown: "next session in 5h 59m".into(),
            next_session_at: 1_752_264_000,
            tradable_count: Some(440),
            tradable: None,
            paused_count: None,
            offhours_tradable_count: None,
            offhours_tradable: None,
        })
        .unwrap();
        assert!(json.get("paused_count").is_none());
        assert!(json.get("offhours_tradable_count").is_none());
        assert!(json.get("offhours_tradable").is_none());
        assert_eq!(json.get("tradable_count"), Some(&serde_json::json!(440)));
        // Always-present machine-readable timestamp (agents schedule on this).
        assert_eq!(json.get("next_session_at"), Some(&serde_json::json!(1_752_264_000)));
    }

    #[test]
    fn hours_json_includes_new_optional_fields_when_some() {
        let json = serde_json::to_value(HoursJson {
            status: "closed",
            session: "Closed",
            session_hours: "Weekend / NYSE holidays",
            now: "Saturday 10:00 AM ET".into(),
            countdown: "opens in 22h 0m".into(),
            next_session_at: 1_752_436_800,
            tradable_count: Some(6),
            tradable: Some(vec!["TSLAon".to_string()]),
            paused_count: Some(121),
            offhours_tradable_count: Some(6),
            offhours_tradable: Some(vec!["TSLAon".to_string(), "NVDAon".to_string()]),
        })
        .unwrap();
        assert_eq!(json.get("paused_count"), Some(&serde_json::json!(121)));
        assert_eq!(json.get("offhours_tradable_count"), Some(&serde_json::json!(6)));
        assert_eq!(
            json.get("offhours_tradable"),
            Some(&serde_json::json!(["TSLAon", "NVDAon"]))
        );
    }

    #[test]
    fn ser_opt_f64_4_matches_ser_f64_4_and_propagates_none() {
        // Same rounding as ser_f64_4 (100/3 = 33.3333... -> 33.3333) — a
        // mutant that reimplements this independently (e.g. copy-pasted with
        // the 2-decimal constant) would desync the two and this would catch it.
        assert_eq!(
            serde_json::to_string(&WrapOpt4(Some(100.0 / 3.0))).unwrap(),
            "33.3333"
        );
        // None must serialize as JSON null, not as 0.0 or be skipped (skipping
        // is the caller's `skip_serializing_if`, not this fn's job).
        assert_eq!(serde_json::to_string(&WrapOpt4(None)).unwrap(), "null");
    }
}
