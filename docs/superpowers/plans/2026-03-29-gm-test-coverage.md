# GM Test Coverage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add 15 pure unit tests for `parse_sell_pct`/`should_skip_position` and 7 httpmock integration tests for `jupiter::get_order`/`api::fetch_session_limits`, with minimal URL injection refactoring.

**Architecture:** Part A adds tests to existing pure functions (no code changes). Part B follows TDD: write failing tests first (compile error proves the interface isn't injectable yet), then make the minimum signature change to pass, then update all callers. `httpmock` and `serde_json` are already dev-dependencies in `crates/ondo/Cargo.toml` — nothing to add.

**Tech Stack:** Rust, `httpmock 0.7`, `serde_json::json!`, `tokio::test`, existing `eyre` error types.

---

## File Map

| File | Change |
|---|---|
| `crates/ondo/src/usecases/gm.rs` | Add 15 pure unit tests; add `api_url`/`jupiter_url` params to 3 private functions; update 5 call sites |
| `crates/ondo/src/api.rs` | `fetch_session_limits()` → `fetch_session_limits(base_url: Option<&str>)`; add 3 httpmock tests |
| `crates/ondo/src/jupiter.rs` | `get_order(...)` → `get_order(base_url: Option<&str>, ...)`; add 4 httpmock tests |
| `crates/cli/src/cmd/gm/list.rs` | Two one-line caller updates: `fetch_session_limits()` → `fetch_session_limits(None)` |
| `crates/cli/src/cmd/gm/trade.rs` | One one-line caller update: `fetch_tradable_set()` → `fetch_tradable_set(None)` |

---

## Task 1: `parse_sell_pct` unit tests

**Files:**
- Modify: `crates/ondo/src/usecases/gm.rs` (add to existing `#[cfg(test)] mod tests` at line 575)

- [ ] **Step 1: Add the 9 tests to the test module**

Open `crates/ondo/src/usecases/gm.rs`. Find the existing `#[cfg(test)] mod tests` block (currently ends at line 672 with `}`). Add these tests before the final closing `}` of that block:

```rust
    // ── parse_sell_pct ────────────────────────────────────────

    #[test]
    fn parse_sell_pct_none_returns_100() {
        assert_eq!(parse_sell_pct(None).unwrap(), 100.0);
    }

    #[test]
    fn parse_sell_pct_full_percent() {
        assert_eq!(parse_sell_pct(Some("100%")).unwrap(), 100.0);
    }

    #[test]
    fn parse_sell_pct_half_percent() {
        assert_eq!(parse_sell_pct(Some("50%")).unwrap(), 50.0);
    }

    #[test]
    fn parse_sell_pct_zero_percent() {
        assert_eq!(parse_sell_pct(Some("0%")).unwrap(), 0.0);
    }

    #[test]
    fn parse_sell_pct_decimal_percent() {
        assert_eq!(parse_sell_pct(Some("1.5%")).unwrap(), 1.5);
    }

    #[test]
    fn parse_sell_pct_over_100_is_err() {
        assert!(parse_sell_pct(Some("101%")).is_err());
    }

    #[test]
    fn parse_sell_pct_negative_is_err() {
        assert!(parse_sell_pct(Some("-1%")).is_err());
    }

    #[test]
    fn parse_sell_pct_missing_percent_suffix_is_err() {
        assert!(parse_sell_pct(Some("50")).is_err());
    }

    #[test]
    fn parse_sell_pct_non_numeric_is_err() {
        assert!(parse_sell_pct(Some("abc%")).is_err());
    }
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p rwa-ondo "parse_sell_pct" 2>&1 | tail -10
```

Expected: all 9 pass. `parse_sell_pct` already exists and is correct; these tests should be green immediately.

- [ ] **Step 3: Commit**

```bash
git add crates/ondo/src/usecases/gm.rs
git commit -m "test: unit tests for parse_sell_pct"
```

---

## Task 2: `should_skip_position` unit tests

**Files:**
- Modify: `crates/ondo/src/usecases/gm.rs` (append to same test module)

The tradable set in `should_skip_position` is compared against `symbol.to_uppercase()` — so the set must hold uppercase values (e.g., `"TSLAON"`, not `"TSLAon"`). `MIN_SELL_VALUE_USD` is `1.5`.

- [ ] **Step 1: Add the 6 tests**

Append before the final `}` of `#[cfg(test)] mod tests` in `crates/ondo/src/usecases/gm.rs`:

```rust
    // ── should_skip_position ──────────────────────────────────

    #[test]
    fn should_skip_zero_value_never_blocks() {
        // est_value == 0.0 does NOT trigger the minimum check (condition is `> 0.0 && < MIN`)
        let tradable: std::collections::HashSet<String> = ["TSLAON".to_string()].into();
        assert!(should_skip_position("TSLAon", 0.0, &tradable).is_none());
    }

    #[test]
    fn should_skip_below_minimum_returns_skip() {
        let tradable: std::collections::HashSet<String> = ["TSLAON".to_string()].into();
        let skip = should_skip_position("TSLAon", 1.2, &tradable).unwrap();
        assert!(skip.reason.contains("below"));
    }

    #[test]
    fn should_skip_at_minimum_boundary_does_not_skip() {
        // 1.5 is exactly MIN_SELL_VALUE_USD — the condition is `< MIN`, not `<=`
        let tradable: std::collections::HashSet<String> = ["TSLAON".to_string()].into();
        assert!(should_skip_position("TSLAon", MIN_SELL_VALUE_USD, &tradable).is_none());
    }

    #[test]
    fn should_skip_empty_tradable_set_does_not_skip() {
        // Empty set = market closed, tradability check is bypassed entirely
        let tradable: std::collections::HashSet<String> = std::collections::HashSet::new();
        assert!(should_skip_position("TSLAon", 2.0, &tradable).is_none());
    }

    #[test]
    fn should_skip_symbol_not_in_tradable_set() {
        let tradable: std::collections::HashSet<String> = ["AAPLON".to_string()].into();
        let skip = should_skip_position("TSLAon", 2.0, &tradable).unwrap();
        assert!(skip.reason.contains("not tradable"));
    }

    #[test]
    fn should_skip_symbol_in_tradable_set_does_not_skip() {
        let tradable: std::collections::HashSet<String> = ["TSLAON".to_string()].into();
        assert!(should_skip_position("TSLAon", 2.0, &tradable).is_none());
    }
```

- [ ] **Step 2: Run the tests**

```bash
cargo test -p rwa-ondo "should_skip" 2>&1 | tail -10
```

Expected: all 6 pass.

- [ ] **Step 3: Commit**

```bash
git add crates/ondo/src/usecases/gm.rs
git commit -m "test: unit tests for should_skip_position"
```

---

## Task 3: `api::fetch_session_limits` URL injection + httpmock tests

**Files:**
- Modify: `crates/ondo/src/api.rs`
- Modify: `crates/ondo/src/usecases/gm.rs`
- Modify: `crates/cli/src/cmd/gm/list.rs`

**Context:** `fetch_session_limits` is called from 4 places: `list.rs:57`, `list.rs:119`, `usecases/gm.rs:347` (in `fetch_tradable_set`), `usecases/gm.rs:465` (in `check_tradable`). The function currently takes no parameters and uses `ONDO_SESSION_URL` directly. The test will call it with `Some(&mock_url)`.

- [ ] **Step 1: Write the 3 failing httpmock tests**

Find the `#[cfg(test)] mod tests` block in `crates/ondo/src/api.rs` (starts at line 524). Add these 3 tests before the final `}` of that block. They won't compile yet — that's expected.

```rust
    // ── fetch_session_limits httpmock ─────────────────────────

    #[tokio::test]
    async fn fetch_session_limits_parses_valid_response() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        server.mock_async(|when, then| {
            when.method(GET).path("/api/limits/session");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({
                    "limits": [
                        {"symbol": "TSLAon", "regular": {"tradable": true}},
                        {"symbol": "AAPLon", "regular": {"tradable": false}}
                    ]
                }));
        }).await;

        let url = format!("{}/api/limits/session", server.base_url());
        let limits = fetch_session_limits(Some(&url)).await.unwrap();

        assert_eq!(limits.len(), 2);
        assert_eq!(limits[0].symbol, "TSLAon");
        assert!(limits[0].is_tradable(Session::Regular));
        assert!(!limits[1].is_tradable(Session::Regular));
    }

    #[tokio::test]
    async fn fetch_session_limits_returns_err_on_http_500() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        server.mock_async(|when, then| {
            when.method(GET).path("/api/limits/session");
            then.status(500).body("Internal Server Error");
        }).await;

        let url = format!("{}/api/limits/session", server.base_url());
        assert!(fetch_session_limits(Some(&url)).await.is_err());
    }

    #[tokio::test]
    async fn fetch_session_limits_returns_err_on_malformed_json() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        server.mock_async(|when, then| {
            when.method(GET).path("/api/limits/session");
            then.status(200)
                .header("content-type", "application/json")
                .body("not json at all");
        }).await;

        let url = format!("{}/api/limits/session", server.base_url());
        assert!(fetch_session_limits(Some(&url)).await.is_err());
    }
```

- [ ] **Step 2: Run to confirm compile error**

```bash
cargo test -p rwa-ondo "fetch_session_limits" 2>&1 | head -15
```

Expected: compile error — `fetch_session_limits` takes 0 arguments but 1 was supplied (or similar).

- [ ] **Step 3: Change `fetch_session_limits` signature in `api.rs`**

In `crates/ondo/src/api.rs`, replace:

```rust
pub async fn fetch_session_limits() -> Result<Vec<SessionLimits>> {
    let resp = HTTP.get(ONDO_SESSION_URL).send().await.map_err(|e| {
```

with:

```rust
pub async fn fetch_session_limits(base_url: Option<&str>) -> Result<Vec<SessionLimits>> {
    let url = base_url.unwrap_or(ONDO_SESSION_URL);
    let resp = HTTP.get(url).send().await.map_err(|e| {
```

- [ ] **Step 4: Update `fetch_tradable_set` and `check_tradable` in `usecases/gm.rs`**

**4a.** Replace the `fetch_tradable_set` function signature and its internal call:

```rust
// Before:
pub async fn fetch_tradable_set() -> std::collections::HashSet<String> {
    let session = api::current_session();
    if session == api::Session::Closed {
        return std::collections::HashSet::new();
    }
    api::fetch_session_limits()
        .await

// After:
pub async fn fetch_tradable_set(api_url: Option<&str>) -> std::collections::HashSet<String> {
    let session = api::current_session();
    if session == api::Session::Closed {
        return std::collections::HashSet::new();
    }
    api::fetch_session_limits(api_url)
        .await
```

**4b.** Replace the `check_tradable` function signature and its internal call:

```rust
// Before:
async fn check_tradable(symbol: &str) -> Result<()> {
    let session = api::current_session();
    if session == api::Session::Closed {
        return Ok(());
    }
    let limits = match api::fetch_session_limits().await {

// After:
async fn check_tradable(symbol: &str, api_url: Option<&str>) -> Result<()> {
    let session = api::current_session();
    if session == api::Session::Closed {
        return Ok(());
    }
    let limits = match api::fetch_session_limits(api_url).await {
```

**4c.** Update the two callers of `check_tradable` inside `prepare_buy` (line ~126) and `prepare_sell` (line ~175):

```rust
// prepare_buy — before:
    check_tradable(&symbol),
// prepare_buy — after:
    check_tradable(&symbol, None),

// prepare_sell — before:
    check_tradable(&symbol),
// prepare_sell — after:
    check_tradable(&symbol, None),
```

- [ ] **Step 5: Update the two callers in `list.rs`**

In `crates/cli/src/cmd/gm/list.rs`, there are two calls. Find them with:

```bash
grep -n "fetch_session_limits" crates/cli/src/cmd/gm/list.rs
```

Replace each `api::fetch_session_limits().await` with `api::fetch_session_limits(None).await`.

- [ ] **Step 6: Update the caller in `trade.rs`**

In `crates/cli/src/cmd/gm/trade.rs` line ~182:

```rust
// Before:
        usecases::gm::fetch_tradable_set()
// After:
        usecases::gm::fetch_tradable_set(None)
```

- [ ] **Step 7: Compile check**

```bash
cargo check --workspace 2>&1 | grep "^error" | head -10
```

Expected: zero errors.

- [ ] **Step 8: Run the httpmock tests**

```bash
cargo test -p rwa-ondo "fetch_session_limits" 2>&1 | tail -10
```

Expected: all 3 pass.

- [ ] **Step 9: Run full ondo test suite**

```bash
cargo test -p rwa-ondo 2>&1 | tail -5
```

Expected: all pass.

- [ ] **Step 10: Commit**

```bash
git add crates/ondo/src/api.rs \
        crates/ondo/src/usecases/gm.rs \
        crates/cli/src/cmd/gm/list.rs \
        crates/cli/src/cmd/gm/trade.rs
git commit -m "test: inject api_url into fetch_session_limits, add httpmock tests"
```

---

## Task 4: `jupiter::get_order` URL injection + httpmock tests

**Files:**
- Modify: `crates/ondo/src/jupiter.rs`
- Modify: `crates/ondo/src/usecases/gm.rs`

**Context:** `jupiter::get_order` is called from 3 places in `usecases/gm.rs`: line 406 and line 426 (both inside `get_order_checked`), and line 558 (inside `execute_with_retry`). `get_order_checked` itself is called from `prepare_buy` (line ~131), `prepare_sell` (line ~225), and `execute_sell_raw` (line ~278).

- [ ] **Step 1: Write the 4 failing httpmock tests**

Find the `#[cfg(test)] mod tests` block in `crates/ondo/src/jupiter.rs` (currently starts at line 330). Add these 4 tests before the final `}` of that block. They won't compile yet.

```rust
    // ── get_order httpmock ────────────────────────────────────

    #[tokio::test]
    async fn get_order_returns_parsed_response_on_success() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        server.mock_async(|when, then| {
            when.method(GET).path("/order");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({
                    "requestId": "test-req-1",
                    "inAmount": "1000000",
                    "outAmount": "500000000",
                    "transaction": "AQAAAA==",
                    "gasless": true,
                    "router": "jupiterz"
                }));
        }).await;

        let order = get_order(
            Some(&server.base_url()),
            USDC_MINT,
            "So11111111111111111111111111111112",
            "1000000",
            "FakeWallet111111111111111111111111111111",
            Some(100),
        ).await.unwrap();

        assert_eq!(order.in_amount, "1000000");
        assert_eq!(order.out_amount, "500000000");
        assert_eq!(order.transaction, Some("AQAAAA==".to_string()));
        assert_eq!(order.gasless, Some(true));
        assert_eq!(order.router, Some("jupiterz".to_string()));
    }

    #[tokio::test]
    async fn get_order_returns_err_when_response_has_error_field() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        server.mock_async(|when, then| {
            when.method(GET).path("/order");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({
                    "requestId": "test-req-2",
                    "inAmount": "1000000",
                    "outAmount": "0",
                    "error": "QUOTE_ERROR",
                    "errorMessage": "no route found"
                }));
        }).await;

        let result = get_order(
            Some(&server.base_url()),
            USDC_MINT,
            "So11111111111111111111111111111112",
            "1000000",
            "FakeWallet111111111111111111111111111111",
            None,
        ).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no route found"));
    }

    #[tokio::test]
    async fn get_order_returns_err_on_empty_transaction() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        server.mock_async(|when, then| {
            when.method(GET).path("/order");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(serde_json::json!({
                    "requestId": "test-req-3",
                    "inAmount": "1000000",
                    "outAmount": "500000000",
                    "transaction": ""
                }));
        }).await;

        let result = get_order(
            Some(&server.base_url()),
            USDC_MINT,
            "So11111111111111111111111111111112",
            "1000000",
            "FakeWallet111111111111111111111111111111",
            None,
        ).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty transaction"));
    }

    #[tokio::test]
    async fn get_order_returns_err_on_http_400_failed_quotes() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        server.mock_async(|when, then| {
            when.method(GET).path("/order");
            then.status(400)
                .body("Failed to get quotes for the given input");
        }).await;

        let result = get_order(
            Some(&server.base_url()),
            USDC_MINT,
            "So11111111111111111111111111111112",
            "1000000",
            "FakeWallet111111111111111111111111111111",
            None,
        ).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No swap route found"));
    }
```

- [ ] **Step 2: Run to confirm compile error**

```bash
cargo test -p rwa-ondo "get_order_returns" 2>&1 | head -15
```

Expected: compile error — `get_order` takes 5 arguments but 6 were supplied (or similar).

- [ ] **Step 3: Change `get_order` signature in `jupiter.rs`**

In `crates/ondo/src/jupiter.rs`, replace:

```rust
pub async fn get_order(
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
) -> Result<OrderResponse> {
    let mut last_err = eyre!("Jupiter /order failed");

    for attempt in 0..=ORDER_MAX_RETRIES {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }

        let mut request = HTTP.get(format!("{SWAP_API_BASE}/order")).query(&[
```

with:

```rust
pub async fn get_order(
    base_url: Option<&str>,
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
) -> Result<OrderResponse> {
    let mut last_err = eyre!("Jupiter /order failed");
    let order_url = format!("{}/order", base_url.unwrap_or(SWAP_API_BASE));

    for attempt in 0..=ORDER_MAX_RETRIES {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }

        let mut request = HTTP.get(&order_url).query(&[
```

- [ ] **Step 4: Update `get_order_checked` in `usecases/gm.rs`**

**4a.** Replace the `get_order_checked` signature:

```rust
// Before:
async fn get_order_checked(
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
    json: bool,
) -> Result<(jupiter::OrderResponse, Option<f64>)> {

// After:
async fn get_order_checked(
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
    json: bool,
    jupiter_url: Option<&str>,
) -> Result<(jupiter::OrderResponse, Option<f64>)> {
```

**4b.** Inside `get_order_checked`, update the two `jupiter::get_order` calls (line ~406 and ~426):

```rust
// Before (line ~406):
    let mut order = jupiter::get_order(input_mint, output_mint, amount, taker, slippage_bps).await?;

// After:
    let mut order = jupiter::get_order(jupiter_url, input_mint, output_mint, amount, taker, slippage_bps).await?;
```

```rust
// Before (line ~426):
                order = jupiter::get_order(input_mint, output_mint, amount, taker, slippage_bps).await?;

// After:
                order = jupiter::get_order(jupiter_url, input_mint, output_mint, amount, taker, slippage_bps).await?;
```

**4c.** Inside `execute_with_retry` (line ~558), the order-refresh call passes `None` (not testable with httpmock since it's a retry path):

```rust
// Before:
                    current_order_owned = Some(
                        jupiter::get_order(
                            params.input_mint,
                            params.output_mint,
                            params.raw_amount,
                            params.taker,
                            params.slippage_bps,
                        )
                        .await?,
                    );

// After:
                    current_order_owned = Some(
                        jupiter::get_order(
                            None,
                            params.input_mint,
                            params.output_mint,
                            params.raw_amount,
                            params.taker,
                            params.slippage_bps,
                        )
                        .await?,
                    );
```

**4d.** Update the 3 callers of `get_order_checked` — add `None` as the last argument:

`prepare_buy` (line ~131):
```rust
// Before:
    let (order, slippage_pct) = get_order_checked(
        jupiter::USDC_MINT,
        &gm_mint,
        &raw_usdc,
        &taker,
        slippage_bps,
        json,
    )
    .await?;

// After:
    let (order, slippage_pct) = get_order_checked(
        jupiter::USDC_MINT,
        &gm_mint,
        &raw_usdc,
        &taker,
        slippage_bps,
        json,
        None,
    )
    .await?;
```

`prepare_sell` (line ~225):
```rust
// Before:
    let (order, slippage_pct) = get_order_checked(
        &gm_mint,
        jupiter::USDC_MINT,
        &raw_gm,
        &taker,
        slippage_bps,
        json,
    )
    .await?;

// After:
    let (order, slippage_pct) = get_order_checked(
        &gm_mint,
        jupiter::USDC_MINT,
        &raw_gm,
        &taker,
        slippage_bps,
        json,
        None,
    )
    .await?;
```

`execute_sell_raw` (line ~278):
```rust
// Before:
    let (order, _) = get_order_checked(
        mint,
        jupiter::USDC_MINT,
        raw_amount,
        taker,
        Some(DEFAULT_SLIPPAGE_BPS),
        json,
    )
    .await?;

// After:
    let (order, _) = get_order_checked(
        mint,
        jupiter::USDC_MINT,
        raw_amount,
        taker,
        Some(DEFAULT_SLIPPAGE_BPS),
        json,
        None,
    )
    .await?;
```

- [ ] **Step 5: Compile check**

```bash
cargo check --workspace 2>&1 | grep "^error" | head -10
```

Expected: zero errors.

- [ ] **Step 6: Run the httpmock tests**

```bash
cargo test -p rwa-ondo "get_order_returns" 2>&1 | tail -10
```

Expected: all 4 pass.

- [ ] **Step 7: Commit**

```bash
git add crates/ondo/src/jupiter.rs \
        crates/ondo/src/usecases/gm.rs
git commit -m "test: inject base_url into get_order, add httpmock tests"
```

---

## Task 5: Full test suite and clippy

**Files:** none

- [ ] **Step 1: Run full workspace tests**

```bash
cargo test --workspace 2>&1 | grep -E "^test result|FAILED"
```

Expected: all pass, 0 failures.

- [ ] **Step 2: Run clippy**

```bash
cargo clippy --all-targets 2>&1 | grep "^error" | head -10
```

Expected: zero errors.
