# RWA CLI — GM Test Coverage Design

**Date:** 2026-03-29
**Scope:** Add missing tests for `usecases/gm.rs` (pure unit tests) and httpmock integration tests for `jupiter.rs` / `api.rs`. Zero behaviour change.

---

## Problem

`usecases/gm.rs` (673 lines) has only 8 tests — all covering slippage math and error display. Two pure functions with zero tests sit directly in the `close-all` trade path:

- `parse_sell_pct` — parses `None | "50%" | "all"` from the CLI
- `should_skip_position` — decides whether to skip a position during `close-all`

The modules that make external HTTP calls (`jupiter.rs`, `api.rs`) have no httpmock tests. A mis-parsed Jupiter response or a broken session-limits JSON would go undetected until production.

---

## Approach

**Part A — Pure unit tests** in `usecases/gm.rs`. No refactoring needed; these are already pure functions.

**Part B — httpmock integration tests** in `jupiter.rs` and `api.rs`. The tests live with the code that owns the HTTP calls. To make URLs injectable for tests without changing public API, add `base_url: Option<&str>` to two functions following the existing `rpc_url: Option<&str>` pattern already used throughout the codebase.

---

## File Map

| File | Change |
|---|---|
| `crates/ondo/src/usecases/gm.rs` | Add 15 pure unit tests; add `api_url: Option<&str>` to `check_tradable` and `fetch_tradable_set` (internal propagation only) |
| `crates/ondo/src/jupiter.rs` | Add `base_url: Option<&str>` to `get_order`; add 4 httpmock tests |
| `crates/ondo/src/api.rs` | Add `base_url: Option<&str>` to `fetch_session_limits`; add 3 httpmock tests |
| `crates/cli/src/cmd/gm/trade.rs` | One-line: `fetch_tradable_set()` → `fetch_tradable_set(None)` |
| `crates/ondo/Cargo.toml` | Add `httpmock = "0.7"` as dev-dependency |

---

## Part A — Pure Unit Tests

### `parse_sell_pct` — 9 cases

All tests are synchronous, no setup needed.

| Input | Expected result |
|---|---|
| `None` | `Ok(100.0)` — missing amount means sell all |
| `Some("100%")` | `Ok(100.0)` |
| `Some("50%")` | `Ok(50.0)` |
| `Some("0%")` | `Ok(0.0)` |
| `Some("1.5%")` | `Ok(1.5)` — decimal percentage is valid |
| `Some("101%")` | `Err` — above 100 |
| `Some("-1%")` | `Err` — below 0 |
| `Some("50")` | `Err` — missing `%` suffix |
| `Some("abc%")` | `Err` — not a number |

### `should_skip_position` — 6 cases

`MIN_SELL_VALUE_USD = 1.5` is the relevant constant.

| `est_value` | `tradable_set` | Expected |
|---|---|---|
| `0.0` | `{"TSLAon"}` | `None` — zero value does not trigger minimum check |
| `1.2` | `{"TSLAon"}` | `Some` with reason `"below $1.50 minimum"` |
| `1.5` | `{"TSLAon"}` | `None` — at boundary, not below |
| `2.0` | `{}` (empty) | `None` — empty set = closed market, tradability check is skipped |
| `2.0` | `{"AAPLon"}` | `Some` with reason `"not tradable in current session"` |
| `2.0` | `{"TSLAon"}` | `None` — above minimum, present in tradable set |

---

## Part B — URL Injection

### Signature changes

**`jupiter.rs`**

```rust
// Before:
pub async fn get_order(
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
) -> Result<OrderResponse>

// After:
pub async fn get_order(
    base_url: Option<&str>,   // None → uses SWAP_API_BASE
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    taker: &str,
    slippage_bps: Option<u32>,
) -> Result<OrderResponse>
```

Inside `get_order`, replace the one URL format string:
```rust
// Before:
HTTP.get(format!("{SWAP_API_BASE}/order"))
// After:
HTTP.get(format!("{}/order", base_url.unwrap_or(SWAP_API_BASE)))
```

(`execute_order` also uses `SWAP_API_BASE` but is not changed — its URL stays hardcoded.)

**`api.rs`**

```rust
// Before:
pub async fn fetch_session_limits() -> Result<Vec<SessionLimits>>

// After:
pub async fn fetch_session_limits(base_url: Option<&str>) -> Result<Vec<SessionLimits>>
```

Inside, replace the URL:
```rust
// Before:
HTTP.get(ONDO_SESSION_URL)
// After:
HTTP.get(base_url.unwrap_or(ONDO_SESSION_URL))
```

**`usecases/gm.rs`** — propagate `api_url` through two private functions:

```rust
// check_tradable: add api_url param, pass to fetch_session_limits
async fn check_tradable(symbol: &str, api_url: Option<&str>) -> Result<()>
// Inside: api::fetch_session_limits(api_url).await

// fetch_tradable_set: add api_url param, pass to fetch_session_limits
pub async fn fetch_tradable_set(api_url: Option<&str>) -> std::collections::HashSet<String>
// Inside: api::fetch_session_limits(api_url).await
```

All callers of `check_tradable` and `fetch_tradable_set` within `prepare_buy`, `prepare_sell`, and `close_all` pass `None`.

`get_order_checked` (private, in `usecases/gm.rs`) calls `jupiter::get_order`. Its signature gains `jupiter_url: Option<&str>` which it passes through to `get_order`. Production callers (`prepare_buy`, `prepare_sell`) pass `None`.

**`trade.rs`** — one line change:
```rust
// Before:
usecases::gm::fetch_tradable_set()
// After:
usecases::gm::fetch_tradable_set(None)
```

### What does NOT change

- `prepare_buy`, `prepare_sell`, `execute_swap` public signatures — unchanged
- `execute_order` in `jupiter.rs` — not touched (testing execute requires real wallet signing)
- All existing tests — all pass unchanged

---

## Part B — httpmock Test Cases

### `jupiter.rs` — 4 tests

**Dev-dependency setup:** add `httpmock = "0.7"` to `[dev-dependencies]` in `crates/ondo/Cargo.toml`.

All tests are `#[tokio::test]` and live in the existing `#[cfg(test)] mod tests` block in `jupiter.rs`.

---

**Test 1: `get_order_returns_parsed_response_on_success`**

Mock: `GET /order` → HTTP 200, body:
```json
{
  "requestId": "req-abc",
  "inAmount": "1000000",
  "outAmount": "500000000",
  "inUsdValue": 1.0,
  "outUsdValue": 0.995,
  "transaction": "AQAAAA==",
  "gasless": true,
  "router": "jupiterz"
}
```

Assert:
- `result.is_ok()`
- `order.in_amount == "1000000"`
- `order.out_amount == "500000000"`
- `order.transaction == Some("AQAAAA==".to_string())`
- `order.gasless == Some(true)`

---

**Test 2: `get_order_returns_err_when_response_has_error_field`**

Mock: `GET /order` → HTTP 200, body:
```json
{
  "requestId": "req-xyz",
  "inAmount": "1000000",
  "outAmount": "0",
  "error": "QUOTE_ERROR",
  "errorMessage": "no route found"
}
```

Assert: `result.is_err()` and error message contains `"no route found"`.

---

**Test 3: `get_order_returns_err_on_empty_transaction`**

Mock: `GET /order` → HTTP 200, body:
```json
{
  "requestId": "req-empty",
  "inAmount": "1000000",
  "outAmount": "500000000",
  "transaction": ""
}
```

Assert: `result.is_err()` and error message contains `"empty transaction"`.

---

**Test 4: `get_order_returns_err_on_http_400_with_failed_quotes`**

Mock: `GET /order` → HTTP 400, body: `"Failed to get quotes for the given input"`.

Assert: `result.is_err()` and error message contains `"No swap route found"`.

---

### `api.rs` — 3 tests

All tests are `#[tokio::test]` in the existing `#[cfg(test)] mod tests` block in `api.rs`.

---

**Test 1: `fetch_session_limits_parses_valid_response`**

The test constructs the full mock URL as `format!("{}/api/limits/session", server.base_url())` and passes it as `base_url`. All three `api.rs` tests use this same URL construction.

Mock: `GET /api/limits/session` → HTTP 200, body:
```json
{
  "limits": [
    {
      "symbol": "TSLAon",
      "regular": { "tradable": true, "maxAttestationCount": null, "maxActiveNotionalValue": null }
    },
    {
      "symbol": "AAPLon",
      "regular": { "tradable": false, "maxAttestationCount": null, "maxActiveNotionalValue": null }
    }
  ]
}
```

Assert:
- `result.is_ok()`
- `limits.len() == 2`
- `limits[0].symbol == "TSLAon"`
- `limits[0].is_tradable(Session::Regular) == true`
- `limits[1].is_tradable(Session::Regular) == false`

---

**Test 2: `fetch_session_limits_returns_err_on_http_500`**

Mock: `GET /api/limits/session` → HTTP 500, body: `"Internal Server Error"`.

Assert: `result.is_err()`.

---

**Test 3: `fetch_session_limits_returns_err_on_malformed_json`**

Mock: `GET /api/limits/session` → HTTP 200, body: `"not json at all"`.

Assert: `result.is_err()`.

---

## Scope

**No behaviour change.** All production call sites pass `None` for the new URL parameters; behaviour is identical to before.

**No new abstractions.** The `Option<&str>` pattern already exists throughout the codebase (`rpc_url`).

**Not in scope:**
- `execute_order` httpmock tests (requires real transaction signing)
- `prepare_buy` / `prepare_sell` end-to-end tests (multi-mock orchestration — separate spec)
- Property-based tests (separate spec if needed)
