# Changelog

All notable changes to this project will be documented here.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
Versioning follows [Semantic Versioning](https://semver.org/).

**JSON contract stability:** once a field appears in a release, it is not removed or renamed in a patch release. New optional fields may be added. Breaking JSON changes require a minor version bump and are listed under **Breaking**.

---

## [0.7.10] - 2026-07-19 — buy-basket bulk input (`--equal`, `--from-file`/stdin)

### Added

- **`buy-basket --equal` and `--from-file`** — `--equal` (with `--total`) splits the total evenly across bare symbols (the only clean equal-weight for an arbitrary N under the exact-100 `--total` rule; floor + dust-to-first, same 5 USDC/item minimum). `--from-file <path>` reads tokens from a file or stdin (`-`; whitespace/newline separated, `#` comment and blank lines ignored), mutually exclusive with positional args. Together they enable filter-driven bulk buys by composition: `search --tradable-only --sector X | jq -r '.items[].symbol' | xargs buy-basket --total N --equal`. No new error kinds (reuses `invalid_amount`/`amount_below_minimum`, both enforced pre-network); `allocation` echo carries the computed equal weights. Additive only — no breaking changes.

---

## [0.7.9] - 2026-07-19 — agent-safe wallet (keychain + send policy), audit remediation, Metis slippage fix

### Added

- **OS-keychain passphrase storage** — `keys store-passphrase` / `keys forget-passphrase`; operational commands resolve the passphrase `RWA_PASSPHRASE` → OS keychain → prompt, so a desktop agent trades headless without the secret ever entering env or transcript. `RWA_KEYRING_DISABLE=1` skips the keychain step.
- **Send policy (allowed recipients)** — `keys policy show/allow/remove` stores a whitelist INSIDE the age-encrypted wallet payload (edits are admin-class: typed passphrase only). Opt-in: once the list is non-empty, any non-interactive `send` (`-y`/`--json`/no TTY) to an unlisted address fails pre-network with `error_kind: recipient_not_allowed` and no prompt (safe by design, not a bug); a live TTY send requires typing the address's last 6 characters AND then the wallet passphrase, verified by re-decrypting the wallet file (env/keychain not consulted). Empty/absent policy = behavior unchanged. Plaintext wallets can't carry a policy (encrypt first).

### Breaking

- **`keys decrypt` and `keys export --reveal` are now admin-class**: the passphrase must be typed at a live terminal; `RWA_PASSPHRASE` and the keychain are deliberately not consulted (headless → `error_kind: interactive_required`, exit 1). Closes the "injected agent strips encryption / exfiltrates the key with a leaked env passphrase" hole. NOTE: shipped in this 0.7.9 patch release as a deliberate maintainer decision despite the breaking nature — pin exactly if you script `keys decrypt` with `RWA_PASSPHRASE` or branch on the affected exit codes.

### Fixed

- **Metis-fallback swaps no longer record a phantom trade.** A reverted transaction was previously scored as a success by the confirmation poll; a confirmation-poll timeout was also reported as success. Both paths now surface a typed failure and skip the ledger write instead of recording a trade that either didn't happen or isn't yet known to have happened. (M1)
- **Transient `/execute` failures — including `route_unfillable` — now exit 75, not 1.** `is_transient_kind` now derives straight from `ExecuteFailureKind::retry_action()` instead of a hand-picked list: three kinds missed by the original list (`missing_cached_order`, `swap_rejected`, `route_unfillable`) move to transient alongside `failed_to_land`/`rfq_failed_to_land`/`internal_error`/`quote_expired`, consistent with `execute_unavailable`'s existing treatment. (M2)
- **`buy-basket`/`sell-basket`/`close-all` now report `status: "partial"` (some items failed) or `"error"` (all items failed) with a non-zero exit code**, instead of `status: "success"` regardless of per-item failures — a caller polling only the top-level exit code could previously miss a fully- or partially-failed basket. The wallet key is now zeroized before the direct `process::exit` on the all-items-failed path (it previously exited before the drop that would have zeroized it). (M3)
- **Off-list-send second factor is now pinned by a unit test.** The suffix+passphrase gate (`off_list_proceed`) has an injectable decision function so a future refactor that weakens it to suffix-only (or drops the passphrase factor) breaks a test instead of surfacing only in a live audit. No behavior change. (M4)
- **Ledger head-truncation is now detected.** A tampered/truncated ledger file whose first surviving line's `prev` no longer chains to the genesis sentinel is now caught as `ledger_integrity: "broken@line N"`. The remaining undetectable case — dropping only the very LAST entry, which no later line references — is a documented known limit, not a gap this change closes. (L1)
- **`sign_transaction`'s signer-slot lookup is now bounds-checked** against the (untrusted) transaction header instead of indexing directly — a malformed/adversarial transaction previously risked panicking the process instead of failing with a typed error. (L2)
- **The BIP39 recovery mnemonic is now zeroized end-to-end.** It was the one wallet secret still carried as a bare `String` (`DecryptedWallet.mnemonic`, the age-payload struct, `generate_with_mnemonic`'s return, `keys.rs`'s imported-phrase path) — now `Zeroizing<String>` at every hop; the two discard-only passphrase-verification sites (the off-list-send re-check, `store-passphrase`) also route through the zeroizing payload loader instead of the `String`-materializing public boundary. (L3)
- **`keys policy remove` now warns when it empties the allowed-recipient list.** Draining it (e.g. rotating out a retired address) silently re-disables the send gate; `--json` adds `policy_now_empty: true` and human mode prints a stderr warning on the remove that reaches zero — a remove that leaves entries behind, or any `allow`, never sets it. (L4)
- **`buy TSLA 0` / `--limit-price 0` now type as `invalid_amount`** instead of `error_kind: null` — the all-zero-trims-to-empty branch in amount parsing used a bare untyped error; every other bad-amount case (negative, non-numeric, over-precision) was already typed. (L7)
- **Metis-fallback quote slippage was read 100x too small.** `calc_slippage` returned `order.price_impact` verbatim, but that field is a decimal *fraction* (`-0.04` = -4%) while every consumer treats the result as a percent — so on the Metis fallback path (thin RFQ tokens, e.g. PFEon/LLYon) a real -4% impact read as -0.04%, defeating the 3% slippage block, the 1% retry, and `--max-bps`. The fraction is now converted to a percent at the single choke point. The primary swap/v2 path (which uses the USD-value branch) was unaffected. (QM-1)
- **`--max-bps` and the 3% slippage guard no longer fail silently when a quote omits cost data.** `--max-bps` now fails *closed* (`cost_too_high`) when an explicit ceiling is set and the quote reports neither slippage nor fee; the plain 3% guard still fails *open* on an unmeasurable quote (honest thin-token quotes may lack metrics) but now warns on stderr instead of passing silently. (QM-2)
- **`price_impact_pct` in `buy`/`sell` JSON now reports a percent (was a decimal fraction),** consistent with the sibling `slippage_pct` field — a Metis quote that shows `slippage_pct: -2.0` no longer also shows `price_impact_pct: -0.02` for the same impact. Display-only; the internal price-impact value and `calc_slippage` are unchanged. (QM-1)

### Changed

- **Agent-contract impact.** `route_unfillable` moves from exit 1 to exit 75 (transient, see M2 above). `buy-basket`/`sell-basket`/`close-all` gain a new `status: "partial"` value (some but not all items failed) alongside the existing `"success"`/`"error"`, and an all-items-failed basket now exits non-zero instead of 0 (M3). All 14 `ExecuteFailureKind` labels (Jupiter `/execute` failures, alongside the existing `GmTradeErrorKind` labels) are now documented in CLAUDE.md/llms.txt/README.md and pinned there by the `docs_sync::error_kinds_are_documented` CI guard (L6).

### Security

- **install.sh CRITICAL fail-open checksum bug, already shipped to `main` ahead of this branch.** `verify_checksum` ran as a bare statement inside `install_prebuilt`, itself called via `if ! install_prebuilt` — `set -e` is suppressed for that whole call tree, so a failing `sha256sum -c` printed `FAILED` but never aborted the install, letting a tampered/corrupt binary install anyway. `verify_checksum` now returns 1 on mismatch and the caller `exit 1`s explicitly; a hermetic fail-closed test (`scripts/test-install.sh`) plus a CI `install-script` job now guard the regression. The same hotfix also tightened the source-fallback path: it resolves and builds the actual latest release tag (`--tag`) instead of floating `main` HEAD, and the release-tag redirect is matched against the exact `github.com/<repo>/releases/tag/` prefix rather than a loose substring.

### Known limitations (documented, not fixed on this branch)

- **L5** — the pre-sign input-debit check reads two RPC values that can land on different slots; a concurrent same-ATA debit could theoretically under-report the debit between reads. A slot-equality guard was implemented and reverted (multi-provider RPC plus slower `simulateTransaction` make honest slot drift common, so it would refuse legitimate swaps); the correct fix (a post-sim ATA re-read) costs an extra RPC round-trip on every swap and is deferred rather than paid universally for this attacker-untriggerable, min-output-bounded race.

## [0.7.8] - 2026-07-16 — buy-basket --total weighted allocation

### Added

- **`buy-basket --total <USDC>`** — weighted allocation: split one USDC total across percent-weight pairs (`TSLA 50% NVDA 30% SPY 20% --total 1000`). Weights must sum to exactly 100; each item floors to raw USDC (dust to the largest weight, so the spent sum equals the total exactly) and must clear the 5 USDC per-item minimum. All validation is local and typed (`invalid_amount` / `amount_below_minimum`) and runs before any wallet or network access. JSON adds an optional `allocation: {total, weights}` echo on `buy-basket` results.

## [0.7.7] - 2026-07-12 — supply-chain hardening, keys/paused wording fixes

### Fixed

- **`keys encrypt` now warns that the recovery phrase is not preserved.** It encrypts the plaintext `key.json`, which never stored the mnemonic, so `keys decrypt` → `keys encrypt` silently drops it (the wallet stays recoverable from the private key, just not from a phrase). Only `keys generate` and seed import embed the phrase.
- **Paused tokens are no longer labeled as a "dividend event."** Ondo sets `is_trading_paused` for two reasons — a dividend window OR the market being closed for the asset (on weekends ~all non-24/7 tokens are flagged; live: 121 paused on a Friday afternoon → 433 of 438 on the weekend). The `⏸` legend and the `trading_paused` message now name both causes and point at `gm hours` for when trading resumes; docs note that the paused check runs before the session check, so a weekend non-flagship surfaces `trading_paused` rather than `market_closed`.

### Security / supply chain

- **All GitHub Actions pinned to full commit SHAs** (tags kept as comments; dependabot maintained) across every workflow. A compromised mutable action tag can no longer inject code into the release job that signs and publishes the binary.
- **Build-provenance attestation** on published release archives (`actions/attest-build-provenance`, OIDC/Sigstore-backed) — verifiable authenticity beyond the same-origin `SHA256SUMS.txt`, via `gh attestation verify <asset> --repo outputlayer/rwa_cli`.
- **`cargo build --release --locked`** in the release workflow — a published binary is always built against the committed `Cargo.lock`.

### Docs

- Added `RELEASING.md` (the release pipeline, previously undocumented) and a CLAUDE.md "what CI cannot verify" note naming the live-only on-chain money paths (`gm_execute`, `jupiter/execute` submit, `transfer`, `gm_gas`) and the expectation of a live dry-run + small real trade before merging changes to them.

## [0.7.6] - 2026-07-11 — money-safety hardening, contract guards, dependency patch

### Fixed

- **Security (defense-in-depth): the pre-sign under-delivery floor is now clamped to the user's requested slippage.** It previously took its tolerance from `order.slippage_bps`, which Jupiter echoes in the `/order` response — a hostile or dynamically-widened response reporting `slippageBps: 10000` would set the floor to 0 and disable the independent under-delivery check (strongest on the Metis direct-submit path, which has no Jupiter co-sign). The floor now uses the *tighter* of the response value and the locally-known `--slippage`/default, so it can never be looser than the user consented to. No behavior change on honest quotes (Jupiter echoes the requested bps).
- `gm hours --tradable` lists the flagship symbols in **human** mode too (it previously populated the list only in `--json`; the human output showed just a count, making the flag look like a no-op).
- A panicked parallel basket/close-all task now lands in `failed[]` instead of vanishing from the `--json` report (a silent drop read as full success while the money op may have executed).
- **`keys encrypt --json` / `keys decrypt --json` now emit JSON on success** (they printed human prose despite advertising `--json`; the error paths already emitted JSON, which hid the success-path leak from tests). Added a contract test covering the success path.
- **`keys decrypt` honours `RWA_PASSPHRASE`** — it used a TTY-only prompt and ignored the env var, so it was the one encrypted-wallet operation that couldn't be scripted (contradicting the documented `RWA_PASSPHRASE` scripted-access support).
- **Dividend-paused tokens are no longer mislabeled in human `list`/`search`/`tradable`.** A paused token rendered with the same `✗` as an out-of-session token under a legend reading "not in this session" — factually wrong (it IS in session, just halted). Paused tokens now show `⏸` with a "paused for a dividend event" legend, distinct from `✗`.
- **`gm search --tag <label>` now appears in the human results header** (`(tag=…)`), like every other filter — it was silently dropped from the header while still filtering correctly.
- `gm hours --tradable` help text corrected: it lists the 24/7 flagship set (human) / the full session-tradable array (`--json`); the full human list is `rwa gm tradable`.
- Docs: the `--json`-without-`-y` contract is clarified — it fails closed ("never executes"), but an otherwise-valid trade yields `confirmation_required` while a precondition failure surfaces its own kind first (and `close-all` on an empty wallet returns success). Agents should not assume `confirmation_required` is universal.

### Changed

- **Dependencies: patched every RUSTSEC advisory** the new audit job flagged (crossbeam-epoch, quinn-proto, rustls-webpki, rand bumped to patched releases; anyhow dropped from the tree). None were reachable in this CLI's usage, but a wallet binary shouldn't ship flagged crates. The two residual advisories are unmaintained build/dev-only crates (async-std ← httpmock dev-dep, proc-macro-error ← age's build-time proc-macro) with no runtime presence.

### Internal

- **New CI drift guards** (the class of bug where behavior silently diverges from docs): every `GmTradeErrorKind` label must be documented in llms.txt/README/CLAUDE.md (`error_kinds_are_documented`, source list kept exhaustive by a compile-time tripwire); a human-mode contract test for `gm hours --tradable` (the suite previously locked only `--json` shapes). Wired the `docs_sync` suite into `make ci` and CI — it had been running in neither.
- **New `Security Audit` CI job** — `cargo audit --deny warnings` (RUSTSEC advisory scan for the wallet binary), deliberately separate from `make ci` (it needs the advisory DB and can go red on a newly-published advisory with no code change).
- Fixed a wall-clock-dependent flake in the `buy_basket_dry_run_enforces_max_bps` contract test (a 500 `/session` fixture failed closed with `market_closed` during a real "Closed" session instead of reaching the cost gate); it now uses a deterministic all-sessions-tradable fixture.
- Bug-hunt provenance: the keys `--json`/passphrase, human paused-label, `--tag` header, and hours help-text fixes above were found by an exhaustive flag×mode probe of the running binary — the same "behavior diverges from docs, invisible to JSON-only tests" class as the `hours --tradable` bug; a human-mode contract test now guards it.

## [0.7.5] - 2026-07-10 — human-UX papercut batch

### Changed

- **Typos get a suggestion**: `Unknown GM token: APPL. Did you mean AAPLon?` — nearest symbol by edit distance ≤ 2 (own tiny Levenshtein, no new dependency); far-off garbage gets no guess, just the `gm search` hint.
- **Human-mode errors are clean**: a typed error renders only its actionable detail (`Minimum buy amount is 5 USDC`) instead of the machine-flavored `wrap-chain: GM trade error [snake_case_kind]: …` read. The JSON contract is untouched (full chain + `error_kind` as before).
- `history <BADSYMBOL>` resolves the symbol before calling the API — typed `unknown_token` (with the suggestion) instead of a raw upstream HTTP 400 dump.
- `buy --help` documents the 5 USDC minimum; `sell --help` warns that amounts are RAW tokens (wallets display raw × shares_per_token — prefer `all`/`NN%`).
- `gm list`: correct pluralization, a ✓/✗ legend in the header, wider name column, and the unfiltered full dump ends with a filter tip.
- A passphrase prompt with no terminal now teaches the fix (set `RWA_PASSPHRASE`, run interactively, or `--allow-plaintext`) instead of `os error 6`.
- `--limit-price` accepts plural units (`shares`/`tokens`); genuinely unknown units still get the teaching error.

## [0.7.4] - 2026-07-10 — internal refactor (no behavior change)

### Internal

- **Option bundles on the money commands.** `buy`/`sell`/`send`/`buy-basket`/`sell-basket`/`close-all` take `ExecOpts {yes, dry_run, json}` + `TradeTuning {slippage, max_bps}` instead of 3 adjacent positional bools and 2 adjacent `Option<u32>`s — a transposed argument can no longer compile silently on a money path. `#[allow(too_many_arguments)]` count: 21 → 15.
- **Three duplications collapsed to single sources**: `NN%` validation (was implemented 3× with drifting error typing → one `amounts::parse_pct`, uniformly typed `invalid_amount`); the 4 copies of the 17-field `TradeJson` literal (→ one `trade_json()` constructor documenting the buy/sell result-side asymmetry at a single site); the identical ~45-line tail of the three `send_*` variants (→ one `send_epilogue()` — "record to the ledger only AFTER a successful transfer" can no longer drift between senders).
- JSON shapes verified byte-identical; net −65 lines.

## [0.7.3] - 2026-07-10 — real-path adaptive pacing (~3× faster live baskets), API-key auto-profile

### Performance

- **Real `-y` baskets and close-all now pace their launches like the dry-run quote fetcher.** The staggered, adaptive launcher (v0.7.0/0.7.1) only covered dry-run quoting — the real execution path spawned every fetch+execute chain at once, bursting Jupiter's per-wallet limit exactly the way the stagger was built to avoid (the source of ~22 s real 10-token baskets while their dry-run quoted in ~4 s). Live-measured after the fix (keyless wallet): **buy-basket 5 tokens = 3.4 s, sell-basket 5 = 2.8 s, zero retries**; when Jupiter did throttle one item mid-close-all, the adaptive widen degraded gracefully to a serial pace instead of a retry storm.
- **`RWA_JUPITER_API_KEY` auto-selects the keyed performance profile**: quote stagger 0 ms (the key raises per-wallet limits, so the burst-dodging stagger only costs latency) and `/order` concurrency 4 (vs the live-tested keyless ceiling of 2). No manual tuning needed anymore; an explicit `RWA_QUOTE_STAGGER_MS` still overrides.

## [0.7.2] - 2026-07-10 — typed input errors, informed consent prompt, keys --json

### Fixed

- **`send USDC all` no longer lets auto-gas silently shrink the send.** For balance-relative amounts (`all`/`100%`/`NN%`) the gas-reservation parse failed and reserved 0, so on a low-SOL wallet the pre-send refuel could divert 5–25 USDC to SOL and the recipient received *balance − gas* with no warning. Balance-relative USDC sends now skip the refuel entirely (an exact amount still reserves itself and refuels from the remainder).
- **`keys show --json` emits JSON.** The flag was advertised in `--help` but ignored — it printed the human text. Now returns `{pubkey, path, encrypted}`.
- **Lock contention exits 75 in human mode too.** `--json` already exited 75; the human path returned an untyped error and exited 1, contradicting the documented contract. The error is now typed `lock_contention` (transient) in both modes.
- Exact-amount `send USDC` checks the balance locally and fails fast with typed `insufficient_funds` instead of an opaque on-chain reject; both SOL-for-gas checks in `send` are typed `insufficient_funds` as well.
- Basket/close-all no longer contain `.expect()` calls that could panic *after* a swap landed on-chain (killing the parallel set and losing the JSON report with tx URLs); the display-total parses degrade to 0 instead.
- `sell` of a token you don't hold names the symbol (`You hold no GOOGLon — nothing to sell`) instead of the raw mint address.

### Added

- **Typed `error_kind` for common input failures** (previously `null`, forcing agents to match English prose): `unknown_token` (message points at `gm search`), `invalid_amount` (message teaches the format: number, `NN%`, or `all`), `invalid_address`, `unknown_wallet`, `self_send`, `reveal_required`, and `lock_contention` (the one transient among them).
- **The real-trade y/N prompt shows the dry-run economics** — implied `Price: ~410 USDC/token` (new line, also in dry-run), spread, fee, all-in estimate, slippage tolerance, per-share view. Previously the moment of highest stakes showed only the receive amount. The auto-gas consent prompt now honestly says "5–25 USDC (sized to current fees)" instead of promising the 5 USDC floor.
- **`keys generate`/`keys import` speak `--json`**: `{status, pubkey, path, encrypted, mnemonic}` (mnemonic on generate only — machine-readable at the only moment it exists for plaintext wallets). `keys add --json` now carries `pubkey` and `encrypted` too.
- **`hours` JSON carries `next_session_at`** (epoch seconds, always present) so agents schedule against a number instead of parsing `"opens in 22h 0m"` prose.

### Removed

- **The no-op `--parallel` flag** (buy-basket, sell-basket, close-all). Parallel has been the default since v0.5; the flag was accepted and ignored, which both lied to the user ("I enabled something") and kept leaking back into docs and agent examples. Passing it now fails with a clap usage error (exit 2) — drop the flag; use `--sequential` only as the rate-limit fallback.

### Changed

- llms.txt (the agent manual): freed operational guidance trapped inside a bash code fence; examples no longer teach the no-op `--parallel` (parallel is the default; `--sequential` is the fallback); documented exit 2 (clap usage error — no JSON envelope), the number policy (exact amounts are decimal strings, prices are rounded floats), and that basket/close-all exit 0 on partial failure — always inspect `failed[]`/`skipped[]`.

### Internal

- `make ci` — local mirror of the GitHub CI pipeline (every step under `RUSTFLAGS=-Dwarnings`), plus an optional pre-push hook (`make install-hooks`).
- Adaptive-pacing launcher takes an injected retry-count reader (process-global removed from the tested path; the stagger test regained its deterministic upper bound).
- Real coverage run: 77.6% lines (cargo-llvm-cov); the sub-60% files are live-execution paths validated by live-money runs. Unit-covered the sector→asset-class·region display fallback, `describe_filters`, and `compute_pnl`'s SOL-buy guard.

## [0.7.1] - 2026-07-10 — adaptive parallel-basket pacing (keyless-friendly)

### Performance

- **Parallel basket/close-all quotes now adapt to Jupiter's per-wallet rate limit** instead of only staggering. Firing quotes with a fixed stagger is fast when the endpoint is generous but, once Jupiter starts throttling the wallet, the retry backoff (0.8/1.6/3.2 s) piles up and the parallel path can end up *slower* than `--sequential`. The launcher now watches a process-wide `/order` retry counter: while it stays flat, launches keep the small stagger (`RWA_QUOTE_STAGGER_MS`, default 350 ms); the moment retries climb past the baseline, remaining launches widen to the serial `SEQUENTIAL_SPACING` (3 s) so the CLI stops feeding the burst. This self-tunes to whatever limit a **keyless** wallet has — fast when possible, graceful serial degradation when throttled — so a public user gets good parallel throughput without needing `RWA_JUPITER_API_KEY` (which still raises the ceiling for full parallel).

## [0.7.0] - 2026-07-10 — just-in-time RFQ fills, non-default account import, faster parallel baskets

### Fixed

- **Thin RFQ-only tokens (PFEon, LLYon, …) are buyable again, and liquid tokens fill via the tighter RFQ route.** The sign-time simulation guard ran *before* Jupiter `/execute`, against current chain state — but RFQ market makers fund the fill **just-in-time** at execution (they mint/position inventory in the same block, confirmed on-chain). Their inventory is empty at simulation time, so the sim ERRORED and the CLI refused a swap that would actually land — a false negative that made these tokens look unbuyable and needlessly rerouted liquid ones to the Metis AMM (worse price). Now, on a managed `/execute` route that funds just-in-time (detected via `gasless`), a simulation that merely ERRORED (`OnChainWouldFail`) is submitted anyway — Jupiter validates server-side, the maker co-signs, and the on-chain min-output (`otherAmountThreshold`) enforces honesty (surfaced as a stderr `note:`). **Safety is unchanged**: a simulation that SUCCEEDED but showed dishonest deltas (`UnsafeDelta`/`OutputBelowQuote`), one that couldn't run (`RpcUnavailable`), a non-JIT route, and the entire Metis direct-submit path are all still hard-refused. Verified live end-to-end across `buy`/`sell`/`buy-basket`/`sell-basket`/`close-all`.

### Added

- **Import a seed phrase at a non-default account.** `keys import` / `keys add --seed-phrase` accept `--account <N>` (→ `m/44'/501'/N'/0'`, Phantom/Solflare "Account N+1") or the mutually-exclusive `--derivation-path <PATH>` (full BIP44, matching `jup-ag/cli`). Default is unchanged (account 0); the flags apply only to seed import. The derived address is echoed and, on a non-default path, a stderr note reminds you the recovery phrase alone restores account 0 — record the path.
- **`RWA_DEBUG=1`** restores verbose diagnostics that are otherwise condensed: the full pre-sign simulation program-log (default: a one-line cause) and the per-backend `/order` quote-failure list.

### Performance

- **Parallel basket quotes are staggered** (`RWA_QUOTE_STAGGER_MS`, default 350 ms between launches). Firing a basket's quotes all at once bursts Jupiter's per-wallet rate limit, whose 429 → retry backoff (0.8/1.6/3.2 s) made the parallel *median* slower than sequential. Staggered, a 3-token basket quotes in **~1.3 s** on a healthy wallet (vs ~7.4 s `--sequential`); best-case latency is `~0.6 s + (N−1) × stagger`. Without an API key, Jupiter's per-wallet limit still bounds throughput and heavy use degrades to multi-second retries — set **`RWA_JUPITER_API_KEY`** for the real parallel unlock, then lower the stagger toward 0. Measured: single quote ~0.6 s, reads 30–200 ms.

### Changed

- **Terminal `route_unfillable` output is concise.** The pre-sign simulation failure and the multi-backend "No swap route found" list no longer dump the full program-log / per-backend `{requestId}` payloads by default (one-line cause; full detail under `RWA_DEBUG=1`). `error_kind` is unchanged.

### Internal

- Added an entropy invariant test pinning full-CSPRNG seed generation (guards against the weak-RNG wallet-drain class), and real unit coverage for the new sign-gate decision (submit/refuse across every `SwapSimError`, including the never-submit-a-dishonest-sim invariant), derivation-path parsing, and the `--account` end-to-end contract against independent reference vectors.

## [0.6.1] - 2026-07-06

### Added
- `gm hours` surfaces the full availability picture: optional `paused_count`, `offhours_tradable_count`, and (with `--tradable`) the `offhours_tradable` flagship list in JSON; human output gains a `24/7 (offhours): N flagship tokens; paused now: M` line. 24/7 trading applies to flagship tokens only (6 of 440 today).

## [0.6.0] — 2026-07-06 — confirmation_required, ledger integrity, install/quote hardening

### Breaking

- **`--json` without `-y` no longer executes.** Every money-moving command (`buy`, `sell`, `send`, `buy-basket`, `sell-basket`, `close-all`) run with `--json` alone now fails closed with the typed `confirmation_required` (exit 1, not transient) instead of running non-interactively. Previously `--json` was treated as auto-consent on its own; now `-y` is required either way. `--dry-run` is unaffected and still previews without executing. The same gate covers auto-gas refuel: with `--json` and no `-y`, the refuel is skipped silently (never auto-approved, never prompted) rather than running ahead of the main operation's own consent check.

### Added

- **Tamper-evident trade ledger.** Each ledger line now carries a `prev` hash-chain link (hex of the first 16 bytes of SHA-256 over the raw previous line; the first entry chains to the hash of empty bytes). `gm pnl` JSON always carries `ledger_integrity`: `"ok"` (chain intact), `"legacy"` (unbroken but predates chaining), or `"broken@line N"` (first line whose link doesn't match). A broken chain warns on stderr (human mode only; in `--json` the signal is the `ledger_integrity` field itself) but never fails `pnl` — P&L still computes from what's readable. In-process ledger appends (parallel basket/close-all legs) are now serialized so concurrent writers can't both chain to the same predecessor and produce a false `broken` verdict.
- **Bounded quote-fetch retries with visible progress.** `/order` backoff is capped at 3.2s per retry (800ms · 2^attempt), and the whole multi-backend quote-fetch pass gets a ~20s retry budget (a fresh budget per outer retry, e.g. slippage refresh or route-unfillable requote) instead of retrying unboundedly. A `still fetching quote (...)` heartbeat prints to stderr on every retry so a slow quote stays visible instead of hanging silently.
- **`install.sh` fails closed on any unverifiable download** — missing `SHA256SUMS.txt` manifest, no checksum entry for the archive, or no `sha256sum`/`shasum` available all now abort the install with exit 1 instead of installing an unverified binary. `RWA_INSTALL_INSECURE=1` is an explicit, unrecommended bypass.
- **Passphrases held in `Zeroizing<String>`** end-to-end (wallet encrypt/decrypt, `keys` commands) instead of plain `String`, so a passphrase is zeroed on drop rather than lingering in freed memory.

### Internal

- Split oversized modules: `crates/ondo/src/gm.rs` → `symbol_resolve.rs`; `solana::balance` → `solana::mint`; `usecases::gm` → `usecases::gm_limit`. `verify_parsed` (wallet sign-time guard) and `get_metis_order` (Jupiter quote flow) decomposed into named steps — no logic changes, same evaluation order and short-circuit behavior.

---

## [0.3.2] — 2026-07-02 — Trade ledger + P&L, full tag taxonomy in search

### Added

- **Per-wallet trade ledger.** Every CLI money operation (buys/sells with both raw legs, gas refuels, transfers, reclaims) is appended to `~/.config/rwa/ledger/<pubkey>.jsonl` — one file per wallet, raw on-chain units. Single `buy`/`sell` previously bypassed even the audit log (only basket paths recorded); both now record everywhere.
- **`rwa gm pnl`** — average entry price, invested basis, unrealized (vs live market price) and realized P&L per token, plus wallet totals. Built **from your own buys/sells only** (deposits/withdrawals are cash movements and are ignored by design); sells beyond CLI-recorded buys are flagged as acquired-elsewhere and excluded rather than corrupting the averages. Stable `PnlJson` shape.
- **Full Ondo tag taxonomy in `list`/`search`.** 81 tokens (almost every ETF) previously listed with no classification because only 2 of Ondo's 5 tag categories were read. Sector-less tokens now display `asset class · region` (Equities · Asia, Fixed Income · US, Crypto-Native Assets · Global); JSON items carry additive optional `asset_class`/`region`; new `--tag <label>` filters across all categories including 24 factor/risk labels (`--tag asia`, `--tag dividend`, `--tag "fixed income"`), and `--search` matches the full tag set too.

---

## [0.3.1] — 2026-07-02 — USDC-only wallets, auto gas refuel, parallel by default, key export

### Added

- **Fund with USDC only.** The SOL-for-fees check moved from preflight to after quoting: gasless routes (Jupiter pays fees and ATA rent — the common case for RFQ market makers) need no SOL at all, so a wallet holding only USDC can trade. Only non-gasless routes (the Metis fallback) still require ~0.002 SOL, surfaced as `insufficient_funds` with a route-aware message. The sign-time guard now also understands native-SOL output (wSOL unwrap credits lamports on the owner, not a token ATA) — verified end-to-end over mock RPC.
- **Automatic SOL gas refuel.** Before a real `buy`/`buy-basket`/`send` (never `send SOL`, never dry-run), if SOL is below 0.003 and USDC covers the operation plus 5 USDC, the CLI buys SOL first. The size is **dynamic**: target = 2× (50 transactions at the live network fee estimate + 5 token-account rents), converted to USDC at the quote's own implied SOL price, clamped to [5, 25] USDC. Bootstrapping from zero SOL requires a gasless route. Interactive runs prompt; `-y`/`--json` auto-approve; `RWA_NO_AUTO_GAS=1` disables. Reported as an optional `gas_refuel: {usdc, sol, tx}` object in `TradeJson`/`BuyBasketResultJson`/`SendJson` (additive). Best-effort: an impossible refuel never fails the main operation.
- **`rwa keys export [--reveal]`** prints the base58 keypair (the format Phantom/Solflare import), the solana-keygen JSON array, and the stored recovery phrase. Gated behind an interactive confirmation or an explicit `--reveal` (mandatory with `--json`).
- **Mnemonic-first `keys generate`.** New wallets derive from a fresh 12-word BIP39 phrase at the standard Solana path (`m/44'/501'/0'/0'` — restorable in Phantom/Solflare; derivation pinned against an independent reference vector). The phrase is printed once; encrypted wallets embed it inside the age payload so `keys export` can reveal it later (legacy payloads still load). Plaintext `key.json` stays a pure solana-keygen array and never stores the phrase. Seed-phrase imports keep the phrase the same way.

### Changed

- **Baskets and `close-all` execute in parallel by default** (live-measured: 1.4 s vs 13.8 s for a 2-position basket; concurrency stays bounded by internal quote/execute semaphores with per-item retries). `--sequential` opts into the old one-at-a-time 3 s spacing as a rate-limit fallback; `--parallel` is still accepted as a no-op for script compatibility.
- README rewritten: current timings, USDC-only quick start, no stale duplication.

---

## [0.3.0] — 2026-07-02 — 24/7 off-hours trading, 5 USDC minimum, safety hardening

### Breaking

- **Minimum buy amount raised from 1 to 5 USDC** (single `buy` and per item in `buy-basket`). Jupiter RFQ market makers routinely decline sub-5-USDC orders outside Regular hours, so 1–4 USDC buys mostly burned a ~14 s quote round-trip ending in `route_unfillable`; the local floor now fails fast with `amount_below_minimum`.
- **`gm reclaim` exits non-zero when every close batch failed.** Previously it printed `status:"error"` in JSON but exited 0, breaking the documented exit-code contract. It now flows through the standard error envelope (exit 1 with `error_kind`).
- **`rwa update --json` errors use the standard envelope** (`status`/`error`/`error_kind`) instead of a private shape, and transient failures (`network`, `rate_limited`) exit **75** instead of always 1 — same retry semantics as the trade paths.

### Added

- **24/7 off-hours trading.** Ondo's session-limits API gained an `offhours` session (weekends/NYSE holidays) where select flagship tokens (TSLAon, NVDAon, SPYon, QQQon, GOOGLon, …) trade around the clock. The CLI previously hard-blocked *all* trading whenever the ET calendar said Closed. Now gating is per-token: an offhours-enabled token trades on weekends; anything else gets a typed `market_closed` explaining that only flagships trade 24/7 and when regular trading resumes. `hours --tradable` and `close-all` skip-filtering work off-hours too. If the limits endpoint is unreachable *during* off-hours the check fails closed (eligibility can't be verified); in regular sessions it keeps failing open, now with a stderr warning.
- **Faster rent reclaim.** `reclaim` batches confirm at `processed` commitment (the `err` field is already authoritative there; stakes are rent dust), cutting the per-batch wait by the processed→confirmed gap (typically 2–6 s). `send` and swaps still confirm at `confirmed`.
- **`route_unfillable` on quote-time exhaustion.** When every quote backend (RFQ MMs and the Metis AMM tail) declines an order, the error now carries the stable `route_unfillable` kind instead of a bare message.

### Fixed

- **Money safety (sign-time guard):** an order whose quoted `outAmount` fails to parse is now *refused* — previously it silently became a 0 floor, disabling the under-delivery check. A missing `confirmationStatus` in an RPC response is no longer assumed confirmed (the timeout decides). The raw `sign_transaction` primitive is crate-private, so external swap signing must pass the intent-verifying guard.
- **Key files are created with `0o600` from the first byte** (`OpenOptions::mode`), closing the window where a freshly written plaintext key was readable under the process umask.
- **`sell` reports `insufficient_funds`** as a typed error kind (parity with `buy`); `sell-basket` gains the over-balance check it lacked. One shared resolver now handles `all`/`N%`/exact for both paths.
- **MM quote rejections fall through backends fast.** "Quote not available from market maker" was retried against the same backend with backoff (~14 s) and then hard-failed without trying Ultra or Metis. It now tries each backend once (~2.7 s total) and can fill via the AMM tail. A base URL pinned to `/swap/v1` also now dispatches through the correct Metis `/quote`+`/swap` flow instead of the managed `/order` endpoint.
- **Robustness:** Ondo API cache writes are atomic (tmp + rename — no more truncated cache from concurrent runs); a 5xx-exhausted RPC endpoint correctly falls through to the next URL on the batch path; the NYSE holiday calendar can no longer panic the CLI on date math; `keys encrypt`/`decrypt` registry repointing matches symlink/`..` path forms instead of leaving entries dangling.

### Changed

- **Internal decoupling (no behavior change):** SPL primitives moved to a shared leaf module (breaking the `wallet`↔`solana` dependency cycle); the portfolio Jupiter-holdings fallback moved from the solana layer to usecases; portfolio P&L, close-all filtering, and SOL fee-reservation math moved out of the CLI layer into unit-tested usecases; ~500 lines of duplicated retry/error/cache plumbing collapsed into shared helpers.
- **Test suite: 372 → 396**, with independent oracles: our own transaction construction is pinned against the on-chain ABI (System Transfer, TransferChecked, ATA Create, CloseAccount), SLIP-10 mnemonic derivation is pinned to an independently derived reference vector, the sign-time simulation gate and the full send pipeline run end-to-end over mock RPC, and every trading/transfer command now has a spawned-binary `--json` contract test.

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
