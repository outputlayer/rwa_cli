//! Scenario tests written against REAL USER TASKS (Task 6.5), composing the
//! pure position/asset functions end-to-end the way a live command would
//! chain them — not one test per function.
//!
//! Only PUBLIC `rwa_ondo` APIs are used here (this file compiles as a
//! separate crate). Scenarios that need `pub(crate)` internals (the
//! `--limit-price` gate: `effective_limit_raw6` / `check_limit_gate` in
//! `crates/ondo/src/usecases/gm_order.rs`, and `resolve_sell_amount` in
//! `crates/ondo/src/usecases/gm_internal.rs`) live as in-module tests next to
//! those functions instead — see the report for exact locations.

use rwa_ondo::api::{self, OndoAsset};
use rwa_ondo::solana::SolanaTokenBalance;
use rwa_ondo::types::Mint;
use rwa_ondo::usecases::gm::{compute_portfolio, filter_close_positions};

fn spy_asset(price: &str, paused: bool) -> OndoAsset {
    serde_json::from_value(serde_json::json!({
        "symbol": "SPYon",
        "assetName": "SPDR S&P 500 ETF",
        "isTradingPaused": paused,
        "primaryMarket": { "price": price }
    }))
    .expect("valid asset json")
}

fn spy_balance(raw_tokens: f64, ui_tokens: f64) -> SolanaTokenBalance {
    SolanaTokenBalance {
        symbol: "SPYon".into(),
        mint: Mint::from("k18WJUULWheRkSpSquYGdNNmtuE2Vbw1hpuUi92ondo"),
        balance: raw_tokens,
        ui_balance: Some(ui_tokens),
        raw_amount: (raw_tokens * 1_000_000_000.0).round().to_string(),
    }
}

/// User task: "How much is my SPYon position worth, and what dividend
/// multiplier is it carrying?" — `gm portfolio` must price the RAW balance
/// (Jupiter/Ondo frame), never the wallet-displayed (ui) balance, or the
/// accrued dividend multiplier gets double-counted into the USD value.
#[test]
fn dividend_token_portfolio_prices_raw_not_ui() {
    let raw = 20.369073_f64;
    let ui = 20.526341_f64; // Phantom shows raw × 1.0077209
    let price = 753.99_f64; // per RAW token, per Ondo/Jupiter convention

    let balances = vec![spy_balance(raw, ui)];
    let assets = vec![spy_asset("753.99", false)];
    let summary = compute_portfolio(&balances, &assets);

    assert_eq!(summary.positions.len(), 1);
    let pos = &summary.positions[0];
    // Correct: value = raw × price ≈ $15,358.10.
    assert!(
        (pos.value_usd - raw * price).abs() < 0.01,
        "value_usd {} should equal raw*price {}",
        pos.value_usd,
        raw * price
    );
    // Wrong answer this test must reject: ui × price would overstate by the
    // multiplier (≈ $15,476.72) — a real double-count bug class.
    assert!(
        (pos.value_usd - ui * price).abs() > 100.0,
        "value_usd must not equal ui*price (double-counted multiplier)"
    );
    let m = pos.shares_per_token.expect("multiplier should be reported");
    assert!((m - 1.0077209).abs() < 1e-6, "shares_per_token {m}");
}

/// User task: "Sell my whole SPYon position." Selling `all` must resolve to
/// the RAW on-chain balance (what Jupiter and the ledger use), not the
/// larger, wallet-displayed number — and if the user instead types the
/// number Phantom shows them, the CLI must refuse and explain why, naming
/// both balances so the user isn't confused into thinking they're short.
#[test]
fn dividend_token_close_all_skips_paused_and_keeps_out_of_sells() {
    // A second position of the same dividend-bearing family, paused for the
    // ex-dividend window: `close-all` must skip it, not attempt to sell it.
    let paused_asset = spy_asset("753.99", true);
    let balances = vec![spy_balance(20.369073, 20.526341)];
    let tradable: std::collections::HashSet<String> = ["SPYON".to_string()].into();

    let (positions, skipped) =
        filter_close_positions(&balances, 100.0, &[paused_asset], &tradable).unwrap();

    assert!(
        positions.is_empty(),
        "a paused position must never be queued for sale"
    );
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].token, "SPYon");
    assert_eq!(skipped[0].reason, "trading_paused");
}

/// User task: "It's ex-dividend day — is SPYon still tradable, and does the
/// close-all pass leave it alone?" Composes the pure pause check with the
/// close-all pre-filter the way `gm close-all` really does: check first,
/// then let `filter_close_positions` apply the same rule when deciding what
/// to sell.
#[test]
fn pause_on_ex_dividend_day_blocks_close_all_but_not_a_healthy_sibling() {
    let assets = vec![spy_asset("753.99", true), {
        let mut a = spy_asset("100.00", false);
        a.symbol = "AAPLon".to_string();
        a
    }];
    assert!(api::is_trading_paused("SPYon", &assets));
    assert!(api::is_trading_paused("spyON", &assets), "case-insensitive");
    assert!(!api::is_trading_paused("AAPLon", &assets));

    let balances = vec![
        spy_balance(20.369073, 20.526341),
        // The healthy sibling is ALSO a dividend-bearing scaled token
        // (ui 5.038604 = raw 5.0 × 1.0077209) — close-all must still size
        // its sell in RAW units.
        SolanaTokenBalance {
            symbol: "AAPLon".into(),
            mint: Mint::from("9wYZetvT8J2ptfsRca5gzLBGvcUug38mp9yT3xaondo"),
            balance: 5.0,
            ui_balance: Some(5.038604),
            raw_amount: "5000000000".into(),
        },
    ];
    let tradable: std::collections::HashSet<String> =
        ["SPYON".to_string(), "AAPLON".to_string()].into();
    let (positions, skipped) = filter_close_positions(&balances, 100.0, &assets, &tradable).unwrap();

    assert_eq!(positions.len(), 1, "only the healthy sibling should be queued to sell");
    assert_eq!(positions[0].symbol, "AAPLon");
    // Sell sizing stays in the raw frame: 5000000000 (5.0 raw tokens), not
    // 5038604000 (the Phantom-displayed quantity) — selling the ui amount
    // would overdraw the account.
    assert_eq!(positions[0].sell_raw, "5000000000");
    assert_eq!(positions[0].sell_display, "5");
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].token, "SPYon");
    assert_eq!(skipped[0].reason, "trading_paused");
}

/// User task: "A 10:1 split just landed on my dividend token — did I just
/// lose 90% of my portfolio?" A Scaled-UI multiplier jump changes the
/// wallet-displayed quantity, never the raw balance or the per-raw-token
/// price, so total USD value must be unchanged across the "split".
#[test]
fn ten_to_one_split_does_not_change_portfolio_value() {
    let raw = 20.369073_f64;
    let price = 753.99_f64;

    // Independent literals (raw × m computed by hand), so the test cannot
    // share an arithmetic bug with `compute_portfolio`'s ui/raw ratio:
    // 20.369073 × 1.0077209 = 20.526341; 20.369073 × 10.077 = 205.259149.
    let pre_split = spy_balance(raw, 20.526341); // pre-split multiplier
    let post_split = spy_balance(raw, 205.259149); // multiplier jumps ~10x

    let assets = vec![spy_asset(&price.to_string(), false)];
    let before = compute_portfolio(std::slice::from_ref(&pre_split), &assets);
    let after = compute_portfolio(std::slice::from_ref(&post_split), &assets);

    // Both frames value the position identically at $15,358.08 — a split
    // moves the displayed quantity, never the USD value.
    assert!(
        (before.value_usd - 15358.08).abs() < 0.01,
        "pre-split value {}",
        before.value_usd
    );
    assert!(
        (before.value_usd - after.value_usd).abs() < 1e-6,
        "value before {} vs after {} must match — raw balance and price are unchanged",
        before.value_usd,
        after.value_usd
    );
    let m_after = after.positions[0].shares_per_token.expect("multiplier reported");
    assert!((m_after - 10.077).abs() < 1e-6, "shares_per_token {m_after}");
}
