# Newtypes + Trading Reliability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Introduce `Symbol` and `Mint` newtypes through the full Rust stack (PR 1), then add three trading-reliability fixes (PR 2).

**Architecture:** PR 1 is purely mechanical — create types, follow the compiler. PR 2 is three isolated, surgical changes in `usecases/gm.rs` and `cmd/gm/trade.rs`. Each PR is a separate commit.

**Tech Stack:** Rust 2024 edition, `serde` with `#[serde(transparent)]`, `eyre`, `tokio`. No new dependencies.

---

## File Map

**PR 1 — Newtypes**

| File | Action |
|---|---|
| `crates/ondo/src/types.rs` | **Create** — `Symbol` and `Mint` newtypes |
| `crates/ondo/src/lib.rs` | Modify — add `pub mod types; pub use types::{Mint, Symbol};` |
| `crates/ondo/src/usecases/gm.rs` | Modify — `SwapPlan.symbol: Symbol`, `SwapParamsOwned.{input,output}_mint: Mint`, `prepare_buy/sell` take `&Symbol`, `resolve_gm_mint` returns `(Symbol, Mint)` |
| `crates/ondo/src/solana/mod.rs` | Modify — `get_balance(mint: &Mint)` |
| `crates/cli/src/cmd/gm/mod.rs` | Modify — `resolve_gm_mint` returns `(Symbol, Mint)`; add `use rwa_ondo::types::{Symbol, Mint}` |
| `crates/cli/src/cmd/gm/trade.rs` | Modify — wrap clap `&str` into `Symbol` at each command boundary |
| `crates/cli/src/cmd/gm/send.rs` | Modify — use `Mint` from `resolve_gm_mint` |

**PR 2 — Reliability**

| File | Action |
|---|---|
| `crates/ondo/src/usecases/gm.rs` | Modify — SOL preflight constant + check, retry log message |
| `crates/cli/src/cmd/gm/trade.rs` | Modify — `close_all` market data error → skip instead of abort |

---

## PR 1 — Newtypes `Symbol` + `Mint`

### Task 1: Create `crates/ondo/src/types.rs`

**Files:**
- Create: `crates/ondo/src/types.rs`

- [ ] **Step 1: Write the file**

```rust
use serde::{Deserialize, Serialize};
use std::ops::Deref;

/// A canonicalized GM token symbol, e.g. "TSLAon", "AAPLon".
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Symbol(String);

impl Deref for Symbol {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Symbol {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for Symbol {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Symbol {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// A Solana mint address (base58, 32 bytes decoded).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Mint(String);

impl Deref for Mint {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for Mint {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Mint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for Mint {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Mint {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_display_and_deref() {
        let s = Symbol::from("TSLAon");
        assert_eq!(s.to_string(), "TSLAon");
        assert_eq!(&*s, "TSLAon");
    }

    #[test]
    fn mint_display_and_deref() {
        let m = Mint::from("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
        assert_eq!(m.to_string(), "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
        assert_eq!(&*m, "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
    }

    #[test]
    fn symbol_from_string_and_str() {
        let a = Symbol::from("TSLA");
        let b = Symbol::from("TSLA".to_string());
        assert_eq!(a, b);
    }

    #[test]
    fn mint_ne_symbol_at_type_level() {
        // Compile-time check: Mint and Symbol are distinct types.
        // This test just confirms both can hold the same string without being equal.
        let sym = Symbol::from("TSLAon");
        let mint = Mint::from("TSLAon");
        assert_eq!(&*sym, &*mint); // same str content
        // sym == mint would be a compile error — that's the point.
    }

    #[test]
    fn symbol_serializes_as_plain_string() {
        let s = Symbol::from("TSLAon");
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "\"TSLAon\"");
    }

    #[test]
    fn mint_serializes_as_plain_string() {
        let m = Mint::from("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(json, "\"EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v\"");
    }
}
```

- [ ] **Step 2: Run the tests to verify they pass**

```bash
cargo test -p rwa-ondo types -- --nocapture
```

Expected: 6 tests pass, 0 failures.

---

### Task 2: Register `types` module in `crates/ondo/src/lib.rs`

**Files:**
- Modify: `crates/ondo/src/lib.rs`

- [ ] **Step 1: Add module declaration and re-exports**

Replace the current `lib.rs` content with:

```rust
pub mod api;
pub mod amounts;
pub mod gm;
pub mod jupiter;
pub mod solana;
pub mod token_list;
pub mod types;
pub mod usecases;
pub mod wallet;

pub use types::{Mint, Symbol};

/// USDC mint address on Solana.
pub const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

/// Shared HTTP client — reuses connection pool and TLS sessions across all API calls.
pub(crate) static HTTP: std::sync::LazyLock<reqwest::Client> = std::sync::LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("rwa-cli/0.1")
        .build()
        .expect("failed to build HTTP client")
});
```

- [ ] **Step 2: Verify it compiles**

```bash
cargo check -p rwa-ondo
```

Expected: no errors. `Symbol` and `Mint` are now accessible as `rwa_ondo::Symbol` and `rwa_ondo::Mint`.

---

### Task 3: Update `crates/ondo/src/usecases/gm.rs`

**Files:**
- Modify: `crates/ondo/src/usecases/gm.rs`

Four changes in this file:

**3a — `resolve_gm_mint` returns `(Symbol, Mint)`**

- [ ] **Step 1: Update `resolve_gm_mint`**

Replace the function at the bottom of the file (before `#[cfg(test)]`):

```rust
fn resolve_gm_mint(symbol: &Symbol, tokens: &[token_list::GmTokenEntry]) -> Result<(Symbol, Mint)> {
    let entry = gm::resolve_token(symbol, tokens)?;
    let mint = entry
        .solana_address
        .ok_or_else(|| eyre!("No Solana address for {}", entry.symbol))?;
    Ok((Symbol::from(entry.symbol), Mint::from(mint)))
}
```

**3b — `SwapPlan` and `SwapParamsOwned` structs**

- [ ] **Step 2: Update `SwapPlan` and `SwapParamsOwned` struct definitions**

Replace:
```rust
pub struct SwapPlan {
    pub symbol: String,
    pub amount: String,
    pub counter_amount: String,
    pub order: jupiter::OrderResponse,
    pub slippage_pct: Option<f64>,
    pub swap: SwapParamsOwned,
    output_decimals: u8,
}
```
with:
```rust
pub struct SwapPlan {
    pub symbol: Symbol,
    pub amount: String,
    pub counter_amount: String,
    pub order: jupiter::OrderResponse,
    pub slippage_pct: Option<f64>,
    pub swap: SwapParamsOwned,
    output_decimals: u8,
}
```

Replace:
```rust
pub struct SwapParamsOwned {
    input_mint: String,
    output_mint: String,
    raw_amount: String,
    taker: String,
    slippage_bps: Option<u32>,
}
```
with:
```rust
pub struct SwapParamsOwned {
    input_mint: Mint,
    output_mint: Mint,
    raw_amount: String,
    taker: String,
    slippage_bps: Option<u32>,
}
```

Replace:
```rust
struct SwapParams<'a> {
    input_mint: &'a str,
    output_mint: &'a str,
    raw_amount: &'a str,
    taker: &'a str,
    slippage_bps: Option<u32>,
}
```
with:
```rust
struct SwapParams<'a> {
    input_mint: &'a Mint,
    output_mint: &'a Mint,
    raw_amount: &'a str,
    taker: &'a str,
    slippage_bps: Option<u32>,
}
```

**3c — `prepare_buy` signature and body**

- [ ] **Step 3: Update `prepare_buy`**

Replace the function signature and the `resolve_gm_mint` call + `SwapParamsOwned` construction:

```rust
pub async fn prepare_buy(
    wallet: &wallet::Wallet,
    symbol: &Symbol,
    amount: &str,
    rpc_url: Option<&str>,
    slippage_bps: Option<u32>,
    json: bool,
) -> Result<SwapPlan> {
    let tokens = token_list::get_token_list();
    let (symbol, gm_mint) = resolve_gm_mint(symbol, tokens)?;
    let taker = wallet.pubkey();

    let raw_usdc = amounts::resolve_amount_to_raw(amount, jupiter::USDC_DECIMALS, || {
        let taker = taker.clone();
        let rpc = rpc_url.map(str::to_string);
        async move {
            let (_, raw) = solana::get_usdc_balance_raw(&taker, rpc.as_deref()).await?;
            Ok(raw)
        }
    })
    .await?;
    let usdc_amount = amounts::format_amount(&raw_usdc, jupiter::USDC_DECIMALS);

    let (preflight_res, tradable_res) = tokio::join!(
        preflight_buy_raw(&taker, &raw_usdc, rpc_url),
        check_tradable(&symbol),
    );
    preflight_res?;
    tradable_res?;

    let (order, slippage_pct) = get_order_checked(
        jupiter::USDC_MINT, // input: USDC
        &gm_mint,           // output: GM token
        &raw_usdc,
        &taker,
        slippage_bps,
        json,
    )
    .await?;

    Ok(SwapPlan {
        symbol,
        amount: amounts::format_amount(&order.out_amount, jupiter::GM_SOL_DECIMALS),
        counter_amount: usdc_amount,
        order,
        slippage_pct,
        swap: SwapParamsOwned {
            input_mint: Mint::from(jupiter::USDC_MINT),
            output_mint: gm_mint,
            raw_amount: raw_usdc,
            taker,
            slippage_bps,
        },
        output_decimals: jupiter::GM_SOL_DECIMALS,
    })
}
```

Note: `get_order_checked` signature stays `input_mint: &str`. Pass `&gm_mint` (which is `&Mint`) — deref coercion to `&str` applies.

**3d — `prepare_sell` signature and body**

- [ ] **Step 4: Update `prepare_sell`**

Replace the function signature and the `SwapParamsOwned` construction:

```rust
pub async fn prepare_sell(
    wallet: &wallet::Wallet,
    symbol: &Symbol,
    amount: &str,
    rpc_url: Option<&str>,
    slippage_bps: Option<u32>,
    json: bool,
) -> Result<SwapPlan> {
    let tokens = token_list::get_token_list();
    let (symbol, gm_mint) = resolve_gm_mint(symbol, tokens)?;
    let taker = wallet.pubkey();
    let gm_dec = jupiter::GM_SOL_DECIMALS;
    let is_all = amount.trim().eq_ignore_ascii_case("all");
    let is_pct = amount.trim().ends_with('%');

    preflight_sell()?;
    let (tradable_res, bal_res) = tokio::join!(
        check_tradable(&symbol),
        solana::get_balance(&taker, &gm_mint, rpc_url),
    );
    tradable_res?;
    let bal = bal_res?;
    if bal.balance <= 0.0 {
        return Err(eyre!("Balance is 0 — nothing to trade"));
    }

    let (sell_amount, raw_gm) = if is_all || is_pct {
        if is_all {
            (
                amounts::format_amount(&bal.raw_amount, gm_dec),
                bal.raw_amount.clone(),
            )
        } else {
            let pct_str = amount
                .trim()
                .strip_suffix('%')
                .ok_or_else(|| eyre!("expected percentage suffix"))?;
            let pct: f64 = pct_str
                .parse()
                .map_err(|_| eyre!("Invalid percentage: {amount}"))?;
            if !(0.0..=100.0).contains(&pct) {
                return Err(eyre!("Percentage must be 0–100, got {pct}"));
            }
            let raw: u128 = bal
                .raw_amount
                .parse()
                .map_err(|_| eyre!("Invalid on-chain amount: {}", bal.raw_amount))?;
            let pct_raw = amounts::pct_of_u128(raw, pct).to_string();
            (amounts::format_amount(&pct_raw, gm_dec), pct_raw)
        }
    } else {
        let raw = amounts::token_to_raw(amount, gm_dec)?;
        let raw_sell: u128 = raw.parse().map_err(|_| eyre!("Invalid amount: {amount}"))?;
        let raw_balance: u128 = bal
            .raw_amount
            .parse()
            .map_err(|_| eyre!("Invalid on-chain amount: {}", bal.raw_amount))?;
        if raw_sell > raw_balance {
            return Err(eyre!(
                "Insufficient {symbol} balance: have {}, trying to sell {}",
                amounts::format_amount(&bal.raw_amount, gm_dec),
                amounts::format_amount(&raw, gm_dec)
            ));
        }
        (amounts::format_amount(&raw, gm_dec), raw)
    };

    let (order, slippage_pct) = get_order_checked(
        &gm_mint,
        jupiter::USDC_MINT,
        &raw_gm,
        &taker,
        slippage_bps,
        json,
    )
    .await?;

    Ok(SwapPlan {
        symbol,
        amount: sell_amount,
        counter_amount: amounts::format_amount(&order.out_amount, jupiter::USDC_DECIMALS),
        order,
        slippage_pct,
        swap: SwapParamsOwned {
            input_mint: gm_mint,
            output_mint: Mint::from(jupiter::USDC_MINT),
            raw_amount: raw_gm,
            taker,
            slippage_bps,
        },
        output_decimals: jupiter::USDC_DECIMALS,
    })
}
```

**3e — `execute_swap` — update `SwapParams` construction**

- [ ] **Step 5: Update `execute_swap`**

Replace only the `SwapParams` construction inside `execute_swap`:

```rust
pub async fn execute_swap(wallet: &wallet::Wallet, plan: &SwapPlan, json: bool) -> Result<SwapExecution> {
    let params = SwapParams {
        input_mint: &plan.swap.input_mint,
        output_mint: &plan.swap.output_mint,
        raw_amount: &plan.swap.raw_amount,
        taker: &plan.swap.taker,
        slippage_bps: plan.swap.slippage_bps,
    };
    // rest unchanged
```

`plan.swap.input_mint: Mint`, `&plan.swap.input_mint: &Mint` → `SwapParams.input_mint: &'a Mint` ✓

**3f — Add `use crate::types::{Mint, Symbol};` import**

- [ ] **Step 6: Add import at top of `usecases/gm.rs`**

Change:
```rust
use eyre::{Result, eyre};

use crate::{amounts, api, gm, jupiter, solana, token_list, wallet};
```
to:
```rust
use eyre::{Result, eyre};

use crate::{amounts, api, gm, jupiter, solana, token_list, wallet};
use crate::types::{Mint, Symbol};
```

- [ ] **Step 7: Compile to find remaining errors**

```bash
cargo check -p rwa-ondo 2>&1 | head -40
```

Fix any remaining mismatches (typically `String::from(...)` that now need `Symbol::from(...)` or `Mint::from(...)`).

- [ ] **Step 8: Run existing tests**

```bash
cargo test -p rwa-ondo usecases
```

Expected: all tests pass (slippage tests, market closed test, etc.).

---

### Task 4: Update `crates/ondo/src/solana/mod.rs` — `get_balance`

**Files:**
- Modify: `crates/ondo/src/solana/mod.rs`

- [ ] **Step 1: Add import for `Mint`**

At the top of `solana/mod.rs`, add to the existing imports:

```rust
use crate::types::Mint;
```

- [ ] **Step 2: Update `get_balance` signature and body**

Replace the `get_balance` function (lines ~273–295):

```rust
/// Fetch balance for a specific GM token on Solana.
pub async fn get_balance(
    wallet: &str,
    mint: &Mint,
    rpc_url: Option<&str>,
) -> Result<SolanaTokenBalance> {
    let accounts: GetTokenAccountsResult = rpc_call_simple(
        "getTokenAccountsByOwner",
        serde_json::json!([wallet, { "mint": mint.as_str() }, { "encoding": "jsonParsed", "commitment": "confirmed" }]),
        rpc_url,
    ).await?;

    let parsed = accounts.accounts();
    let (balance, raw_amount) = sum_matching_raw_amounts(&parsed, mint)?;
    if raw_amount == "0" {
        return Err(eyre!("No token account found for mint {mint}"));
    }
    Ok(SolanaTokenBalance {
        symbol: String::new(),
        mint: mint.to_string(),
        balance,
        raw_amount,
    })
}
```

Note: `sum_matching_raw_amounts(&parsed, mint)` — `mint: &Mint` deref-coerces to `&str` automatically. ✓

- [ ] **Step 3: Compile check**

```bash
cargo check -p rwa-ondo 2>&1 | head -20
```

Expected: errors only from `crates/cli` (not yet updated). `rwa-ondo` itself should be clean.

---

### Task 5: Update `crates/cli/src/cmd/gm/mod.rs`

**Files:**
- Modify: `crates/cli/src/cmd/gm/mod.rs`

- [ ] **Step 1: Add `Symbol` and `Mint` to imports**

Change:
```rust
use rwa_ondo::{gm, token_list, usecases, wallet};
```
to:
```rust
use rwa_ondo::{gm, token_list, types::{Mint, Symbol}, usecases, wallet};
```

- [ ] **Step 2: Update `resolve_gm_mint` to return `(Symbol, Mint)`**

Replace the `resolve_gm_mint` function at the bottom of `mod.rs`:

```rust
fn resolve_gm_mint(symbol: &str, tokens: &[token_list::GmTokenEntry]) -> Result<(Symbol, Mint)> {
    let entry = gm::resolve_token(symbol, tokens)?;
    let mint = entry
        .solana_address
        .ok_or_else(|| eyre::eyre!("No Solana address for {}", entry.symbol))?;
    Ok((Symbol::from(entry.symbol), Mint::from(mint)))
}
```

- [ ] **Step 3: Compile check**

```bash
cargo check -p rwa-cli 2>&1 | head -40
```

Errors will appear in `send.rs` and `trade.rs` — those are fixed in Tasks 6 and 7.

---

### Task 6: Update `crates/cli/src/cmd/gm/send.rs`

**Files:**
- Modify: `crates/cli/src/cmd/gm/send.rs`

The only function that calls `resolve_gm_mint` here is `send_gm_token`. Two call sites change:

- [ ] **Step 1: Update `send_gm_token` call sites**

In `send_gm_token`, `gm_mint` is now `Mint`. Update the closure that calls `solana::get_balance`:

```rust
async fn send_gm_token(w: &wallet::Wallet, symbol: &str, amount: &str, to: &str, yes: bool, dry_run: bool, json: bool, rpc_url: Option<&str>) -> Result<()> {
    let pubkey = w.pubkey();
    let tokens = token_list::get_token_list();
    let (sym, gm_mint) = resolve_gm_mint(symbol, tokens)?;
    // sym: Symbol, gm_mint: Mint — no other changes needed here
    // because:
    //   &gm_mint deref-coerces to &str for transfer_spl ✓
    //   sym.to_string() / sym.clone() work via Display + Clone ✓
    //   get_balance(&pk, &gm_mint, ...) takes &Mint ✓

    let sol = solana::get_sol_balance(&pubkey, rpc_url).await?;
    let min_sol = solana::estimate_gas_needed(true, true, rpc_url).await;
    if sol < min_sol {
        return Err(eyre::eyre!("Insufficient SOL for gas (have {sol:.4}, need ≥{min_sol:.4})"));
    }

    let raw_str = amounts::resolve_amount_to_raw(amount, jupiter::GM_SOL_DECIMALS, || {
        let pk = pubkey.clone();
        let mint = gm_mint.clone();
        let rpc = rpc_url.map(String::from);
        async move {
            let b = solana::get_balance(&pk, &mint, rpc.as_deref()).await?;
            Ok(b.raw_amount)
        }
    }).await?;

    if raw_str == "0" {
        return Err(eyre::eyre!("Balance is 0 — nothing to send"));
    }

    let token_display = amounts::format_amount(&raw_str, jupiter::GM_SOL_DECIMALS);
    let raw: u64 = raw_str.parse().map_err(|_| eyre::eyre!("Invalid token amount"))?;

    if !json {
        println!("Send {} {sym} → {to}", token_display);
    }

    if dry_run {
        if json {
            return json_out(&SendJson {
                status: "dry_run",
                token: sym.to_string(),
                amount: token_display.clone(),
                recipient: to.into(),
                tx: String::new(),
            });
        }
        println!("[DRY RUN] Transfer not executed.");
        return Ok(());
    }

    if !yes && !json && !confirm("Proceed?") {
        return Err(eyre::eyre!("Cancelled"));
    }

    let result = solana::transfer_spl(w, to, &gm_mint, raw, 9, true, rpc_url).await?;
    let sig = &result.signature;

    if json {
        return json_out(&SendJson {
            status: if result.confirmed { "success" } else { "sent" },
            token: sym.to_string(),
            amount: token_display.clone(),
            recipient: to.into(),
            tx: format!("https://solscan.io/tx/{sig}"),
        });
    }
    println!("✓ Sent {} {sym} → {to}", token_display);
    println!("  https://solscan.io/tx/{sig}");
    if !result.confirmed {
        println!("  ⚠ Confirmation timed out — tx may still land. Check Solscan.");
    }
    Ok(())
}
```

Key: `transfer_spl(w, to, &gm_mint, ...)` — `&gm_mint: &Mint` deref-coerces to `&str` because `transfer_spl` takes `mint: &str`. ✓

- [ ] **Step 2: Compile check**

```bash
cargo check -p rwa-cli 2>&1 | grep "send.rs"
```

Expected: no errors in `send.rs`.

---

### Task 7: Update `crates/cli/src/cmd/gm/trade.rs`

**Files:**
- Modify: `crates/cli/src/cmd/gm/trade.rs`

- [ ] **Step 1: Add import at top of `trade.rs`**

Add to the existing `use` block:

```rust
use rwa_ondo::{amounts, api, jupiter, solana, token_list, usecases};
use rwa_ondo::types::Symbol;
```

- [ ] **Step 2: Update `buy` — wrap symbol into `Symbol` at boundary**

In the `buy` function, add one line immediately after `let w = load_wallet()?;`:

```rust
pub async fn buy(
    symbol: &str,
    amount: &str,
    yes: bool,
    dry_run: bool,
    json: bool,
    rpc_url: Option<&str>,
    slippage: Option<u32>,
) -> Result<()> {
    let w = load_wallet()?;
    let symbol = Symbol::from(symbol);
    let plan = usecases::gm::prepare_buy(&w, &symbol, amount, rpc_url, slippage, json).await?;

    if !json {
        println!(
            "Getting quote for {} USDC -> {} ...",
            plan.counter_amount, plan.symbol
        );
        println!("You will receive ~{} {}", plan.amount, plan.symbol);
    }

    if dry_run {
        if json {
            return json_out(&TradeJson {
                status: "dry_run",
                amount: plan.amount,
                token: plan.symbol.to_string(),
                counter_amount: plan.counter_amount,
                counter_token: "USDC",
                tx: String::new(),
                slippage_pct: plan.slippage_pct,
                price_impact_pct: plan.order.price_impact,
                fee_bps: plan.order.fee_bps,
                gasless: plan.order.gasless,
                router: plan.order.router.clone(),
            });
        }
        println!("\n[DRY RUN] Trade not executed.");
        println!("  Would buy: ~{} {}", plan.amount, plan.symbol);
        println!("  Would spend: {} USDC", plan.counter_amount);
        if let Some(pi) = plan.order.price_impact {
            println!("  Price impact: {pi:.4}%");
        }
        return Ok(());
    }

    if !yes && !json && !confirm("Proceed?") {
        println!("Cancelled.");
        return Ok(());
    }

    if !json {
        println!("Executing swap...");
    }
    let result = usecases::gm::execute_swap(&w, &plan, json).await?;

    if json {
        return json_out(&TradeJson {
            status: "success",
            amount: result.output_amount,
            token: plan.symbol.to_string(),
            counter_amount: plan.counter_amount,
            counter_token: "USDC",
            tx: format!("https://solscan.io/tx/{}", result.signature),
            slippage_pct: plan.slippage_pct,
            price_impact_pct: plan.order.price_impact,
            fee_bps: plan.order.fee_bps,
            gasless: plan.order.gasless,
            router: plan.order.router.clone(),
        });
    }

    println!("\nSwap successful!");
    println!("  Bought:    {} {}", result.output_amount, plan.symbol);
    println!("  Spent:     {} USDC", plan.counter_amount);
    println!("  Tx:        https://solscan.io/tx/{}", result.signature);
    Ok(())
}
```

- [ ] **Step 3: Update `sell`**

Replace the full `sell` function in `trade.rs`:

```rust
pub async fn sell(
    symbol: &str,
    amount: &str,
    yes: bool,
    dry_run: bool,
    json: bool,
    rpc_url: Option<&str>,
    slippage: Option<u32>,
) -> Result<()> {
    let w = load_wallet()?;
    let symbol = Symbol::from(symbol);
    let plan = usecases::gm::prepare_sell(&w, &symbol, amount, rpc_url, slippage, json).await?;

    if !json {
        println!(
            "Getting quote for {} {} -> USDC ...",
            plan.amount, plan.symbol
        );
        println!("You will receive ~{} USDC", plan.counter_amount);
    }

    if dry_run {
        if json {
            return json_out(&TradeJson {
                status: "dry_run",
                amount: plan.amount,
                token: plan.symbol.to_string(),
                counter_amount: plan.counter_amount,
                counter_token: "USDC",
                tx: String::new(),
                slippage_pct: plan.slippage_pct,
                price_impact_pct: plan.order.price_impact,
                fee_bps: plan.order.fee_bps,
                gasless: plan.order.gasless,
                router: plan.order.router.clone(),
            });
        }
        println!("\n[DRY RUN] Trade not executed.");
        println!("  Would sell: {} {}", plan.amount, plan.symbol);
        println!("  Would receive: ~{} USDC", plan.counter_amount);
        if let Some(pi) = plan.order.price_impact {
            println!("  Price impact: {pi:.4}%");
        }
        return Ok(());
    }

    if !yes && !json && !confirm("Proceed?") {
        println!("Cancelled.");
        return Ok(());
    }

    if !json {
        println!("Executing swap...");
    }
    let result = usecases::gm::execute_swap(&w, &plan, json).await?;

    if json {
        return json_out(&TradeJson {
            status: "success",
            amount: plan.amount,
            token: plan.symbol.to_string(),
            counter_amount: result.output_amount,
            counter_token: "USDC",
            tx: format!("https://solscan.io/tx/{}", result.signature),
            slippage_pct: plan.slippage_pct,
            price_impact_pct: plan.order.price_impact,
            fee_bps: plan.order.fee_bps,
            gasless: plan.order.gasless,
            router: plan.order.router.clone(),
        });
    }

    println!("\nSwap successful!");
    println!("  Sold:      {} {}", plan.amount, plan.symbol);
    println!("  Received:  {} USDC", result.output_amount);
    println!("  Tx:        https://solscan.io/tx/{}", result.signature);
    Ok(())
}
```

- [ ] **Step 4: Compile everything**

```bash
cargo check --workspace 2>&1 | head -40
```

Expected: zero errors. Fix any remaining mismatches — they will all be `String` vs `Symbol`/`Mint` type errors; use `.to_string()` to convert `Symbol`/`Mint` to `String`, and `Symbol::from(...)` / `Mint::from(...)` to convert from `&str`.

---

### Task 8: Run full test suite and commit PR 1

**Files:** none

- [ ] **Step 1: Run all tests**

```bash
cargo test --workspace
```

Expected: all 128+ tests pass. No behaviour change — only types changed.

- [ ] **Step 2: Run clippy**

```bash
cargo clippy --all-targets
```

Expected: zero warnings/errors.

- [ ] **Step 3: Commit PR 1**

```bash
git add crates/ondo/src/types.rs \
        crates/ondo/src/lib.rs \
        crates/ondo/src/usecases/gm.rs \
        crates/ondo/src/solana/mod.rs \
        crates/cli/src/cmd/gm/mod.rs \
        crates/cli/src/cmd/gm/trade.rs \
        crates/cli/src/cmd/gm/send.rs
git commit -m "feat: introduce Symbol and Mint newtypes through full stack"
```

---

## PR 2 — Trading Reliability

### Task 9: SOL preflight check in `preflight_buy_raw`

**Files:**
- Modify: `crates/ondo/src/usecases/gm.rs`

- [ ] **Step 1: Write a unit test for the SOL check first**

In the `#[cfg(test)]` block at the bottom of `usecases/gm.rs`, add after the existing tests:

```rust
#[test]
fn min_sol_for_fees_constant_is_reasonable() {
    // 0.002 SOL = 2_000_000 lamports — covers rent-exempt + typical priority fee
    let lamports = (MIN_SOL_FOR_FEES * 1_000_000_000.0) as u64;
    assert_eq!(lamports, 2_000_000);
    assert!(lamports > 0);
}
```

- [ ] **Step 2: Run the test — it fails because constant doesn't exist yet**

```bash
cargo test -p rwa-ondo usecases::tests::min_sol_for_fees_constant_is_reasonable
```

Expected: compile error — `MIN_SOL_FOR_FEES` not defined.

- [ ] **Step 3: Add constant and SOL check to `preflight_buy_raw`**

Add the constant after `MIN_SELL_VALUE_USD`:

```rust
/// Minimum SOL balance required to cover transaction fees (rent + priority).
const MIN_SOL_FOR_FEES: f64 = 0.002;
```

Replace the `preflight_buy_raw` function:

```rust
async fn preflight_buy_raw(pubkey: &str, raw_usdc_amount: &str, rpc_url: Option<&str>) -> Result<()> {
    check_trading_hours()?;
    let requested: u128 = raw_usdc_amount.parse().map_err(|_| eyre!("Invalid USDC amount: {raw_usdc_amount}"))?;
    let minimum = 10u128.pow(jupiter::USDC_DECIMALS as u32) * MIN_USDC_AMOUNT as u128;
    if requested < minimum {
        return Err(eyre!("Minimum buy amount is {MIN_USDC_AMOUNT} USDC"));
    }

    let (usdc_res, sol_raw_res) = tokio::join!(
        solana::get_usdc_balance_raw(pubkey, rpc_url),
        solana::get_sol_balance_raw(pubkey, rpc_url),
    );

    let (_, balance_raw) = usdc_res?;
    let balance: u128 = balance_raw.parse().map_err(|_| eyre!("Invalid on-chain USDC amount: {balance_raw}"))?;
    if balance < requested {
        return Err(eyre!(
            "Insufficient USDC: {:.6} USDC (need {:.6})\n  Fund wallet: {pubkey}",
            balance as f64 / 10f64.powi(jupiter::USDC_DECIMALS as i32),
            requested as f64 / 10f64.powi(jupiter::USDC_DECIMALS as i32)
        ));
    }

    let sol_raw = sol_raw_res?;
    let sol_lamports: u64 = sol_raw.parse().map_err(|_| eyre!("Invalid on-chain SOL amount: {sol_raw}"))?;
    let sol = sol_lamports as f64 / 1_000_000_000.0;
    if sol < MIN_SOL_FOR_FEES {
        return Err(eyre!(
            "Insufficient SOL for transaction fees: have {sol:.6} SOL, need ~{MIN_SOL_FOR_FEES} SOL.\n  Fund wallet: {pubkey}"
        ));
    }

    Ok(())
}
```

- [ ] **Step 4: Run the test**

```bash
cargo test -p rwa-ondo usecases::tests::min_sol_for_fees_constant_is_reasonable
```

Expected: PASS.

- [ ] **Step 5: Run all ondo tests**

```bash
cargo test -p rwa-ondo
```

Expected: all tests pass.

---

### Task 10: `close-all` continues on missing market data

**Files:**
- Modify: `crates/cli/src/cmd/gm/trade.rs`

- [ ] **Step 1: Write a unit test**

In `crates/cli/src/cmd/gm/trade.rs`, add to the `#[cfg(test)]` block (create it if it doesn't exist):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Verify that the close_all loop's market data handling doesn't panic on missing data.
    // This is tested via should_skip_position which is already covered in usecases tests.
    // The skip path here is tested by verifying the CloseSkipJson shape matches what
    // close_all would produce for a "market data unavailable" skip.
    #[test]
    fn close_skip_json_market_data_unavailable_has_correct_shape() {
        let skip = CloseSkipJson {
            token: "TSLAon".to_string(),
            estimated_usd: 0.0,
            reason: "market data unavailable",
        };
        let json = serde_json::to_value(&skip).unwrap();
        assert_eq!(json.pointer("/token"), Some(&serde_json::Value::from("TSLAon")));
        assert_eq!(json.pointer("/reason"), Some(&serde_json::Value::from("market data unavailable")));
    }
}
```

- [ ] **Step 2: Run test (should pass already — it tests the JSON shape only)**

```bash
cargo test -p rwa-cli close_skip_json_market_data_unavailable
```

Expected: PASS.

- [ ] **Step 3: Update `close_all` to skip on market data error**

In `trade.rs`, find this block inside the `close_all` loop (around line 264):

```rust
        let (price, _) = api::market_snapshot_for_symbol(&tb.symbol, &assets)
            .map_err(|e| eyre::eyre!("Invalid market data for {}: {}", tb.symbol, e))?;
        let est_value = sell_balance * price;
```

Replace with:

```rust
        let est_value = match api::market_snapshot_for_symbol(&tb.symbol, &assets) {
            Ok((price, _)) => sell_balance * price,
            Err(_) => {
                if !json {
                    eprintln!("  Skipping {} — market data unavailable", tb.symbol);
                }
                skipped.push(CloseSkipJson {
                    token: tb.symbol.clone(),
                    estimated_usd: 0.0,
                    reason: "market data unavailable",
                });
                continue;
            }
        };
```

- [ ] **Step 4: Run all tests**

```bash
cargo test --workspace
```

Expected: all tests pass.

---

### Task 11: Retry context in logs

**Files:**
- Modify: `crates/ondo/src/usecases/gm.rs`

- [ ] **Step 1: Write a test for the log message format**

In `#[cfg(test)]` block of `usecases/gm.rs`, add:

```rust
#[test]
fn execute_failure_display_includes_error_text() {
    use crate::jupiter::{ExecuteFailure, ExecuteFailureKind};
    let err = ExecuteFailure {
        kind: ExecuteFailureKind::FailedToLand,
        code: Some(-1000),
        message: "landing failure".to_string(),
    };
    let msg = err.to_string();
    // Verify the error Display includes enough context for a retry log line.
    assert!(msg.contains("landing failure"));
    assert!(msg.contains("-1000"));
}
```

- [ ] **Step 2: Run the test**

```bash
cargo test -p rwa-ondo usecases::tests::execute_failure_display_includes_error_text
```

Expected: PASS (the Display impl already includes this — test confirms it).

- [ ] **Step 3: Update retry log line in `execute_with_retry`**

In `execute_with_retry`, find:

```rust
                if !json {
                    eprintln!(
                        "Transient error (attempt {}/{}), retrying in 3s...",
                        attempt + 1,
                        MAX_SWAP_RETRIES
                    );
                }
```

Replace with:

```rust
                if !json {
                    eprintln!(
                        "Transient error (attempt {}/{}): {e}, retrying in 3s...",
                        attempt + 1,
                        MAX_SWAP_RETRIES
                    );
                }
```

- [ ] **Step 4: Compile check**

```bash
cargo check -p rwa-ondo
```

Expected: no errors.

---

### Task 12: Run full suite and commit PR 2

**Files:** none

- [ ] **Step 1: Run all tests**

```bash
cargo test --workspace
```

Expected: all tests pass.

- [ ] **Step 2: Run clippy**

```bash
cargo clippy --all-targets
```

Expected: zero warnings/errors.

- [ ] **Step 3: Commit PR 2**

```bash
git add crates/ondo/src/usecases/gm.rs \
        crates/cli/src/cmd/gm/trade.rs
git commit -m "fix: SOL preflight check, close-all market data skip, retry error context"
```
