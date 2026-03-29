# RWA CLI — Code Quality Improvements Design

**Date:** 2026-03-29
**Scope:** Series of 4 independent PRs improving type safety, trading reliability, agent JSON contracts, and test coverage.
**Approach:** Variant 1 — "Types first". Newtypes land before other PRs so the compiler guards subsequent changes.

---

## PR 1 — Newtypes `Symbol` + `Mint`

### Goal

Replace bare `String` arguments for token symbols and mint addresses with typed newtypes. The compiler catches transposed arguments and incorrect string reuse at the boundary between CLI and domain logic.

### New file: `crates/ondo/src/types.rs`

```rust
pub struct Symbol(String);  // e.g. "TSLA", "TSLAon"
pub struct Mint(String);    // base58 Solana address
```

Both types implement: `Display`, `Deref<Target=str>`, `AsRef<str>`, `From<String>`, `From<&str>`, `Clone`, `Debug`, `PartialEq`, `Eq`, `Hash`, `serde::{Serialize, Deserialize}`.

Re-exported from `crates/ondo/src/lib.rs` as `pub use types::{Symbol, Mint}`.

### Signature changes in `crates/ondo`

| Location | Before | After |
|---|---|---|
| `gm::resolve_token` | returns `(String, String)` | returns `(Symbol, Mint)` |
| `usecases::gm::prepare_buy/prepare_sell` | `symbol: &str` | `symbol: &Symbol` |
| `SwapPlan.symbol` | `String` | `Symbol` |
| `SwapPlan.swap.input_mint / output_mint` | `String` | `Mint` |
| `token_list::GmTokenEntry.solana_address` | `Option<&'static str>` | unchanged — static array can't hold owned `Mint`; convert at `resolve_gm_mint` boundary |
| `solana::get_balance` | `mint: &str` | `mint: &Mint` |
| `solana::get_usdc_balance_raw` | `pubkey: &str` | `pubkey: &str` (unchanged — pubkey stays `&str`) |

### Signature changes in `crates/cli`

CLI receives `String` from clap → wraps in `Symbol` before calling `ondo`. The newtype boundary is at the top of each command handler.

### What the compiler catches after PR 1

```rust
// Before: silent bug — args transposed
prepare_buy(wallet, mint_str, symbol_str, ...)

// After: compile error — type mismatch
prepare_buy(wallet, &mint, &symbol, ...)  // Mint ≠ Symbol
```

### Scope

~15–20 files. Changes are mechanical — the compiler guides the diff. No behaviour change.

---

## PR 2 — Trading Reliability

### Goal

Three targeted fixes in `usecases/gm.rs` and `solana/`. No behaviour changes to the happy path.

### Fix 1: SOL preflight check in `preflight_buy_raw`

**Problem:** If SOL balance < rent-exempt minimum, the transaction fails on-chain with a cryptic error.

**Fix:** In `preflight_buy_raw`, after checking USDC balance, fetch SOL balance and verify it is ≥ 0.002 SOL. If not, return early with a clear error:

```
Insufficient SOL for transaction fees: have 0.0001 SOL, need ~0.002 SOL.
Fund wallet: <pubkey>
```

Threshold constant: `MIN_SOL_FOR_FEES: f64 = 0.002`.

### Fix 2: `close-all` continues on missing market data

**Problem:** In `trade.rs`, `market_snapshot_for_symbol(...)? ` propagates with `?`. If one token has no market data, the entire `close-all` loop aborts.

**Fix:** Replace `?` with `match` — on error, push to `skipped` with `reason: "market data unavailable"` and `continue` the loop.

### Fix 3: Retry context in logs

**Problem:** `execute_with_retry` logs `"Transient error (attempt X/Y), retrying in 3s..."` without the error message.

**Fix:** Include the error: `"Transient error ({attempt}/{MAX}): {e}, retrying in 3s..."`.

### What is NOT changed

- Retry counts and delays (product decision)
- Default slippage (100 bps — product decision)
- No exponential backoff (3s is sufficient for Jupiter MM)

---

## PR 3 — JSON Contracts and Agent Errors

### Goal

When `--json` is active and a command fails, output structured JSON to stdout instead of plain text to stderr. Agents can reliably branch on `error_kind` without parsing strings.

### Error JSON schema

```json
{
  "status": "error",
  "error_kind": "<kind>",
  "error": "<human readable message>"
}
```

### `error_kind` mapping

| Typed error | `error_kind` |
|---|---|
| `GmTradeErrorKind::MarketClosed` | `"market_closed"` |
| `GmTradeErrorKind::NotTradable` | `"not_tradable"` |
| `GmTradeErrorKind::SlippageTooHigh` | `"slippage_too_high"` |
| `TransactionErrorKind::ConfirmationTimeout` | `"confirmation_timeout"` |
| `TransactionErrorKind::OnChainFailure` | `"on_chain_failure"` |
| `OndoErrorKind::Network` | `"network_error"` |
| all others | `"unknown_error"` |

### Implementation

In `crates/cli/src/lib.rs`, the top-level error handler tries `downcast_ref` for each known typed error in order. If `--json` flag is active, calls `json_out(&ErrorJson { ... })` and exits 1. Otherwise falls through to existing `eyre` stderr formatting.

### What is NOT changed

- Human-mode output (only `--json` path)
- Successful response schemas
- `portfolio` JSON schema (already stable)

---

## PR 4 — Tests

### Goal

Cover three untested paths. All tests are offline — no network, no real Solana.

### Test 1: Wallet encrypt/decrypt roundtrip (`crates/ondo/src/wallet.rs`)

```rust
#[test]
fn encrypt_decrypt_roundtrip() {
    let wallet = Wallet::generate();
    let passphrase = "test-passphrase";
    let tmp = tempfile::tempdir().unwrap();
    wallet.save_encrypted(tmp.path(), passphrase).unwrap();
    let loaded = Wallet::from_encrypted_file(tmp.path(), passphrase).unwrap();
    assert_eq!(wallet.pubkey(), loaded.pubkey());
}
```

Covers: age encryption, file write/read, `0o600` permissions.
Dependency: `tempfile` added to `[dev-dependencies]` if not already present.

### Test 2: Jupiter execute failure kind mapping (`crates/ondo/src/jupiter.rs`)

Using `httpmock`, test that HTTP/JSON responses from Jupiter map to the correct `ExecuteRetryAction`:

- `400` + body `"TRANSACTION_EXPIRED"` → `ExecuteRetryAction::RefreshOrder`
- `400` + body `"SIMULATION_ERROR"` → `ExecuteRetryAction::None`
- `503` → `ExecuteRetryAction::Retry`

### Test 3: `should_skip_position` edge cases (`crates/ondo/src/usecases/gm.rs`)

Pure function, 4 unit tests:

- `est_value = 1.50` → not skipped (at the threshold)
- `est_value = 1.49` → skipped, reason `"below $1.50 minimum"`
- `est_value = 0.00` → not skipped (zero balance handled separately)
- token not in tradable set → skipped, reason `"not tradable in current session"`

### What is NOT added

- End-to-end tests with real Solana
- File locking tests (OS-dependent, low CI value)
- Mock for `fetch_session_limits` (covered by `current_session()` logic)

---

## PR Order and Dependencies

```
PR 1 (newtypes) → PR 2 (reliability) → PR 3 (JSON errors) → PR 4 (tests)
```

PR 2, 3, 4 are independent of each other after PR 1 lands. They can be reviewed in parallel but should merge in order to avoid conflicts.

---

## Anti-Overengineering Checks

- No new crates, traits, or abstraction layers
- No config flags or modes
- No changes to public JSON schemas (except PR 3 adds `error` JSON — additive only)
- Each PR has a single, clearly stated purpose
- All changes are reversible without affecting other PRs
