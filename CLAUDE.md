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
- `install.sh` fails closed (exit 1) on any unverifiable download: missing `SHA256SUMS.txt` manifest, no checksum entry for the archive, or no `sha256sum`/`shasum` tool available. `RWA_INSTALL_INSECURE=1` is an explicit opt-in bypass that installs without verification (prints a warning)
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
- Amounts can be exact (`100`), percentage (`50%`), or `all`; sell amounts are in RAW tokens — for dividend-accruing tokens wallets display raw × multiplier (shares_per_token), so "sell the number Phantom shows" can exceed the raw balance — the error then names both values; use `all`/`NN%` to avoid the mismatch
- Inputs with too many decimal places must be rejected, not silently rounded
- Minimum buy amount is 5 USDC — enforced for single `buy` and per item in `buy-basket`
- `send` and `sell` are different actions
- There is no `quote` command; preview uses `buy/sell --dry-run`
- `--quote-only` (buy only) previews a quote for any size by skipping the funds check; never executes, conflicts with `-y`, and emits the `dry_run` JSON shape
- `--max-bps <N>` (buy/sell/baskets/close-all) rejects trades whose quoted all-in cost (spread + fee) exceeds N bps; `RWA_MAX_BPS` is the env default; surfaced as `cost_too_high` (per-item `failed[]` entries in multi-trade commands)
- `--limit-price <P> [share|token]` (buy/sell only) makes the trade conditional: the quote's implied price (USDC per token) must be ≤ P for buy / ≥ P for sell (equality passes), else the command fails with `condition_not_met` (exit 1, not transient) — in `--dry-run` too. The gated quote is the one executed; worst-case fill = limit ± slippage tolerance. Price is per RAW token by default; canonical form gives the unit as a space-separated second word (`--limit-price 748 share`) to compare per underlying share via the mint's scaled-UI multiplier (the raw threshold then floats with dividend accrual — intended; requires the multiplier: RPC failure fails closed with exit 75); joined forms (`--limit-price 748share`) still work. `token` (bare = token) accepted for explicitness. On success/dry-run the JSON echoes the value as `limit_price` (space-joined when given as two words). Conflicts with `--quote-only`. Canonical synthetic limit order: run it from cron/a script every N minutes until it fills (exit 0), e.g. `rwa gm buy TSLA 100 --limit-price 400 --slippage 20 -y --json`; DCA is plain scheduled `buy -y`; stop-loss is an external reference-price check + market `sell -y` (price guarantee is not wanted there). Note: on a low-SOL wallet a real `-y` run may auto-refuel gas (USDC→SOL) before the condition check — bounded, and disabled by `RWA_NO_AUTO_GAS=1`
- `--slippage <BPS>` is accepted by buy/sell and all multi-trade commands (baskets, close-all)
- `close-all` is the canonical path for selling many positions
- `portfolio` uses nested JSON: `cash.*` plus `gm_positions.*`
- Every CLI money operation is appended to a per-wallet trade ledger (`~/.config/rwa/ledger/<pubkey>.jsonl`, raw units). Each entry carries a `prev` hash chain link (hex of the first 16 bytes of SHA-256 over the raw previous line; genesis sentinel is the hash of empty bytes) — tamper-evident, not tamper-proof. `gm pnl` builds average entry price, realized and unrealized P&L **from buys/sells only** (deposits/withdrawals are ignored by design); sells beyond CLI-recorded buys are flagged `oversold` and excluded rather than mispriced. `PnlJson.ledger_integrity` is ALWAYS present: `"ok"` (chain intact), `"legacy"` (pre-chain entries present, unbroken), or `"broken@line N"` (first line whose `prev` doesn't match); a broken chain warns on stderr (human mode only; in `--json` the signal is the `ledger_integrity` field itself) but never fails `pnl`
- Legacy flat `portfolio` fields should be treated as obsolete

## Commands

```bash
rwa gm hours
rwa gm hours --tradable
rwa gm list
rwa gm search --search <keyword>
rwa gm search --tradable-only --sector <SECTOR>
rwa gm search --tag <LABEL>               # any Ondo tag: asset class, region, factor (asia, dividend, "fixed income", ...)
rwa gm tradable [SYM ...]

rwa gm buy <SYM> <AMT> --dry-run
rwa gm buy <SYM> <AMT> -y
rwa gm buy <SYM> <AMT> -y --slippage 50
rwa gm buy <SYM> <AMT> --quote-only
rwa gm buy <SYM> <AMT> --limit-price <P> -y           # execute only if quoted price <= P (USDC/token)
rwa gm sell <SYM> <AMT> --limit-price <P> share -y    # execute only if quoted price >= P (USDC/share)
rwa gm sell <SYM> <AMT> --dry-run
rwa gm sell <SYM> <AMT> -y
rwa gm close-all --dry-run
rwa gm close-all -y
rwa gm close-all --sequential -y   # rate-limit-friendly fallback
rwa gm close-all 50% -y

rwa gm buy-basket AAPL 10 TSLA 15 NVDA 5 --dry-run
rwa gm buy-basket AAPL 10 TSLA 15 -y
rwa gm buy-basket AAPL 10 TSLA 15 --sequential -y
rwa gm sell-basket SPY 5 TSLA all --dry-run
rwa gm sell-basket SPY 5 TSLA 3 -y
rwa gm sell-basket SPY 5 TSLA 3 --sequential -y

rwa gm portfolio [WALLET]
rwa gm pnl                          # cost basis + realized/unrealized P&L from CLI trades
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
rwa keys add <NAME> --path <PATH>                       # register an existing key file by name
rwa keys add <NAME> --seed-phrase "..." --path <PATH>   # import a seed phrase to <PATH> (encrypted by default) and register
rwa keys add <NAME> --private-key <KEY> --path <PATH>   # import a base58/hex/base64 key to <PATH> and register
rwa keys export --reveal            # print private key (base58 + JSON) and stored recovery phrase
rwa keys list                       # list named wallets (* = active)
rwa keys use <NAME>                 # set the active wallet
rwa keys remove <NAME>              # unregister (key file is NOT deleted)
rwa --wallet <NAME> gm buy ...      # select a wallet for one command

rwa update --check
rwa update -y
```

`history` default range is `1M`.

## Trading / market behavior

- Trading sessions are ET-based: Pre-Market, Regular, Post-Market, Overnight, Closed
- Ondo trades 24/7: weekends/holidays ("Closed") map to the API's `offhours` session, where only select flagship tokens (TSLAon, NVDAon, SPYon, QQQon, GOOGLon, …) are tradable. There is no blanket weekend block — per-token gating decides; a non-offhours token gets `market_closed`
- Ondo assets and session-limits responses are disk-cached read-through for 60s (`~/.config/rwa/.cache/`); stale fallback on network failure (assets 1h, limits 10m). Explicit URL overrides bypass the cache
- `buy` and `sell` check tradability before calling Jupiter
- `close-all` skips tiny positions and non-tradable tokens
- `close-all` and basket trading default to **parallel** (bounded by internal order/execute semaphores); `--sequential` opts into one-at-a-time with 3s spacing (rate-limit fallback); `--parallel` is accepted as a no-op for compatibility
- Swap confirmation happens server-side in Jupiter `/execute` (no local wait); `send` and Metis-fallback swaps confirm locally at `confirmed` commitment; `reclaim` batches confirm at `processed` (fast path — low-stakes rent, err field already authoritative)
- GM tokens are total-return trackers (Token-2022 Scaled UI): dividends accrue via the mint's multiplier — wallet-displayed balances = raw × multiplier, and token price = share price × multiplier. The CLI's canonical frame is RAW everywhere (amounts, prices, ledger, pnl); `portfolio` values positions as raw × price and exposes `shares_per_token` when the multiplier ≠ 1; trade previews add informational `share_price`/`shares_per_token`
- Ondo pauses individual assets around dividend events (ex-dividend windows; ETFs longer): surfaced as `trading_paused` (exit 1, not transient) on buy/sell, `skipped[]` reason in close-all, per-item `failed[]` entry with `error_kind: "trading_paused"` in buy-basket/sell-basket, and an optional `trading_paused: true` flag in `list`/`search`/`tradable` JSON. The pause check fails open (with a stderr warning) if the Ondo assets API is unreachable

## Jupiter behavior

- Auto gas refuel: before a real `buy`/`buy-basket`/`send` (never `send SOL`, never dry-run), if SOL < 0.003 and USDC covers the operation + 5 USDC, the CLI buys SOL first. The size is **dynamic**: target = 2x (50 txs at the live fee estimate + 5 ATA rents), converted to USDC at the quote's implied SOL price, clamped to [5, 25] USDC (bootstrap from zero SOL requires a gasless route); interactive runs prompt, `-y` auto-approves, `--json` alone (no `-y`) skips the refuel silently rather than prompting or auto-approving (same consent gate as execution itself, since v0.6.0), `RWA_NO_AUTO_GAS=1` disables; surfaced as optional `gas_refuel: {usdc, sol, tx}` in `TradeJson`/`BuyBasketResultJson`/`SendJson`. Best-effort: an impossible refuel never fails the main operation
- USDC-only wallets (zero SOL) can trade: the SOL-for-fees check runs AFTER quoting and passes when the route is gasless (Jupiter pays fees + ATA rent); only non-gasless routes (Metis fallback) require ~0.002 SOL, surfaced as `insufficient_funds`. `send` and `reclaim` always need SOL
- Jupiter requests use `api.jup.ag` (deprecated `lite-api.jup.ag` retired); set `RWA_JUPITER_API_KEY` to raise limits. Transient `/execute` failures surface as `execute_unavailable` and auto-retry; ambiguous timeouts are not retried.
- Public routing order is `lite-api.jup.ag/swap/v2` first, then `ultra-api.jup.ag`, then `lite-api.jup.ag/ultra/v1`, with `lite-api.jup.ag/swap/v1` as the final fallback
- Default slippage is 100 bps
- Quotes with >1% slippage are refreshed up to 5 times (cycles through different MMs)
- Swaps with >3% slippage are blocked after all retries exhausted
- CLI auto-retries transient swap failures; agents should not retry manually
- Surfaced trade/runtime error kinds include `market_closed`, `not_tradable`, `slippage_too_high`, `cost_too_high`, `confirmation_timeout`, `on_chain_failure`, `execute_unavailable`, `route_unfillable`, `rpc_unavailable` (Solana RPC unreachable after retries), `amount_below_minimum`, `condition_not_met` (`--limit-price` unmet), `trading_paused` (Ondo dividend-window pause), `insufficient_funds`, `no_position`, and `confirmation_required` (`--json` without `-y` on a money-moving command; exit 1, not transient)
- The `/order` quote-fetch retry loop backs off up to 3.2s between attempts (800ms · 2^attempt, capped), within a ~20s retry budget per quote-fetch pass (fresh budget per outer retry, e.g. slippage refresh or route-unfillable requote); a stderr heartbeat (`still fetching quote (...)`) reports each retry so a slow quote is visible, not silent
- If a quoted route would fail on-chain (RFQ MM can't fill) or under-delivers vs its own quote, the CLI excludes that router and refetches a quote (auto-routing to metis/dflow/…); `route_unfillable` surfaces only if all retries are exhausted. A rerouted quote materially worse than the previewed one (beyond slippage tolerance) aborts with `slippage_too_high` instead of executing silently
- `RWA_EXCLUDE_ROUTERS` (comma-separated, e.g. `jupiterz,dflow`) manually pins routers to avoid when quoting buy/sell; merged with the auto-excluded set
- `gm portfolio` reads from Solana RPC, falling back to the Jupiter **Ultra** holdings API on RPC `unavailable` (JSON marks `source: "jupiter"`). Swaps use Swap V2; holdings use Ultra v1.
- Modern Jupiter routes settle the swap via CPI inside the router program (no top-level SPL transfer), so swaps are verified before signing by **on-chain simulation** of balance deltas, not static byte-parsing

## Wallet behavior

- `keys generate` is mnemonic-first: the wallet derives from a fresh 12-word BIP39 phrase at `m/44'/501'/0'/0'` (Phantom/Solflare-compatible); the phrase is printed once. Encrypted wallets embed the phrase inside the age payload (`{key, mnemonic}` object; legacy bare arrays still load); plaintext `key.json` never stores it
- `keys export` prints the base58 keypair (Phantom import format), the solana-keygen JSON array, and the stored recovery phrase; gated behind an interactive confirmation or `--reveal` (mandatory with `--json`)
- Plaintext wallet: `~/.config/rwa/key.json`
- Encrypted wallet: `~/.config/rwa/key.age`
- Unix permissions should stay `0o600`
- `RWA_PASSPHRASE` can be used for scripted access to encrypted wallets; passphrases are held in `Zeroizing<String>` end-to-end (zeroed on drop) rather than plain `String`
- Multiple named wallets: registry at `~/.config/rwa/wallets.toml` maps a name to an absolute key-file path plus an `active` pointer (file is `0o600`; holds paths, not keys)
- Wallet selection priority: `--wallet <name>` / `RWA_WALLET` > registry `active` > legacy `key.json`/`key.age` default. An absent/empty registry behaves exactly as the old single-wallet setup
- Key files stay wherever the user registered them; key type (plaintext vs age) is detected by file content, not extension
- The legacy `key.json`/`key.age` is auto-registered as `default` the first time a `keys add/list/use` command runs
- `keys encrypt`/`decrypt` operate on the legacy default location only and reconcile any registry entry that pointed at the renamed file; named external wallets are managed in the form they were registered
- Selecting an unknown wallet fails with a non-transient error (exit 1) listing available names
- Sign-time guard: before signing a swap, the CLI simulates the exact Jupiter transaction and confirms the input mint is debited by no more than expected and the expected output mint is credited to this wallet by **at least the quoted amount minus slippage tolerance** (an under-delivering fill is refused, the router excluded, and the quote refetched — surfaced as `route_unfillable` if retries run out); fails closed if the RPC is unreachable

## Agent usage rules

- Always prefer `rwa --json`
- Use `-y` only for real execution
- **`--json` without `-y` never executes** (breaking since v0.6.0): a money-moving command run with `--json` alone fails closed with `confirmation_required` (exit 1) instead of running non-interactively. This also gates auto-gas refuel: json auto-approves the refuel only when `-y` is also passed. Add `-y` to execute, or `--dry-run` to preview
- Use `--dry-run` for large or uncertain actions
- Never run wallet-changing commands in parallel
- Treat JSON output as a stable contract for scripts and agents
- Exit codes: 75 (EX_TEMPFAIL) for transient failures worth retrying (`rpc_unavailable`, `execute_unavailable`, `confirmation_timeout`, lock contention); 1 for everything else
- Use `gm tradable <SYM...>` to check one or many symbols
- Use `gm search --tradable-only ...` for bulk scans without Python
- `list`/`search` JSON items carry optional `asset_class` and `region` (Ondo tags) alongside `sector`; sector-less ETFs display `asset class · region`. `--tag <label>` filters across ALL Ondo tag categories (sector, asset class, region, factor/risk — 24 factor labels like Large Cap/Dividend/High Yield); `--search` matches them too
- Use `hours --tradable` only when the user wants the full currently tradable set
- Use `buy-basket` / `sell-basket` for multi-token trades; parallel is the default (use `--sequential` only if rate-limited)
- For full exit: `close-all -> reclaim -> send USDC all -> send SOL all`
- Use `--wallet <name>` (or `RWA_WALLET`) to pick among named wallets; `rwa keys list --json` returns `{wallets:[{name,path,pubkey,active,encrypted}]}`

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
