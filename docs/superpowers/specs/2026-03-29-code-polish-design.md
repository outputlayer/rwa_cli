# RWA CLI — Code Polish Design

**Date:** 2026-03-29
**Scope:** Two structural cleanups with zero behaviour change. No new features, no logic changes.

---

## Change 1 — `SolanaTokenBalance.mint: Mint`

### Problem

`SolanaTokenBalance.mint` is a plain `String` (declared in `crates/ondo/src/solana/mod.rs`). Every other mint-address boundary in the codebase now uses `Mint`. The `close_all` path passes `&tb.mint` to `execute_sell_raw` as `&str`, bypassing the newtype guarantee introduced in PR 1.

### Fix

Change the field type from `String` to `Mint`.

```rust
pub struct SolanaTokenBalance {
    pub symbol: String,
    pub mint: Mint,   // was: String
    pub balance: f64,
    pub raw_amount: String,
}
```

### Affected call sites (compiler-guided)

| Location | Change |
|---|---|
| `parse_gm_balances` — field assignment | `mint: info.mint.clone()` → `mint: Mint::from(&info.mint)` |
| `get_balance` — field assignment | `mint: mint.to_string()` → `mint: mint.clone()` |
| `close_all` in `trade.rs` — `&tb.mint` | unchanged — `&Mint` deref-coerces to `&str` automatically |

### What does NOT change

- Serialization: `Mint` uses `#[serde(transparent)]`, wire format is identical to bare `String`.
- All arithmetic and amount logic in `amounts.rs` — untouched.
- Security: `wallet.rs`, key material, permissions — untouched.

---

## Change 2 — Remove thin re-exports from `jupiter.rs`

### Problem

Three functions in `jupiter.rs` are pure pass-throughs to `amounts::`:

```rust
pub fn usdc_to_raw(amount: &str) -> Result<String> {
    amounts::token_to_raw(amount, USDC_DECIMALS)
}
pub fn token_to_raw(amount: &str, decimals: u8) -> Result<String> {
    amounts::token_to_raw(amount, decimals)
}
pub fn format_amount(raw: &str, decimals: u8) -> String {
    amounts::format_amount(raw, decimals)
}
```

`usdc_to_raw` and `token_to_raw` have **zero call sites** — dead code. `format_amount` has one call site in `trade.rs:295`.

### Fix

Delete all three functions from `jupiter.rs`. Update the one call site:

```rust
// trade.rs:295 — before:
let sell_display = jupiter::format_amount(&sell_raw, jupiter::GM_SOL_DECIMALS);

// after:
let sell_display = amounts::format_amount(&sell_raw, jupiter::GM_SOL_DECIMALS);
```

`trade.rs` already imports `rwa_ondo::amounts` — no new import needed.

### What does NOT change

- The underlying `amounts::format_amount` function — identical logic, identical results.
- All financial precision guarantees in `amounts.rs` — untouched.

---

## Scope

**Files modified:** `crates/ondo/src/solana/mod.rs`, `crates/ondo/src/jupiter.rs`, `crates/cli/src/cmd/gm/trade.rs`

**Files untouched:** `amounts.rs`, `wallet.rs`, `usecases/gm.rs`, `types.rs`, all other files.

**No new dependencies. No new abstractions. No behaviour change.**

---

## Anti-Overengineering Check

- No new crates, traits, or abstraction layers ✓
- No config flags or modes ✓
- No changes to public JSON schemas ✓
- Changes are purely structural — compiler validates correctness ✓
