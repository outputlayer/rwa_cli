# Code Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the last two structural gaps: type `SolanaTokenBalance.mint` as `Mint`, and delete three dead/thin re-export functions from `jupiter.rs`.

**Architecture:** Two independent, mechanical changes. No logic changes, no new abstractions, no behaviour change. The compiler validates correctness in both cases.

**Tech Stack:** Rust, `eyre`, `serde` with `#[serde(transparent)]`. No new dependencies.

---

## File Map

| File | Change |
|---|---|
| `crates/ondo/src/solana/mod.rs` | `SolanaTokenBalance.mint: String` → `mint: Mint`; update two field assignments |
| `crates/ondo/src/jupiter.rs` | Delete `usdc_to_raw`, `token_to_raw`, `format_amount` (lines 380–394) |
| `crates/cli/src/cmd/gm/trade.rs` | One line: `jupiter::format_amount` → `amounts::format_amount` |

---

## Task 1: `SolanaTokenBalance.mint: Mint`

**Files:**
- Modify: `crates/ondo/src/solana/mod.rs:173-179` (struct), `mod.rs:216` (`parse_gm_balances`), `mod.rs:292` (`get_balance`)

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` block in `crates/ondo/src/solana/mod.rs` (before the closing `}`):

```rust
#[test]
fn solana_token_balance_mint_is_mint_newtype() {
    // This test only compiles once mint: Mint (not String).
    // Also verifies Deref coercion to &str still works.
    let b = SolanaTokenBalance {
        symbol: String::new(),
        mint: Mint::from("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"),
        balance: 0.0,
        raw_amount: "0".to_string(),
    };
    assert_eq!(&*b.mint, "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
}
```

- [ ] **Step 2: Run — expect compile error**

```bash
cargo test -p rwa-ondo "solana_token_balance_mint_is_mint_newtype" 2>&1 | head -10
```

Expected: compile error — `expected String, found Mint` (or similar).

- [ ] **Step 3: Change the struct field**

In `crates/ondo/src/solana/mod.rs`, replace the struct (lines 172–179):

```rust
/// A Solana SPL token balance.
#[derive(Debug, Clone)]
pub struct SolanaTokenBalance {
    pub symbol: String,
    pub mint: Mint,
    pub balance: f64,
    /// Raw on-chain amount as string (no float precision loss).
    pub raw_amount: String,
}
```

- [ ] **Step 4: Fix `parse_gm_balances` — line 216**

Replace:
```rust
mint: info.mint.clone(),
```
with:
```rust
mint: Mint::from(&info.mint),
```

- [ ] **Step 5: Fix `get_balance` — line 292**

Replace:
```rust
mint: mint.to_string(),
```
with:
```rust
mint: mint.clone(),
```

(`mint` is already `&Mint`, so `.clone()` gives `Mint` directly. No allocation via `to_string()`.)

- [ ] **Step 6: Compile check**

```bash
cargo check -p rwa-ondo 2>&1 | head -20
```

Expected: zero errors.

- [ ] **Step 7: Run the test**

```bash
cargo test -p rwa-ondo "solana_token_balance_mint_is_mint_newtype" 2>&1 | tail -5
```

Expected: PASS.

- [ ] **Step 8: Run all ondo tests**

```bash
cargo test -p rwa-ondo 2>&1 | tail -5
```

Expected: all pass (the `parse_gm_balances_*` tests still pass because `Mint` is `#[serde(transparent)]`).

---

## Task 2: Remove thin re-exports from `jupiter.rs`

**Files:**
- Modify: `crates/ondo/src/jupiter.rs:380-394`
- Modify: `crates/cli/src/cmd/gm/trade.rs:295`

- [ ] **Step 1: Delete the three functions from `jupiter.rs`**

Remove lines 380–394 entirely (the doc comment + function body for each):

```rust
// DELETE this entire block:

/// Convert a human-readable USDC amount (e.g. "100.50") to on-chain units (6 decimals).
pub fn usdc_to_raw(amount: &str) -> Result<String> {
    amounts::token_to_raw(amount, USDC_DECIMALS)
}

/// Convert a human-readable token amount to on-chain units with specified decimals.
pub fn token_to_raw(amount: &str, decimals: u8) -> Result<String> {
    amounts::token_to_raw(amount, decimals)
}

/// Format on-chain amount to human-readable with given decimals.
#[must_use]
pub fn format_amount(raw: &str, decimals: u8) -> String {
    amounts::format_amount(raw, decimals)
}
```

- [ ] **Step 2: Update the one call site in `trade.rs`**

In `crates/cli/src/cmd/gm/trade.rs`, line 295, replace:

```rust
let sell_display = jupiter::format_amount(&sell_raw, jupiter::GM_SOL_DECIMALS);
```

with:

```rust
let sell_display = amounts::format_amount(&sell_raw, jupiter::GM_SOL_DECIMALS);
```

`trade.rs` already has `use rwa_ondo::{amounts, ...}` at the top — no new import needed.

- [ ] **Step 3: Compile check**

```bash
cargo check --workspace 2>&1 | head -20
```

Expected: zero errors.

---

## Task 3: Full test suite and commit

**Files:** none

- [ ] **Step 1: Run all tests**

```bash
cargo test --workspace 2>&1 | grep -E "^test result|FAILED"
```

Expected: all pass, 0 failures.

- [ ] **Step 2: Run clippy**

```bash
cargo clippy --all-targets 2>&1 | grep "^error" | head -10
```

Expected: zero errors.

- [ ] **Step 3: Commit**

```bash
git add crates/ondo/src/solana/mod.rs \
        crates/ondo/src/jupiter.rs \
        crates/cli/src/cmd/gm/trade.rs
git commit -m "refactor: complete Mint newtype coverage, remove dead jupiter re-exports"
```
