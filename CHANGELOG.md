# Changelog

All notable changes to this project will be documented here.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
Versioning follows [Semantic Versioning](https://semver.org/).

**JSON contract stability:** once a field appears in a release, it is not removed or renamed in a patch release. New optional fields may be added. Breaking JSON changes require a minor version bump and are listed under **Breaking**.

---

## [0.2.28] — 2026-06-26 — Import a key directly with `keys add`

### Added

- **`rwa keys add <NAME> --seed-phrase "..." --path <PATH>`** (and `--private-key <KEY>`) imports a key in one step: it derives the wallet, writes it to `<PATH>` **encrypted by default** (passphrase prompt, or `RWA_PASSPHRASE`; `--allow-plaintext` opts out), and registers it under `<NAME>`. Previously `keys add` could only register an already-existing file. The no-source form (`keys add <NAME> --path <PATH>`) is unchanged. Import refuses to overwrite an existing file at `<PATH>`, and the name is validated (and rejected if taken) before any file is written.

---

## [0.2.27] — 2026-06-26 — Named wallets (path registry)

### Added

- **Multiple named wallets.** A registry at `~/.config/rwa/wallets.toml` (file `0o600`, holds paths only — never key material) maps a name to an absolute key-file path plus an `active` pointer. Key files stay wherever you registered them; their type (plaintext `key.json` vs age-encrypted) is detected by file content, not extension.
- **New `keys` subcommands:** `rwa keys add <NAME> --path <PATH>` (register an existing key file), `rwa keys list` (`--json` returns the stable shape `{"wallets":[{name,path,pubkey,active,encrypted}]}`; `pubkey` is `null` for encrypted wallets), `rwa keys use <NAME>` (set active), `rwa keys remove <NAME>` (unregister — the key file is **not** deleted).
- **Global `--wallet <NAME>` flag and `RWA_WALLET` env** select a wallet for any command. Selection priority: `--wallet` > `RWA_WALLET` > registry `active` > legacy `key.json`/`key.age` default. An absent/empty registry behaves exactly like the previous single-wallet setup; the legacy key is lazily auto-registered as `default` the first time a `keys add/list/use` command runs.
- `keys show` now honors the selection; `keys encrypt`/`decrypt` operate on the legacy default location and reconcile any registry entry that pointed at the renamed file. Selecting an unknown wallet fails closed with a non-transient error (exit 1) listing the available names.

---

## [0.2.19] — 2026-06-05 — Fix all-in cost sign; performance tests

### Fixed

- **`buy`/`sell` `--dry-run` "Est. all-in" now reconciles with the lines above it.** The "Spread/cost" line printed a *favorable* spread as a positive number while "Est. all-in" subtracted it, so the figures didn't visibly add up (a favorable 13.6 bps spread + 10 bps fee printed as `-3.6`, which looked wrong). Both lines now use one signed-cost convention — **positive = costs you, negative = in your favor** — so Spread + Fee = Est. all-in. JSON `slippage_pct` keeps its raw sign; only the human preview changed.

### Added

- **Performance test suite.** Criterion microbenchmarks for hot CPU paths (`cargo bench -p rwa-ondo`: amount formatting, address validation, the sign-time tx parser `decode_and_verify`); deterministic unit tests for `/order` retry classification and exponential backoff; a semaphore-concurrency timing test; and `scripts/bench-latency.sh` for real command latency (p50/p95) and basket sequential-vs-parallel. The full end-to-end retry test (~12 s of real backoff) is `#[ignore]`d — run with `--ignored`.

---

## [0.2.18] — 2026-06-05 — `RWA_EXCLUDE_ROUTERS` escape hatch

### Added

- **`RWA_EXCLUDE_ROUTERS`** — comma-separated list of Jupiter routers (e.g. `jupiterz,dflow`) to avoid when quoting. Complements the automatic route-around-unfillable behavior (v0.2.17) with a manual pin for a router that is persistently bad for you. Applies to `buy`, `sell`, and the auto-retry refetch; merged (deduped) with any routers the retry loop already excluded. Verified with a real round-trip forced onto `metis`.

---

## [0.2.17] — 2026-06-05 — Route around unfillable quotes

### Fixed

- **`gm buy`/`gm sell` could fail when the quoted router couldn't fill.** Jupiter's aggregator sometimes returns an RFQ route (e.g. `jupiterz`) whose market maker lacks inventory at execution time; the pre-sign simulation correctly caught that the transaction would fail on-chain, but the CLI gave up instead of trying another route. Now an unfillable route is excluded (`excludeRouters`) and the quote is refetched, so a fillable router (metis, dflow, …) is chosen automatically. An *unsafe* simulation (would overspend, or the expected output mint isn't credited) remains a hard, non-retried refusal.

### Changed

- New `route_unfillable` error kind: surfaced only if every retry is exhausted; transient by nature, so the CLI retries it for you.

---

## [0.2.16] — 2026-06-05 — Swap simulation guard (fixes all swap/v2 trades)

### Fixed

- **`gm buy`/`gm sell` failed with "swap execution failed" on every trade.** Jupiter migrated all swap/v2 routes (jupiterz, dflow, metis, okx) to settle the swap via CPI *inside* the router program — there is no longer a top-level SPL token transfer. The sign-time verifier required one and so fail-closed on every route. Trading was fully broken.
- **Single `buy`/`sell` errors hid their cause.** The top-level JSON error printed only the outermost wrap (`swap execution failed`) with no `error_kind`. Error rendering is now centralized (`rwa_cli::render_error`): JSON carries the full cause chain plus `error_kind`, and human mode prints the chain.

### Changed

- **New sign-time guard: on-chain simulation of balance deltas.** Before signing, the CLI simulates the exact Jupiter transaction (`sigVerify=false`) and confirms the real effect from pre/post balances — the input mint is debited by **no more than** the expected amount, and the expected output mint is credited to this wallet. This is route-agnostic and a stronger guarantee than the previous static byte-parse. The static verifier is retained for the contradiction checks it can still make (wrong amount/mint, foreign recipient, user-not-signing) and defers to simulation on CPI routes. If the RPC is unreachable the check fails closed (refuses to sign).

---

## [0.2.15] — 2026-06-05 — Metis fallback honors Jupiter API key

### Changed

- The last-resort **Metis V1** quote/swap fallback (`api.jup.ag/swap/v1`) now sends the same Jupiter headers as the primary path: `x-client-platform` always, plus `x-api-key` when `RWA_JUPITER_API_KEY` is set. Previously these two calls were unauthenticated, so a configured key did not raise their rate limits. No behavior change when no key is set (the header is harmless). Closes the last Jupiter call sites that ignored the key.

---

## [0.2.14] — 2026-06-05 — Max-bps cost gate

### Added

- **`--max-bps <N>` on `gm buy`/`gm sell`** — rejects a trade whose quoted all-in cost (spread + Jupiter fee, the "Est. all-in" shown in previews) exceeds N basis points, with `error_kind: "cost_too_high"`. A tunable ceiling tighter than the 3% slippage block. `RWA_MAX_BPS` sets a global default (the flag overrides it). The gate runs in `--dry-run`/`--quote-only` too, so it doubles as an agent pass/fail cost check.

---

## [0.2.13] — 2026-06-05 — Portfolio Jupiter fallback

### Added

- **`gm portfolio` falls back to Jupiter Ultra holdings when Solana RPC is unavailable.** When every Solana endpoint rate-limits/fails, balances are read from `api.jup.ag/ultra/v1/holdings` instead (honoring `RWA_JUPITER_API_KEY`), so the command keeps working with no config. The JSON output includes `"source":"jupiter"` only on fallback (absent on the normal RPC path); human mode prints a one-line note. Swaps continue to use Swap V2; only the holdings read uses Ultra v1.

---

## [0.2.12] — 2026-06-05 — Jupiter api.jup.ag migration + execute resilience

### Fixed

- **Swaps no longer fail when Jupiter throttles the deprecated `lite-api.jup.ag` host.** All Jupiter calls now use `api.jup.ag` (same paths; `lite-api.jup.ag` is deprecated and rate-limited to ~1 req/s). Transient `/execute` failures (HTTP 429/5xx, or a connection error before the request reached Jupiter) are now typed `execute_unavailable` and auto-retried with a fresh order, surfacing a stable `error_kind` (previously these transient failures were untyped and showed only an opaque `swap execution failed`). Ambiguous post-send timeouts are deliberately not retried, to avoid double-submitting a swap.

### Added

- **`RWA_JUPITER_API_KEY`** — sends `x-api-key` to `api.jup.ag` for higher rate limits (the free keyless tier is ~1 req/s; a key raises it). Trade-side analog of `RWA_RPC_URL`.

---

## [0.2.11] — 2026-06-05 — Cost in bps on previews

### Added

- **Dry-run / `--quote-only` previews now show cost in basis points.** `buy` and `sell` previews print the spread in bps, the Jupiter fee in bps, and an estimated all-in cost (`Est. all-in`), so trade cost is readable at a glance. Human output only — the JSON contract is unchanged (it already carries `fee_bps` and `slippage_pct`).

---

## [0.2.10] — 2026-06-05 — Quote-only previews + docs drift guard

### Added

- **`rwa gm buy --quote-only`** — preview a Jupiter quote for any size, skipping only the wallet-balance check, so you can size a trade before funding. Implies dry-run (never executes; `--quote-only -y` is rejected by clap) and still enforces market hours, the 1 USDC minimum, tradability, and the slippage refresh/>3% block. JSON output uses the existing `dry_run` shape.

### Internal

- Extracted a pure, unit-tested `check_buy_funds` from the buy pre-flight.
- New `docs_sync` integration test fails CI when a CLI command is missing from README.md / CLAUDE.md (catches doc drift).

---

## [0.2.9] — 2026-06-04 — Self-update

### Added

- **`rwa update`** — upgrade the binary in place to the latest GitHub Release. Verifies the downloaded archive's SHA-256 against the published `SHA256SUMS.txt` (fail-closed — a mismatch or missing entry aborts without replacing). `--check` reports availability without changing anything; `-y` skips the confirmation prompt; `--json` emits `{status, current, latest, target}` (or `{status:"error", error_kind}`). Error kinds: `checksum_mismatch`, `no_release_asset`, `not_writable`, `network`, `rate_limited`.

---

## [0.2.8] — 2026-06-04 — RPC reliability & agent JSON

### Fixed

- **`gm portfolio` no longer fails on a single transient RPC blip.** Transport errors (connection reset, timeout) during a Solana RPC call were returned immediately without retry, contradicting the retryability classifier. In race mode, one endpoint's transient error plus a rate-limit (HTTP 429) on the other made the whole call fail (`error sending request for url ... all RPC endpoints failed`). Transient network/timeout errors are now retried with exponential backoff like 429/5xx, on both the single and batch RPC paths.
- **Sell-percentage math uses exact integer arithmetic** (`pct_of_u128`) instead of float, avoiding precision drift on `sell <SYM> <PCT>%` and `close-all <PCT>%`.
- Retry transient failures on Ondo HTTP API calls (prices, history, session limits).

### Added

- **`error_kind` in JSON error output** — trade/runtime failures surface a stable machine-readable kind (`market_closed`, `not_tradable`, `slippage_too_high`, `quote_expired`, `swap_rejected`, …) for agents and scripts.
- **`unavailable[]` in `gm portfolio` JSON** — symbols whose market data can't be fetched are skipped from positions and reported separately with a reason, instead of silently distorting totals.
- **Persistent JSONL audit log** for swap operations.
- **`RWA_RPC_URL` hint** is now surfaced in race-mode "all endpoints failed" errors (the sequential path already had it). README and `llms.txt` document the public-node rate-limit reality and the free dedicated-endpoint escape hatch.

### Internal

- Split `solana/rpc.rs` into `rpc/{mod,error,sequential,race}` and `jupiter.rs` into `jupiter/{types,order,execute}`.
- Extracted shared `gm/helpers`, unified `close_all` sequential/parallel paths, and per-item processors in basket flows.
- Replaced silent `unwrap_or(0.0)` / parse fallbacks with `.expect()` on invariant paths so bad upstream data fails loudly.

### Docs

- Fixed README Architecture section drift (module files that became directories: `api/`, `wallet/`, `solana/rpc/`).

### Tests

- Workspace test count: 236 → 250, including 2 regression tests for transient-RPC-error retry (local TCP server that drops the first connection, then serves a valid response).

---

## [0.2.7] — 2026-05-09 — Security hot-fix

### Security

- **Verify Jupiter swap instructions before signing.** `wallet::sign_jupiter_swap` now decodes the base64-encoded transaction returned by Jupiter and refuses to sign unless the on-chain instructions match the user's intent: input mint and amount, output mint, and signer (the wallet pubkey appears in the signer set). Compromised Jupiter API responses or MITM tampering on a custom RPC URL can no longer redirect funds to a third party. The verifier accepts both standard AMM transactions (user is fee payer at index 0) and gasless flows — Jupiter Z (RFQ, market maker pays gas) and Ultra gasless (Jupiter pays gas) — by searching for the wallet pubkey across all signer slots rather than requiring it at index 0. The input-transfer authority check independently confirms the wallet authorized the actual debit. The check tolerates compute-budget and ALT-extend instructions; unknown extras are allowed but at least one SPL Token transfer/transfer_checked from the wallet's input ATA at the expected amount is required.
- **Wallet encryption is now the default.** `rwa keys generate` and `rwa keys import` write `key.age` (passphrase-encrypted) by default. Pass `--allow-plaintext` to opt out (with a stderr warning). Plaintext `key.json` files remain readable for backward compatibility, but `rwa keys show` now prints a deprecation warning when it detects one — encrypt with `rwa keys encrypt`.
- **Minimum passphrase length enforced.** `prompt_new_passphrase` rejects passphrases shorter than 12 characters and rejects digits-only passphrases (low-entropy scrypt bypass). Existing encrypted wallets are unaffected.
- **`RWA_PASSPHRASE` env warning.** When the passphrase is read from the `RWA_PASSPHRASE` environment variable, a one-time stderr warning is printed about leakage via shell history, `ps -E`, and core dumps. Prefer interactive prompt or a file-based mechanism for production setups.

### Internal

- New `crates/ondo/src/wallet/verify.rs` module with `ExpectedSwap`, `decode_and_verify`, and `VerifyError`. Tolerant V0 + legacy message parser; positive ATA verification for both Token and Token-2022 programs.
- `wallet.rs` reorganized as `wallet/` directory module to host the `verify` submodule.
- `jupiter::execute_order`, `execute_managed_order`, and `execute_metis_order` take `&ExpectedSwap` and route through the new `sign_jupiter_swap` wrapper. The generic `wallet::sign_transaction` is unchanged and continues to back the `transfer_sol`/`transfer_spl` paths that don't go through Jupiter.
- 11 new tests in `wallet::verify::tests` (parser-level reject scenarios + happy path) and 2 integration tests in `wallet::tests` (`sign_jupiter_swap_signs_when_intent_matches`, `sign_jupiter_swap_refuses_amount_mismatch`).

### Tests

- Workspace test count: 234 → 236 (190 in v0.2.6 → 236 here, +46 across security work). All paths through `execute_with_retry` (single buy/sell, basket buy/sell, close-all sequential/parallel) are covered by the verifier.

### Notes for users

- If you have automation that relied on `rwa keys generate` writing plaintext `key.json` without flags, add `--allow-plaintext` to keep that behaviour, or migrate to encrypted keys (`--encrypt` is now the default).
- The deprecated `--encrypt` flag still works but is hidden from `--help` and prints a deprecation note.

---

## [0.2.6] — 2026-04-16

### Performance
- `rwa gm portfolio` now returns in ~1.2–1.7 s typical (previously 21–26 s). Read-only Solana RPC calls race across all configured endpoints in parallel instead of trying one at a time; the first successful response wins and the loser is aborted mid-flight. Writes (`sendTransaction`) still use the sequential strategy to avoid double-submission.
- Side benefit: `rwa gm balance` and other read-heavy commands are faster under the same change.

### Internal
- New `RpcMode::{Sequential, Race}` enum on the RPC layer — every call-site explicitly picks a mode (compile-time safety against accidentally racing a write).
- Per-URL timeout in race mode is 8 s (was 20 s in sequential mode), since a slow node is almost always beaten by a fast peer.

## [0.2.0] — 2026-03-29

### Breaking

- **Portfolio JSON restructured.** `sol`, `usdc`, `total_value_usd`, `change_24h_usd`, `change_24h_pct` are no longer top-level. New shape:
  ```json
  {
    "wallet": "...",
    "cash": { "sol": 0.087, "usdc": 0.00 },
    "gm_positions": {
      "positions": [...],
      "value_usd": 0.00,
      "change_24h_usd": 0.00,
      "change_24h_pct": 0.00
    }
  }
  ```
- **`rwa gm quote` removed.** Use `rwa gm buy <SYM> <AMT> --dry-run` instead — it validates balance and tradability in addition to returning the full quote (`slippage_pct`, `price_impact_pct`, `fee_bps`, `gasless`, `router`).
- **`alloc_pct` renamed to `gm_alloc_pct`** in position objects — reflects that allocation is within GM positions only, not total portfolio.

### Added

- `--dry-run` flag on `buy`, `sell`, `send`, `close-all` — validates and shows quote without executing.
- `buy/sell --dry-run` JSON now includes `price_impact_pct` and `fee_bps` (previously only in `quote`).
- Wallet encryption via [age](https://age-encryption.org/):
  - `rwa keys generate --encrypt` — create encrypted wallet (`key.age`)
  - `rwa keys encrypt` / `decrypt` — convert between `key.json` ↔ `key.age`
  - `RWA_PASSPHRASE` env var for scripted/agent access
- `zeroize` on all in-memory secret key material.
- Integration tests with `httpmock` for portfolio RPC parsing (standard, out-of-order batch, malformed entries, empty, error propagation, sort order).
- Property-based tests (`proptest`) for `token_to_raw` / `format_amount` roundtrips.
- Typed error kinds surfaced in JSON: `market_closed`, `not_tradable`, `slippage_too_high`, `confirmation_timeout`, `on_chain_failure`.
- `amounts` module — centralized raw↔display amount parsing and formatting.
- `usecases::gm` module — trade logic extracted from CLI layer (prepare/execute split).
- CI job split: `check`, `unit tests (ondo)`, `unit tests (cli)`, `integration tests`, `release build`.

### Fixed

- RPC batch responses now sorted by `id` before processing — handles out-of-order delivery per JSON-RPC 2.0 spec.
- Custom `--rpc-url` no longer appends public fallback URLs (prevents silent failover to wrong endpoint).
- Removed extrnode (401) and drpc (400) from default RPC fallback list — only `mainnet-beta.solana.com` and `publicnode` remain.
- SOL `send all` now works on raw lamports — no float precision loss on exact drain.
- Portfolio crash on RPC nodes returning errors in batch response.
- Malformed batch entries silently skipped instead of crashing.

### Changed

- Workspace edition: `2021` → `2024`.
- Version: `0.1.0` → `0.2.0`.

---

## [0.1.0] — 2025-12-01

Initial release.

- `rwa gm buy / sell / close-all / portfolio / history / list / send / reclaim / hours`
- `rwa keys generate / import / show`
- Jupiter Ultra gasless swaps, RFQ routing
- Token-2022 support for Ondo GM tokens
- SLIP-10 mnemonic derivation (m/44'/501'/0'/0')
- Solana RPC with retry and URL rotation
