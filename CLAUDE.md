# RWA CLI

Rust CLI for trading tokenized stocks & ETFs (Ondo Global Markets) on Solana via Jupiter.

## Build / Test / Lint

```bash
cargo build
cargo build --release
cargo clippy --all-targets
cargo test --workspace
cargo bench -p rwa-ondo            # criterion microbenchmarks (hot CPU paths)
bash scripts/bench-latency.sh      # real command latency p50/p95 (network-bound)
cargo run -- gm hours
cargo install --path bin/rwa
```

## Install / Release model

- `install.sh` is binary-first: it downloads a pre-built release asset when available
- `install.sh` falls back to `cargo install --git ...` when a release asset is unavailable
- Release assets are produced by `.github/workflows/release.yml`
- Supported release targets: Linux, macOS, Windows

## Workspace structure

- `bin/rwa/` — thin binary entry point
- `crates/cli/` — clap parsing, human/JSON output, process lock, command orchestration
- `crates/cli/src/cmd/gm/` — trade, close_all, basket, reclaim, list, portfolio, send, shared preflight helpers
- `crates/ondo/` — protocol layer: Solana RPC, Jupiter, Ondo API, wallet
- `crates/ondo/src/usecases/` — trade orchestration: prepare, execute, order fetch, retry, preflight
- `crates/ondo/src/solana/` — RPC retry, balances, fees, transactions, transfers
- `crates/ondo/src/jupiter.rs` — Jupiter public swap backends (`swap/v2` first, then Ultra/public fallbacks)
- `crates/ondo/src/api.rs` — Ondo prices, history, session limits

## Code conventions

- Use `eyre` for errors
- Keep dependencies in root `[workspace.dependencies]`
- Avoid `.unwrap()` on fallible runtime paths
- Keep the repo pure Rust; no native C dependencies
- Avoid excessive concurrent Solana RPC calls
- Wallet-changing commands must remain sequential

## Product conventions

- Both `TSLA` and `TSLAon` are accepted token symbols
- Amounts can be exact (`100`), percentage (`50%`), or `all`
- Inputs with too many decimal places must be rejected, not silently rounded
- `send` and `sell` are different actions
- There is no `quote` command; preview uses `buy/sell --dry-run`
- `--quote-only` (buy only) previews a quote for any size by skipping the funds check; never executes, conflicts with `-y`, and emits the `dry_run` JSON shape
- `--max-bps <N>` (buy/sell/baskets/close-all) rejects trades whose quoted all-in cost (spread + fee) exceeds N bps; `RWA_MAX_BPS` is the env default; surfaced as `cost_too_high` (per-item `failed[]` entries in multi-trade commands)
- `--slippage <BPS>` is accepted by buy/sell and all multi-trade commands (baskets, close-all)
- `close-all` is the canonical path for selling many positions
- `portfolio` uses nested JSON: `cash.*` plus `gm_positions.*`
- Legacy flat `portfolio` fields should be treated as obsolete

## Commands

```bash
rwa gm hours
rwa gm hours --tradable
rwa gm list
rwa gm search --search <keyword>
rwa gm search --tradable-only --sector <SECTOR>
rwa gm tradable [SYM ...]

rwa gm buy <SYM> <AMT> --dry-run
rwa gm buy <SYM> <AMT> -y
rwa gm buy <SYM> <AMT> -y --slippage 50
rwa gm buy <SYM> <AMT> --quote-only
rwa gm sell <SYM> <AMT> --dry-run
rwa gm sell <SYM> <AMT> -y
rwa gm close-all --dry-run
rwa gm close-all -y
rwa gm close-all --parallel -y
rwa gm close-all 50% -y

rwa gm buy-basket AAPL 10 TSLA 15 NVDA 5 --dry-run
rwa gm buy-basket AAPL 10 TSLA 15 -y
rwa gm buy-basket AAPL 10 TSLA 15 --parallel -y
rwa gm sell-basket SPY 5 TSLA all --dry-run
rwa gm sell-basket SPY 5 TSLA 3 -y
rwa gm sell-basket SPY 5 TSLA 3 --parallel -y

rwa gm portfolio [WALLET]
rwa gm history <SYM> [-r RANGE]

rwa gm send <TOKEN> <AMT> <TO> --dry-run
rwa gm send <TOKEN> <AMT> <TO> -y
rwa gm reclaim
rwa gm reclaim --token <SYM>

rwa keys generate
rwa keys generate --encrypt
rwa keys import --seed-phrase|--private-key|--file
rwa keys encrypt
rwa keys decrypt
rwa keys show

rwa update --check
rwa update -y
```

`history` default range is `1M`.

## Trading / market behavior

- Trading sessions are ET-based: Pre-Market, Regular, Post-Market, Overnight, Closed
- `buy` and `sell` check tradability before calling Jupiter
- `close-all` skips tiny positions and non-tradable tokens
- `close-all` and basket trading default to sequential (3s spacing); use `--parallel` for concurrent swaps

## Jupiter behavior

- Jupiter handles gas for swaps in many cases; users still need SOL for transfers
- Jupiter requests use `api.jup.ag` (deprecated `lite-api.jup.ag` retired); set `RWA_JUPITER_API_KEY` to raise limits. Transient `/execute` failures surface as `execute_unavailable` and auto-retry; ambiguous timeouts are not retried.
- Public routing order is `lite-api.jup.ag/swap/v2` first, then `ultra-api.jup.ag`, then `lite-api.jup.ag/ultra/v1`, with `lite-api.jup.ag/swap/v1` as the final fallback
- Default slippage is 100 bps
- Quotes with >1% slippage are refreshed up to 5 times (cycles through different MMs)
- Swaps with >3% slippage are blocked after all retries exhausted
- CLI auto-retries transient swap failures; agents should not retry manually
- Surfaced trade/runtime error kinds include `market_closed`, `not_tradable`, `slippage_too_high`, `cost_too_high`, `confirmation_timeout`, `on_chain_failure`, `execute_unavailable`, `route_unfillable`, and `rpc_unavailable` (Solana RPC unreachable after retries)
- If a quoted route would fail on-chain (RFQ MM can't fill), the CLI excludes that router and refetches a quote (auto-routing to metis/dflow/…); `route_unfillable` surfaces only if all retries are exhausted
- `RWA_EXCLUDE_ROUTERS` (comma-separated, e.g. `jupiterz,dflow`) manually pins routers to avoid when quoting buy/sell; merged with the auto-excluded set
- `gm portfolio` reads from Solana RPC, falling back to the Jupiter **Ultra** holdings API on RPC `unavailable` (JSON marks `source: "jupiter"`). Swaps use Swap V2; holdings use Ultra v1.
- Modern Jupiter routes settle the swap via CPI inside the router program (no top-level SPL transfer), so swaps are verified before signing by **on-chain simulation** of balance deltas, not static byte-parsing

## Wallet behavior

- Plaintext wallet: `~/.config/rwa/key.json`
- Encrypted wallet: `~/.config/rwa/key.age`
- Unix permissions should stay `0o600`
- `RWA_PASSPHRASE` can be used for scripted access to encrypted wallets
- Sign-time guard: before signing a swap, the CLI simulates the exact Jupiter transaction and confirms the input mint is debited by no more than expected and the expected output mint is credited to this wallet by **at least the quoted amount minus slippage tolerance** (an under-delivering fill is refused, the router excluded, and the quote refetched — surfaced as `route_unfillable` if retries run out); fails closed if the RPC is unreachable

## Agent usage rules

- Always prefer `rwa --json`
- Use `-y` only for real execution
- Use `--dry-run` for large or uncertain actions
- Never run wallet-changing commands in parallel
- Treat JSON output as a stable contract for scripts and agents
- Use `gm tradable <SYM...>` to check one or many symbols
- Use `gm search --tradable-only ...` for bulk scans without Python
- Use `hours --tradable` only when the user wants the full currently tradable set
- Use `buy-basket` / `sell-basket` for multi-token trades; prefer `--parallel` for speed
- For full exit: `close-all -> reclaim -> send USDC all -> send SOL all`

## Anti-Overengineering Checklist

- Add a new layer only if it clearly reduces bugs, duplication, or agent ambiguity
- Prefer improving an existing module over creating a new crate, trait, or abstraction
- Do not add multiple ways to do the same user task; keep one canonical path
- Optimize for stable JSON/output contracts before optimizing architecture aesthetics
- Prefer typed errors at external boundaries over broad internal framework refactors
- Do not add config, flags, or modes unless they remove a real user or agent pain point
- Keep user-facing behavior simpler than internal implementation
- If a change does not make money movement safer or agent behavior clearer, question it
- Avoid future-proofing abstractions unless there is current pressure from real code
- When in doubt, choose the smaller change with the clearer behavior

## What not to do

- Do not add EVM code or dependencies
- Do not reintroduce a separate `quote` command without strong product reason
- Do not silently round user-entered amounts
- Do not replace `close-all` with manual multi-sell flows
- Do not let docs, skills, `llms.txt`, and CLI drift out of sync
