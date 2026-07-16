# RWA CLI

Rust CLI for trading tokenized stocks & ETFs (Ondo Global Markets) on Solana via Jupiter.

## Build / Test / Lint

```bash
make ci                            # RUN BEFORE EVERY PUSH — exact mirror of .github/workflows/ci.yml
make install-hooks                 # optional: pre-push hook that runs `make ci` automatically
cargo build
cargo build --release
cargo clippy --all-targets
cargo test --workspace
cargo bench -p rwa-ondo            # criterion microbenchmarks (hot CPU paths)
bash scripts/bench-latency.sh      # real command latency p50/p95 (network-bound)
cargo run -- gm hours
cargo install --path bin/rwa
```

**CI runs every step with `RUSTFLAGS=-Dwarnings`** (clippy lints, `unsafe_code`, `dead_code`, … are hard errors), and plain local `cargo test`/`cargo clippy` (no `-Dwarnings`, plus clippy's incremental cache) can be green while CI is red. `make ci` reproduces CI exactly — always run it before pushing. Release steps are in `RELEASING.md`.

**What CI (and `make ci`) CANNOT verify — live-only paths.** The real on-chain money paths never run in CI (no wallet, no live Jupiter/RPC): `usecases/gm_execute.rs` (the `/execute` submit + retry loop), `jupiter/execute.rs` submit, `solana/transfer.rs` (`send`), and `usecases/gm_gas.rs` (auto-refuel). These are exactly the sub-60%-coverage files, and they're validated by **live-money runs**, not tests (a real `--dry-run` then a small `-y` trade, e.g. on a flagship token). If you change any of them, a green `make ci` is necessary but NOT sufficient — do a live dry-run + small real trade before merging, and keep sends to a known test address. Everything else (quote math, slippage/limit/cost gates, the sign-time delta checks against a mocked RPC, amount parsing, ledger/pnl, JSON shapes via the spawned-binary `cli_contract` suite) IS covered by tests.

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
- `--max-bps <N>` (buy/sell/baskets/close-all) rejects trades whose quoted all-in cost (spread + fee) exceeds N bps; `RWA_MAX_BPS` is the env default; surfaced as `cost_too_high` (per-item `failed[]` entries in multi-trade commands). All-in cost nets the fee against price-impact (`fee_bps − slippage_pct·100`), so a **favorable spread offsets the fee** — a quote with a 10-bps fee but a better-than-quoted price can pass `--max-bps 5` (even `--max-bps 0`); the cap is on net cost, not the raw fee
- `--total <USDC>` (buy-basket only) splits one USDC total across percent-weight pairs (`TSLA 50% NVDA 30% SPY 20% --total 1000`): weights must be percentages summing to exactly 100 (up to 6 decimal places), each computed item floors to raw USDC with the dust going to the largest weight (spent sum == total exactly), and every item must clear the 5 USDC minimum — all validated before any wallet/network access (`invalid_amount` / `amount_below_minimum`). Percent amounts without `--total`, absolute amounts with it, and duplicate symbols are rejected. JSON gains an optional `allocation: {total, weights}` echo (`allocation.weights` keys echo the symbols as entered; `bought[].token` echoes the canonical symbol, e.g. `AALon`, in `--dry-run` previews and the as-entered symbol on real runs — a pre-existing basket convention)
- `--limit-price <P> [share|token]` (buy/sell only) makes the trade conditional: the quote's implied price (USDC per token) must be ≤ P for buy / ≥ P for sell (equality passes), else the command fails with `condition_not_met` (exit 1, not transient) — in `--dry-run` too. The gated quote is the one executed; worst-case fill = limit ± slippage tolerance. Price is per RAW token by default; canonical form gives the unit as a space-separated second word (`--limit-price 748 share`) to compare per underlying share via the mint's scaled-UI multiplier (the raw threshold then floats with dividend accrual — intended; requires the multiplier: RPC failure fails closed with exit 75); joined forms (`--limit-price 748share`) still work. `token` (bare = token) accepted for explicitness. On success/dry-run the JSON echoes the value as `limit_price` (space-joined when given as two words). Conflicts with `--quote-only`. Canonical synthetic limit order: run it from cron/a script every N minutes until it fills (exit 0), e.g. `rwa gm buy TSLA 100 --limit-price 400 --slippage 20 -y --json`; DCA is plain scheduled `buy -y`; stop-loss is an external reference-price check + market `sell -y` (price guarantee is not wanted there). Note: on a low-SOL wallet a real `-y` run may auto-refuel gas (USDC→SOL) before the condition check — bounded, and disabled by `RWA_NO_AUTO_GAS=1`
- `--slippage <BPS>` is accepted by buy/sell and all multi-trade commands (baskets, close-all)
- `close-all` is the canonical path for selling many positions
- `portfolio` uses nested JSON: `cash.*` plus `gm_positions.*`
- Every CLI money operation is appended to a per-wallet trade ledger (`~/.config/rwa/ledger/<pubkey>.jsonl`, raw units). Each entry carries a `prev` hash chain link (hex of the first 16 bytes of SHA-256 over the raw previous line; genesis sentinel is the hash of empty bytes) — tamper-evident, not tamper-proof. `gm pnl` builds average entry price, realized and unrealized P&L **from buys/sells only** (deposits/withdrawals are ignored by design); sells beyond CLI-recorded buys are flagged `oversold` and excluded rather than mispriced. `PnlJson.ledger_integrity` is ALWAYS present: `"ok"` (chain intact), `"legacy"` (pre-chain entries present, unbroken), or `"broken@line N"` (first line whose `prev` doesn't match); a broken chain warns on stderr (human mode only; in `--json` the signal is the `ledger_integrity` field itself) but never fails `pnl`
- Legacy flat `portfolio` fields should be treated as obsolete

## Commands

```bash
rwa gm hours                # + offhours flagship count and paused count (dividends + weekend-closed; fail-open)
rwa gm hours --tradable     # also lists the offhours flagship symbols
rwa gm list
rwa gm search --search <keyword>
rwa gm search --tradable-only --sector <SECTOR>
rwa gm search --type <stock|etf>          # filter by instrument type (case-insensitive)
rwa gm search --name-keyword <WORD>       # filter by company-name keyword (repeatable)
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
rwa gm buy-basket TSLA 50% NVDA 30% SPY 20% --total 1000 --dry-run
rwa gm buy-basket TSLA 50% NVDA 30% SPY 20% --total 1000 -y
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

rwa keys generate                       # encrypted by default (prompts for a passphrase)
rwa keys generate --allow-plaintext     # opt out of encryption (not recommended)
rwa keys import --seed-phrase|--private-key|--file
rwa keys encrypt
rwa keys decrypt
rwa keys show
rwa keys add <NAME> --path <PATH>                       # register an existing key file by name
rwa keys add <NAME> --seed-phrase "..." --path <PATH>   # import a seed phrase to <PATH> (encrypted by default) and register
rwa keys add <NAME> --seed-phrase "..." --account 1 --path <PATH>              # import at m/44'/501'/1'/0' (Phantom "Account 2")
rwa keys add <NAME> --seed-phrase "..." --derivation-path "m/44'/501'/1'/0'" --path <PATH>  # same, explicit BIP44 path
rwa keys add <NAME> --private-key <KEY> --path <PATH>   # import a base58/hex/base64 key to <PATH> and register
rwa keys export --reveal            # print private key (base58 + JSON) and stored recovery phrase
rwa keys store-passphrase           # verify + save the encrypted wallet's passphrase in the OS keychain (TTY-only)
rwa keys forget-passphrase          # remove the stored passphrase from the OS keychain
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
- `close-all` and basket trading default to **parallel** (bounded by internal order/execute semaphores); `--sequential` opts into one-at-a-time with 3s spacing (rate-limit fallback). The legacy no-op `--parallel` flag was removed in v0.7.2 (it now errors — parallel needs no flag)
- Parallel quotes are **staggered and adaptive** (`RWA_QUOTE_STAGGER_MS`, default 350ms between launches): firing a basket's quotes all at once bursts Jupiter's per-wallet rate limit, whose retry backoff (0.8/1.6/3.2s) costs far more than the stagger. The stagger keeps the overlap while dodging the burst; best case a 3-token basket quotes in ~1.3s. **Once Jupiter starts pushing back** (the process-wide `/order` retry counter climbs past the launcher's baseline), remaining launches widen to the serial `SEQUENTIAL_SPACING` (3s) so the CLI stops feeding the burst — a keyless-friendly adaptive back-off that self-tunes to whatever per-wallet limit the user has (fast when the endpoint is generous, graceful serial pace when it throttles). a configured `RWA_JUPITER_API_KEY` auto-selects the keyed profile: stagger 0 + `/order` concurrency 4 (vs 2 keyless); an explicit `RWA_QUOTE_STAGGER_MS` always overrides. The same staggered adaptive launcher paces REAL `-y` runs (baskets and close-all fetch+execute chains), not just dry-run quotes
- Swap confirmation happens server-side in Jupiter `/execute` (no local wait); `send` and Metis-fallback swaps confirm locally at `confirmed` commitment; `reclaim` batches confirm at `processed` (fast path — low-stakes rent, err field already authoritative)
- GM tokens are total-return trackers (Token-2022 Scaled UI): dividends accrue via the mint's multiplier — wallet-displayed balances = raw × multiplier, and token price = share price × multiplier. The CLI's canonical frame is RAW everywhere (amounts, prices, ledger, pnl); `portfolio` values positions as raw × price and exposes `shares_per_token` when the multiplier ≠ 1; trade previews add informational `share_price`/`shares_per_token`
- Ondo flags an asset `is_trading_paused` for TWO reasons: a dividend window (ex-dividend; ETFs longer) OR the market being closed for it (on weekends/holidays most non-24/7 tokens are flagged paused — the paused check runs before the session check, so a weekend non-flagship surfaces `trading_paused`, not `market_closed`). Surfaced as `trading_paused` (exit 1, not transient) on buy/sell, `skipped[]` reason in close-all, per-item `failed[]` entry with `error_kind: "trading_paused"` in buy-basket/sell-basket, and an optional `trading_paused: true` flag in `list`/`search`/`tradable` JSON. The pause check fails open (with a stderr warning) if the Ondo assets API is unreachable

## Jupiter behavior

- Auto gas refuel: before a real `buy`/`buy-basket`/`send` (never `send SOL`, never dry-run), if SOL < 0.003 and USDC covers the operation + 5 USDC, the CLI buys SOL first. The size is **dynamic**: target = 2x (50 txs at the live fee estimate + 5 ATA rents), converted to USDC at the quote's implied SOL price, clamped to [5, 25] USDC (bootstrap from zero SOL requires a gasless route); interactive runs prompt, `-y` auto-approves, `--json` alone (no `-y`) skips the refuel silently rather than prompting or auto-approving (same consent gate as execution itself, since v0.6.0), `RWA_NO_AUTO_GAS=1` disables; surfaced as optional `gas_refuel: {usdc, sol, tx}` in `TradeJson`/`BuyBasketResultJson`/`SendJson`. Best-effort: an impossible refuel never fails the main operation
- USDC-only wallets (zero SOL) can trade: the SOL-for-fees check runs AFTER quoting and passes when the route is gasless (Jupiter pays fees + ATA rent); only non-gasless routes (Metis fallback) require ~0.002 SOL, surfaced as `insufficient_funds`. `send` and `reclaim` always need SOL
- Jupiter requests use `api.jup.ag` (deprecated `lite-api.jup.ag` retired); set `RWA_JUPITER_API_KEY` to raise limits. Transient `/execute` failures surface as `execute_unavailable` and auto-retry; ambiguous timeouts are not retried.
- Public routing order is `lite-api.jup.ag/swap/v2` first, then `ultra-api.jup.ag`, then `lite-api.jup.ag/ultra/v1`, with `lite-api.jup.ag/swap/v1` as the final fallback
- Default slippage is 100 bps
- Quotes with >1% slippage are refreshed up to 5 times (cycles through different MMs)
- Swaps with >3% slippage are blocked after all retries exhausted
- CLI auto-retries transient swap failures; agents should not retry manually
- Surfaced trade/runtime error kinds include `market_closed`, `not_tradable`, `slippage_too_high`, `cost_too_high`, `confirmation_timeout`, `on_chain_failure`, `execute_unavailable`, `route_unfillable`, `rpc_unavailable` (Solana RPC unreachable after retries), `amount_below_minimum`, `condition_not_met` (`--limit-price` unmet), `trading_paused` (Ondo pause: a dividend window, or the market being closed for the asset — check `gm hours` for when it resumes), `insufficient_funds`, `no_position`, and `confirmation_required` (`--json` without `-y` on a money-moving command; exit 1, not transient)
- Input/environment error kinds (previously `error_kind: null`): `unknown_token` (symbol not in the GM list), `invalid_amount` (bad format/too many decimals/bad percentage), `invalid_address` (send recipient), `unknown_wallet` (`--wallet`/`RWA_WALLET` miss; message lists available names), `self_send` (send to own address), `reveal_required` (`keys export --json` without `--reveal`), `interactive_required` (admin command — keys export --reveal / keys decrypt — run without a TTY: type the passphrase interactively; env/keychain are not consulted for these), and `lock_contention` (another rwa process holds the lock; **transient — exit 75 in both human and json modes**)
- The `/order` quote-fetch retry loop backs off up to 3.2s between attempts (800ms · 2^attempt, capped), within a ~20s retry budget per quote-fetch pass (fresh budget per outer retry, e.g. slippage refresh or route-unfillable requote); a stderr heartbeat (`still fetching quote (...)`) reports each retry so a slow quote is visible, not silent
- If a quoted route would fail on-chain (RFQ MM can't fill) or under-delivers vs its own quote, the CLI excludes that router and refetches a quote (auto-routing to metis/dflow/…); `route_unfillable` surfaces only if all retries are exhausted. A rerouted quote materially worse than the previewed one (beyond slippage tolerance) aborts with `slippage_too_high` instead of executing silently
- RFQ market makers fund the fill **just-in-time** at `/execute` (they mint/position inventory in the same block, e.g. Ondo GM tokens via `MintWithUsdc`), which a pre-execute simulation can't see. So on a **managed `/execute` route that funds just-in-time** (detected via `gasless`), a simulation that merely ERRORED (`OnChainWouldFail`) is submitted anyway — Jupiter validates server-side, the maker co-signs, and the on-chain min-output (`otherAmountThreshold`) enforces honesty; surfaced as a stderr `note:`. This is what makes thin RFQ-only tokens (PFEon/LLYon) buyable from the CLI, matching the Ondo app. A simulation that SUCCEEDED but showed dishonest deltas (`UnsafeDelta`/`OutputBelowQuote`), one that couldn't run (`RpcUnavailable`), or ANY failure on the Metis direct-submit path is still a hard refusal. `route_unfillable` now surfaces only for a genuinely unfillable route after retries
- `RWA_EXCLUDE_ROUTERS` (comma-separated, e.g. `jupiterz,dflow`) manually pins routers to avoid when quoting buy/sell; merged with the auto-excluded set
- `RWA_DEBUG=1` restores verbose diagnostics that are suppressed by default: the full pre-sign simulation program-log (otherwise condensed to a one-line cause) and the per-backend `/order` quote-failure list
- `gm portfolio` reads from Solana RPC, falling back to the Jupiter **Ultra** holdings API on RPC `unavailable` (JSON marks `source: "jupiter"`). Swaps use Swap V2; holdings use Ultra v1.
- Modern Jupiter routes settle the swap via CPI inside the router program (no top-level SPL transfer), so swaps are verified before signing by **on-chain simulation** of balance deltas, not static byte-parsing

## Wallet behavior

- `keys generate` is mnemonic-first: the wallet derives from a fresh 12-word BIP39 phrase at `m/44'/501'/0'/0'` (Phantom/Solflare-compatible); the phrase is printed once. Encrypted wallets embed the phrase inside the age payload (`{key, mnemonic}` object; legacy bare arrays still load); plaintext `key.json` never stores it
- Seed-phrase import (`keys import`/`keys add --seed-phrase`) accepts an optional derivation path: `--account <N>` (→ `m/44'/501'/N'/0'`; `N=0` default, matches Phantom/Solflare "Account N+1") or the mutually-exclusive `--derivation-path <PATH>` (full BIP44). Both apply only to seed import (rejected on `--file`/`--private-key`). A non-default path warns on stderr that the recovery phrase alone restores account 0 — the path must be recorded to restore that wallet elsewhere (the path is NOT yet stored in the payload; re-import with the same `--derivation-path`)
- `keys export` prints the base58 keypair (Phantom import format), the solana-keygen JSON array, and the stored recovery phrase; gated behind an interactive confirmation or `--reveal` (mandatory with `--json`)
- Config paths below are shown as `~/.config/rwa/...` (the Linux XDG config dir). The CLI actually uses the **platform config dir** (`dirs::config_dir()`): on macOS that's `~/Library/Application Support/rwa/`, on Windows `%APPDATA%\rwa\`. Substitute accordingly
- Plaintext wallet: `~/.config/rwa/key.json`
- Encrypted wallet: `~/.config/rwa/key.age`
- Unix permissions should stay `0o600`
- `RWA_PASSPHRASE` can be used for scripted access to operational commands on encrypted wallets; passphrases are held in `Zeroizing<String>` end-to-end (zeroed on drop) rather than plain `String`
- Multiple named wallets: registry at `~/.config/rwa/wallets.toml` maps a name to an absolute key-file path plus an `active` pointer (file is `0o600`; holds paths, not keys)
- Wallet selection priority: `--wallet <name>` / `RWA_WALLET` > registry `active` > legacy `key.json`/`key.age` default. An absent/empty registry behaves exactly as the old single-wallet setup
- Key files stay wherever the user registered them; key type (plaintext vs age) is detected by file content, not extension
- The legacy `key.json`/`key.age` is auto-registered as `default` the first time a `keys add/list/use` command runs
- `keys encrypt`/`decrypt` operate on the legacy default location only and reconcile any registry entry that pointed at the renamed file; named external wallets are managed in the form they were registered. Both speak `--json` (`{status, action, path, encrypted}`)
- **`keys encrypt` does NOT store the recovery phrase** (and `keys decrypt` → `keys encrypt` therefore loses it): it encrypts the plaintext `key.json`, which never held the mnemonic — only `keys generate` and seed import embed the phrase in the age payload. After encrypting, `keys export --reveal` shows the private key but `mnemonic` is null. `keys encrypt` warns about this on stderr; the wallet is still fully recoverable from the private key, just not from a phrase
- Selecting an unknown wallet fails with a non-transient error (exit 1) listing available names
- Two passphrase classes: operational commands (trading, `keys show`) read `RWA_PASSPHRASE` → OS keychain (per wallet name) → interactive prompt — recommended desktop-agent setup: encrypted wallet + keychain, no env passphrase, so the agent can trade but never learns the secret; admin commands (`keys export --reveal`, `keys decrypt`, `keys store-passphrase`) accept ONLY a live TTY prompt — env and keychain are deliberately skipped, so an agent that never learned the passphrase can't perform them (`error_kind: interactive_required` when run headless; BREAKING vs ≤0.7.8: `keys decrypt` no longer honors `RWA_PASSPHRASE`). `keys store-passphrase [--wallet N]` verifies the passphrase by decrypting the wallet before saving it to the keychain (nothing is stored on a rejected passphrase); `keys forget-passphrase [--wallet N]` removes it. Both speak `--json` (`{status, action, wallet}`, `forget-passphrase` adds `existed`); `RWA_KEYRING_DISABLE=1` disables the keychain step entirely and makes both commands error loudly (an explicit user request must not silently no-op)
- Sign-time guard: before signing a swap, the CLI simulates the exact Jupiter transaction and confirms the input mint is debited by no more than expected and the expected output mint is credited to this wallet by **at least the quoted amount minus slippage tolerance** (an under-delivering fill is refused, the router excluded, and the quote refetched — surfaced as `route_unfillable` if retries run out); fails closed if the RPC is unreachable. Exception (see above): when the simulation merely ERRORS (`OnChainWouldFail`) on a managed `/execute` route that funds just-in-time, the CLI submits anyway — an errored simulation carries no delta info to check, and the maker's JIT fill + on-chain min-output cover honesty. The delta checks still apply whenever the simulation SUCCEEDS, and the Metis direct-submit path is always strict

## Agent usage rules

- Always prefer `rwa --json`
- Use `-y` only for real execution
- **`--json` without `-y` never executes** (breaking since v0.6.0): the six gated money-moving commands (buy, sell, send, buy-basket, sell-basket, close-all) run with `--json` alone fail closed instead of running non-interactively. An otherwise-valid invocation returns `confirmation_required` (exit 1); a precondition failure surfaces its own kind first (e.g. `insufficient_funds`, `no_position`, `amount_below_minimum`) — the guarantee is "never executes", not "always `confirmation_required`". `close-all` on an empty wallet returns `status:success` (nothing to sell) before the gate. `reclaim` reclaims rent to your own wallet and runs without confirmation. This also gates auto-gas refuel: json auto-approves the refuel only when `-y` is also passed. Add `-y` to execute, or `--dry-run` to preview
- Use `--dry-run` for large or uncertain actions
- Never run wallet-changing commands in parallel
- Treat JSON output as a stable contract for scripts and agents
- Exit codes: 75 (EX_TEMPFAIL) for transient failures worth retrying (`rpc_unavailable`, `execute_unavailable`, `confirmation_timeout`, lock contention); 1 for everything else
- Use `gm tradable <SYM...>` to check one or many symbols
- Use `gm search --tradable-only ...` for bulk scans without Python
- `list`/`search` JSON items carry optional `asset_class` and `region` (Ondo tags) alongside `sector`; sector-less ETFs display `asset class · region`. `--tag <label>` filters across ALL Ondo tag categories (sector, asset class, region, factor/risk — 24 factor labels like Large Cap/Dividend/High Yield); `--search` matches them too
- Use `hours --tradable` only when the user wants the full currently tradable set; `hours` (no flag) already reports `offhours_tradable_count`/`paused_count` — optional, absent on asset-fetch failure
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
