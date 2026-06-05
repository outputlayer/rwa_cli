# rwa — Trade Tokenized Stocks on Solana

CLI for buying & selling tokenized stocks and ETFs ([Ondo Global Markets](https://ondo.finance/)) on Solana via Jupiter. 264 tokens — TSLA, AAPL, NVDA, SPY, QQQ, and more.

> **⚠️ Beta (pre-v1).** Breaking changes possible. Not financial advice — use at your own risk, and always preview with `--dry-run` before trading real funds.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/outputlayer/rwa_cli/main/install.sh | sh
```

Binary-first: downloads a pre-built binary for Linux/macOS/Windows from [Releases](https://github.com/outputlayer/rwa_cli/releases), falling back to a source build. Pin a version with `RWA_VERSION=v0.2.9 sh`, or build directly: `cargo install --git https://github.com/outputlayer/rwa_cli --bin rwa`.

Already installed? Update in place: `rwa update` (`--check` to preview, `-y` to skip the prompt).

## Quick Start

```bash
rwa keys generate --encrypt      # 1. Create an encrypted Solana wallet
rwa keys show                    # 2. Show your address — fund it with SOL + USDC
rwa gm hours                     # 3. Check the market session
rwa gm buy TSLA 100 --dry-run    # 4. Preview a $100 TSLA buy
rwa gm buy TSLA 100 -y           # 5. Execute it
rwa gm portfolio                 # 6. See your holdings
```

Fund your wallet with **SOL** (network fees) and **USDC** (trading) before your first trade.

## Commands

| Command | Description |
|---------|-------------|
| `rwa gm hours [--tradable]` | Current session + tradable count (or list tradable tokens) |
| `rwa gm list` | All 264 tokens with tradable status |
| `rwa gm search --search <kw>` | Search by name/symbol/sector |
| `rwa gm search --tradable-only --sector <S> --type <stock\|etf>` | Bulk filtering |
| `rwa gm tradable [SYM ...]` | Check tradable status for one or many symbols |
| `rwa gm buy <SYM> <AMT> [-y] [--dry-run] [--slippage <BPS>]` | Buy with USDC |
| `rwa gm sell <SYM> <AMT> [-y] [--dry-run]` | Sell for USDC (exact, `50%`, or `all`) |
| `rwa gm close-all [<PCT>] [-y] [--parallel]` | Sell every position (or a % of each) |
| `rwa gm buy-basket <SYM AMT ...> [-y] [--parallel]` | Buy multiple tokens at once |
| `rwa gm sell-basket <SYM AMT ...> [-y] [--parallel]` | Sell multiple tokens at once |
| `rwa gm portfolio [WALLET]` | Holdings + allocation + 24h change |
| `rwa gm history <SYM> [-r RANGE]` | Price history (1D/1W/1M/3M/1Y/ALL) |
| `rwa gm send <TOKEN> <AMT> <TO> [-y] [--dry-run]` | Send SOL/USDC/tokens to another wallet |
| `rwa gm reclaim [--token <SYM>]` | Close empty token accounts, reclaim SOL rent |
| `rwa keys generate \| import \| show \| encrypt \| decrypt` | Wallet management |
| `rwa update [--check] [-y]` | Update rwa to the latest release |

## Key concepts

- **Preview first.** There's no `quote` command — use `--dry-run` on `buy`/`sell` to validate and see a quote without executing.
- **`-y`** skips the confirmation prompt (required for non-interactive/agent use).
- **`--quote-only`** (on `buy`) previews a quote for *any* size, skipping the wallet-balance check — useful to size a trade before funding. It never executes (combining with `-y` is rejected) and still blocks on a closed market or >3% slippage. Output uses the same `dry_run` shape.
- **Amounts**: exact (`100`), percentage (`50%`), or `all`. Converted with exact on-chain precision — too many decimals is rejected, never silently rounded.
- **`send` ≠ `sell`.** `sell` swaps a token to USDC; `send` transfers assets to another wallet. `send USDC all` sends your *entire* USDC balance, not just recent proceeds.
- **`close-all`** is the canonical way to exit many positions; it skips dust (< $1.50) and reports it. Use `--parallel` to run all swaps at once.
- **Slippage**: default 1% (100 bps), tunable with `--slippage`; swaps above 3% are blocked.
- **Trading sessions are ET-based** and not every token trades in every session:

  | Session | Hours (ET) |
  |---------|------------|
  | Pre-Market | 4:00 – 9:29 AM |
  | Regular | 9:30 AM – 3:59 PM |
  | Post-Market | 4:00 – 7:59 PM |
  | Overnight | 8:00 PM – 3:59 AM |
  | Closed | Weekends / NYSE holidays |

## For agents

Install the skill pack, then talk to your agent in plain language:

```bash
npx skills add outputlayer/rwa_skills -g -y
```

```
Buy $100 of TSLAon · Show my portfolio · Create a wallet · Send all USDC to <ADDRESS>
```

| Skill | Covers |
|-------|--------|
| `rwa-trade` | buy, sell, hours, tradable, close-all, baskets |
| `rwa-portfolio` | holdings, allocation, 24h change, history |
| `rwa-wallet` | create/import/encrypt, send/withdraw, reclaim, update |

Every command supports `--json` for machine-readable output — treat it as a stable contract. Agent rules: prefer `--json`, use `-y` only for real execution, `--dry-run` for previews, and never run wallet-changing commands as parallel shell processes (use the built-in `--parallel` flag instead). See [outputlayer/rwa_skills](https://github.com/outputlayer/rwa_skills) for manual install (Cursor, Claude Code, OpenCode, …).

## Reliability & RPC

The default public Solana endpoints are rate-limited with no SLA. Under load you may hit `Solana RPC error [unavailable] ... HTTP 429`. The CLI retries transient errors with backoff, but the real fix is a dedicated endpoint — a **free API key** from Alchemy, Helius, QuickNode, Chainstack, or dRPC:

```bash
export RWA_RPC_URL="https://<your-endpoint>"   # or pass --rpc-url
```

When set, the CLI uses only that endpoint (no silent fallback).

Jupiter calls use `api.jup.ag` (the public `lite-api.jup.ag` host is deprecated and throttled to ~1 request/second). Under heavy load, set a free Jupiter API key for higher limits:

```bash
export RWA_JUPITER_API_KEY="<your-jupiter-key>"
```

If the Solana RPC read fails entirely, `gm portfolio` automatically falls back to Jupiter's Ultra holdings API (`api.jup.ag/ultra/v1/holdings`) so it still returns your balances; the JSON output then includes `"source":"jupiter"`.

<details>
<summary><b>Performance & parallel speedup</b></summary>

| Command | Time |
|---------|------|
| `keys show` / `--help` | ~7 ms (local) |
| `gm hours` / `history` | ~0.5 s |
| `gm buy --dry-run` | ~0.8 s |
| `gm portfolio` | ~1.0 s (RPC + Ondo, parallel) |
| `gm list` | ~1.3 s |
| `gm buy` / `sell` | ~22 s (swap confirmation) |
| `close-all` / baskets, sequential | N×22 s + (N−1)×3 s |
| `close-all` / baskets, `--parallel` | ~22 s flat |

`--parallel` runs all swaps of *different* tokens concurrently from one wallet at normal spread (live-tested: 4-token parallel buy → immediate parallel sell, all 8 swaps at 0.2–0.7%):

| Tokens | Sequential | Parallel | Speedup |
|--------|-----------|----------|---------|
| 2 | ~47 s | ~22 s | 2.1× |
| 4 | ~97 s | ~22 s | 4.4× |
| 6 | ~147 s | ~22 s | 6.7× |
| 8 | ~197 s | ~22 s | 9.0× |
| 10 | ~247 s | ~22 s | 11.2× |

</details>

<details>
<summary><b>How it works (tokens, routing, JSON contract)</b></summary>

- Ondo GM tokens are **total-return trackers** (dividends reinvested, so 1 token ≠ 1 share). Both `TSLA` and `TSLAon` symbol forms are accepted.
- Swaps route through [Jupiter](https://jup.ag/): `lite-api.jup.ag/swap/v2` first, then `ultra-api.jup.ag`, then `lite-api.jup.ag/ultra/v1`, with `lite-api.jup.ag/swap/v1` as the final fallback. Jupiter covers gas for many swaps; you still need SOL for transfers.
- `portfolio` JSON separates cash from positions: `cash.{sol,usdc}` and `gm_positions.{value_usd, change_24h_usd, change_24h_pct, positions[].gm_alloc_pct}`. Symbols with unavailable market data are listed under `unavailable[]`.
- Trade/send/close-all JSON shapes are covered by regression tests and kept stable. Surfaced `error_kind` values include `market_closed`, `not_tradable`, `slippage_too_high`, `confirmation_timeout`, `on_chain_failure`.

</details>

<details>
<summary><b>Architecture</b></summary>

```
bin/rwa/        → binary entry point
crates/cli/     → clap parsing, human/JSON output, process lock, command orchestration
crates/ondo/    → protocol layer
  amounts.rs    → exact amount parsing/formatting/percent math
  audit.rs      → persistent JSONL audit log for swaps
  usecases/     → buy/sell/close-all flows
  solana/       → rpc/ (retry + URL race), fee, transaction, transfer, balance
  jupiter.rs + jupiter/ → swap backends (order, execute, types)
  api/          → Ondo session/pricing/history
  wallet/       → Ed25519 wallet (SLIP-10, BIP39) + tx verification
```

~13k lines of Rust, ~3 MB binary, 270+ tests. Pure Rust, no native C deps, no `unsafe`. Uses `rustls-tls` and `ed25519-dalek` (with zeroize).

</details>

## Development

```bash
cargo build
cargo test --workspace
cargo clippy --all-targets
cargo run -- gm hours
```

## License

MIT
