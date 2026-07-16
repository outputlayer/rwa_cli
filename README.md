# rwa — Trade Tokenized Stocks on Solana

CLI for buying & selling tokenized stocks and ETFs ([Ondo Global Markets](https://ondo.finance/)) on Solana via Jupiter. 438 tokens — TSLA, AAPL, NVDA, SPY, QQQ, and more. Flagship tokens trade 24/7.

> **⚠️ Beta (pre-v1).** Breaking changes possible. Not financial advice — use at your own risk, and always preview with `--dry-run` before trading real funds.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/outputlayer/rwa_cli/main/install.sh | sh
```

Downloads a pre-built binary for Linux/macOS/Windows from [Releases](https://github.com/outputlayer/rwa_cli/releases), falling back to a source build. Update later with `rwa update`.

The installer verifies every download against the release's SHA-256 checksums and fails closed (refuses to install) if that verification is impossible — no manifest, no checksum entry, or no `sha256sum`/`shasum` on the system. `RWA_INSTALL_INSECURE=1` is an explicit, unrecommended bypass.

## Quick start

```bash
rwa keys generate                # 1. Create a wallet (encrypted; shows a recovery phrase)
rwa keys show                    # 2. Your address — fund it with USDC
rwa gm buy TSLA 10 --dry-run     # 3. Preview a $10 TSLA buy
rwa gm buy TSLA 10 -y            # 4. Execute it
rwa gm portfolio                 # 5. See your holdings
```

**USDC is enough.** If the wallet has no SOL, the CLI buys a small amount automatically for network fees (dynamic 5–25 USDC, sized to current fees; disable with `RWA_NO_AUTO_GAS=1`) and tops it up when it runs low. Swaps themselves are usually gasless.

## Commands

| Command | Description |
|---------|-------------|
| `rwa gm buy <SYM> <AMT> [-y]` | Buy with USDC (min 5 USDC) |
| `rwa gm sell <SYM> <AMT> [-y]` | Sell for USDC (exact, `50%`, or `all`) |
| `rwa gm buy/sell ... --limit-price <P> [share\|token]` | Conditional trade: fills only at ≤ P (buy) / ≥ P (sell); e.g. `--limit-price 748 share` (bare = token) |
| `rwa gm close-all [<PCT>] [-y]` | Sell every position (or a % of each) |
| `rwa gm buy-basket <SYM AMT ...> [-y]` | Buy multiple tokens at once |
| `rwa gm buy-basket <SYM PCT% ...> --total <USDC> [-y]` | Split a USDC total across percent weights |
| `rwa gm sell-basket <SYM AMT ...> [-y]` | Sell multiple tokens at once |
| `rwa gm portfolio [WALLET]` | Holdings + allocation + 24h change |
| `rwa gm pnl` | Avg entry price + realized/unrealized P&L (from your CLI trades) |
| `rwa gm hours [--tradable]` | Current session (+ what trades right now; 24/7 = flagships only) |
| `rwa gm list` / `search` / `tradable` | Browse & filter (sector, `--tag asia/dividend/...`) |
| `rwa gm history <SYM> [-r 1D..ALL]` | Price history |
| `rwa gm send <TOKEN> <AMT> <TO> [-y]` | Transfer SOL/USDC/tokens to another wallet |
| `rwa gm reclaim` | Close empty token accounts, reclaim SOL rent |
| `rwa keys generate/import/export/show` | Wallet: create, import, back up |
| `rwa keys encrypt/decrypt` | Toggle passphrase encryption of the key file |
| `rwa keys store-passphrase/forget-passphrase` | Save (TTY-only) / remove (headless OK) the passphrase in the OS keychain |
| `rwa keys add/list/use/remove` | Multiple named wallets |
| `rwa update [-y]` | Self-update |

Every trading command takes `--dry-run` (preview), `-y` (skip confirmation), `--slippage <BPS>`, `--max-bps <N>` (cost ceiling), and `--json`.

## Things to know

- **Preview first.** `--dry-run` validates and quotes without executing; `--quote-only` (buy) quotes any size even without funds.
- **Conditional orders.** `--limit-price <P>` (buy/sell) only fills if the quoted price is ≤ P (buy) / ≥ P (sell), else `condition_not_met` (exit 1) — in `--dry-run` too; worst-case fill is limit ± slippage. Add `share` to gate per underlying share instead of per raw token. For a synthetic limit order, run it on a schedule until it fills: `rwa gm buy TSLA 100 --limit-price 400 --slippage 20 -y --json`.
- **Amounts** are exact (`100`), percent (`50%`), or `all` — never silently rounded. Minimum buy: 5 USDC. Token sell amounts are raw; for dividend-accruing tokens wallets display slightly more (shares_per_token multiplier) — if an exact sell overshoots, the error shows both raw and wallet-displayed numbers; prefer `all`/`50%`.
- **P&L is tracked automatically.** Every CLI trade lands in a local per-wallet ledger with a tamper-evident hash chain; `rwa gm pnl` shows average entry price, realized and unrealized P&L (built from your trades only) plus `ledger_integrity` (`ok`/`legacy`/`broken@line N`) so a corrupted ledger is visible instead of silently mispricing.
- **`sell` swaps to USDC; `send` transfers out.** `send USDC all` sends your *entire* USDC balance.
- **Multi-token commands run in parallel** by default (internally bounded); `--sequential` is the rate-limit fallback. `close-all` is the canonical exit — it skips dust and reports what it skipped.
- **Slippage**: 1% default, hard-blocked above 3%; unfillable routes are retried through another market maker automatically.
- **Liquidity is market-maker-driven.** These tokens trade only against USDC, filled by Ondo's RFQ makers (who mint just-in-time). Thin single-name tokens depend on a maker being able to fill *right now* — a rare `route_unfillable` (exit 1) means none could; retry shortly. Under a keyless wallet, Jupiter's per-wallet rate limit also bounds throughput (set `RWA_JUPITER_API_KEY` to raise it).
- **Back up your wallet.** `keys generate` derives from a BIP39 phrase (works in Phantom/Solflare) and shows it once; encrypted wallets keep it — reveal anytime with `keys export --reveal`.
- **Sessions are ET-based** — Pre-Market 4:00, Regular 9:30, Post-Market 16:00, Overnight 20:00. Weekends/holidays are off-hours: only flagship tokens (TSLAon, NVDAon, SPYon, QQQon, GOOGLon, …) trade then. `rwa gm hours --tradable` shows what's available right now.

## For agents

```bash
npx skills add outputlayer/rwa_skills -g -y
```

Then talk in plain language: *"Buy $100 of TSLA · Show my portfolio · Send all USDC to a friend's address"*.

Every command supports `--json` — a stable contract with typed `error_kind` values and exit code 75 for retry-worthy transient failures (1 otherwise). Rules: prefer `--json`, use `-y` only for real execution, never run wallet-changing commands as parallel shell processes (multi-token commands parallelize internally). **`--json` without `-y` never executes** the six gated money-moving commands (buy, sell, send, buy-basket, sell-basket, close-all) — it fails closed instead of running non-interactively (an otherwise-valid trade returns `error_kind: confirmation_required` (exit 1); a precondition failure like `insufficient_funds`/`no_position` surfaces its own kind first); add `-y` to execute or `--dry-run` to preview. `reclaim` reclaims rent to your own wallet and runs without confirmation. Manual skill install: [outputlayer/rwa_skills](https://github.com/outputlayer/rwa_skills).

## RPC endpoints

Public Solana endpoints are rate-limited. The CLI retries with backoff and falls back to Jupiter's holdings API for `portfolio`, but for regular use get a free dedicated endpoint (Helius, Alchemy, QuickNode, …):

```bash
export RWA_RPC_URL="https://<your-endpoint>"       # Solana RPC (or --rpc-url)
export RWA_JUPITER_API_KEY="<your-jupiter-key>"    # optional: higher Jupiter limits
```

<details>
<summary><b>Performance (live-measured)</b></summary>

| Command | Time |
|---------|------|
| `keys show`, `--help` | ~10 ms |
| `gm hours` (cached), `list`, `tradable` | 30–200 ms |
| `gm portfolio`, `pnl` | 0.2–1 s |
| single quote (`buy`/`sell --dry-run`) | ~0.6 s |
| `gm buy` / `sell` (single, executed) | 1–6 s |
| baskets / `close-all` (parallel, default) | ~1.3 s for 3 tokens on a healthy wallet¹ |
| baskets / `close-all` (`--sequential`) | ~7.4 s for 3 tokens (3 s spacing) |
| `gm reclaim` | ~1.5 s |

¹ Parallel quotes are staggered (`RWA_QUOTE_STAGGER_MS`, default 350 ms) to dodge Jupiter's per-wallet 429 burst; best case is `~0.6 s + (N−1) × stagger`. Without an API key the per-wallet rate limit still bounds throughput — under heavy use latency degrades to multi-second retries. Setting **`RWA_JUPITER_API_KEY`** auto-selects the keyed profile (stagger 0, higher quote concurrency) — no manual tuning needed (`RWA_QUOTE_STAGGER_MS` still overrides). Numbers are network-bound (Jupiter / Solana RPC) and vary with connectivity and rate-limit state.

</details>

<details>
<summary><b>How it works</b></summary>

- Ondo GM tokens are **total-return trackers** (dividends reinvested, so 1 token ≠ 1 share): token price = share price × multiplier, and `portfolio` shows `shares_per_token` once it drifts from 1. `TSLA` and `TSLAon` are both accepted.
- Swaps route through [Jupiter](https://jup.ag/) (`swap/v2` → Ultra fallbacks → Metis). Quotes with bad slippage are refreshed across market makers; a route that would fail on-chain is excluded and requoted. Quote fetching backs off up to 3.2s between retries within a ~20s budget per quote-fetch pass, printing a `still fetching quote (...)` heartbeat to stderr so a slow quote stays visible instead of hanging silently.
- **Every swap is verified before signing**: the transaction is simulated on-chain and the CLI refuses to sign unless the balance deltas match the quote (debit ≤ expected, credit ≥ quote − slippage). Wallet keys never leave the machine; encrypted storage is `age` with your passphrase.
- `portfolio` JSON: `cash.{sol,usdc}` + `gm_positions.{value_usd, change_24h_usd, positions[]}`. Surfaced `error_kind` values: `market_closed`, `not_tradable`, `slippage_too_high`, `cost_too_high`, `condition_not_met`, `trading_paused`, `route_unfillable`, `rpc_unavailable`, `insufficient_funds`, `amount_below_minimum`, `no_position`, `confirmation_timeout`, `on_chain_failure`, `execute_unavailable`, `confirmation_required` (`--json` without `-y`), plus input kinds `unknown_token`, `invalid_amount`, `invalid_address`, `unknown_wallet`, `self_send`, `reveal_required`, `interactive_required`, `recipient_not_allowed`, and `lock_contention` (transient, exit 75).

</details>

<details>
<summary><b>Architecture</b></summary>

```
bin/rwa/        → binary entry point
crates/cli/     → clap parsing, human/JSON output, process lock
crates/ondo/    → protocol layer
  usecases/     → trade orchestration (prepare, execute, retry, gas refuel)
  solana/       → RPC retry/race, balances, fees, transfers, swap simulation
  jupiter/      → swap backends (order, execute, holdings)
  api/          → Ondo sessions, prices, history
  wallet/       → Ed25519 + SLIP-10/BIP39, sign-time tx verification
  spl.rs        → shared SPL primitives (program ids, ATA derivation)
```

~19k lines of pure Rust (no C deps, no `unsafe`), 500+ tests including adversarial sign-time verification and spawned-binary JSON-contract tests.

</details>

## Development

```bash
make ci                # run before pushing — exact mirror of the GitHub CI (RUSTFLAGS=-Dwarnings)
make install-hooks     # optional: pre-push hook that runs `make ci` automatically
```

CI treats warnings as errors (`-Dwarnings`), so `make ci` — not a plain `cargo test`/`clippy` — is what keeps `main` green.

## License

MIT
