# rwa — Trade Tokenized Stocks on Solana

CLI for buying & selling tokenized stocks and ETFs ([Ondo Global Markets](https://ondo.finance/)) on Solana via Jupiter. 438 tokens — TSLA, AAPL, NVDA, SPY, QQQ, and more. Flagship tokens trade 24/7.

> **⚠️ Beta (pre-v1).** Breaking changes possible. Not financial advice — use at your own risk, and always preview with `--dry-run` before trading real funds.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/outputlayer/rwa_cli/main/install.sh | sh
```

Downloads a pre-built binary for Linux/macOS/Windows from [Releases](https://github.com/outputlayer/rwa_cli/releases), falling back to a source build. Update later with `rwa update`.

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
| `rwa gm close-all [<PCT>] [-y]` | Sell every position (or a % of each) |
| `rwa gm buy-basket <SYM AMT ...> [-y]` | Buy multiple tokens at once |
| `rwa gm sell-basket <SYM AMT ...> [-y]` | Sell multiple tokens at once |
| `rwa gm portfolio [WALLET]` | Holdings + allocation + 24h change |
| `rwa gm pnl` | Avg entry price + realized/unrealized P&L (from your CLI trades) |
| `rwa gm hours [--tradable]` | Current session (+ what trades right now) |
| `rwa gm list` / `search` / `tradable` | Browse & filter (sector, `--tag asia/dividend/...`) |
| `rwa gm history <SYM> [-r 1D..ALL]` | Price history |
| `rwa gm send <TOKEN> <AMT> <TO> [-y]` | Transfer SOL/USDC/tokens to another wallet |
| `rwa gm reclaim` | Close empty token accounts, reclaim SOL rent |
| `rwa keys generate/import/export/show` | Wallet: create, import, back up |
| `rwa keys encrypt/decrypt` | Toggle passphrase encryption of the key file |
| `rwa keys add/list/use/remove` | Multiple named wallets |
| `rwa update [-y]` | Self-update |

Every trading command takes `--dry-run` (preview), `-y` (skip confirmation), `--slippage <BPS>`, `--max-bps <N>` (cost ceiling), and `--json`.

## Things to know

- **Preview first.** `--dry-run` validates and quotes without executing; `--quote-only` (buy) quotes any size even without funds.
- **Amounts** are exact (`100`), percent (`50%`), or `all` — never silently rounded. Minimum buy: 5 USDC.
- **P&L is tracked automatically.** Every CLI trade lands in a local per-wallet ledger; `rwa gm pnl` shows average entry price, realized and unrealized P&L — built from your trades only.
- **`sell` swaps to USDC; `send` transfers out.** `send USDC all` sends your *entire* USDC balance.
- **Multi-token commands run in parallel** by default (internally bounded); `--sequential` is the rate-limit fallback. `close-all` is the canonical exit — it skips dust and reports what it skipped.
- **Slippage**: 1% default, hard-blocked above 3%; unfillable routes are retried through another market maker automatically.
- **Back up your wallet.** `keys generate` derives from a BIP39 phrase (works in Phantom/Solflare) and shows it once; encrypted wallets keep it — reveal anytime with `keys export --reveal`.
- **Sessions are ET-based** — Pre-Market 4:00, Regular 9:30, Post-Market 16:00, Overnight 20:00. Weekends/holidays are off-hours: only flagship tokens (TSLAon, NVDAon, SPYon, QQQon, GOOGLon, …) trade then. `rwa gm hours --tradable` shows what's available right now.

## For agents

```bash
npx skills add outputlayer/rwa_skills -g -y
```

Then talk in plain language: *"Buy $100 of TSLA · Show my portfolio · Send all USDC to <ADDRESS>"*.

Every command supports `--json` — a stable contract with typed `error_kind` values and exit code 75 for retry-worthy transient failures (1 otherwise). Rules: prefer `--json`, use `-y` only for real execution, never run wallet-changing commands as parallel shell processes (multi-token commands parallelize internally). Manual skill install: [outputlayer/rwa_skills](https://github.com/outputlayer/rwa_skills).

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
| `gm hours`, `history`, `portfolio` | 0.5–1 s |
| `gm buy` / `sell` (single) | 1–6 s |
| baskets / `close-all` (parallel, default) | 1.5–8 s for 2–10 tokens |
| baskets / `close-all` (`--sequential`) | ~N×5 s + 3 s spacing |
| `gm reclaim` | ~1.5 s |

</details>

<details>
<summary><b>How it works</b></summary>

- Ondo GM tokens are **total-return trackers** (dividends reinvested, so 1 token ≠ 1 share). `TSLA` and `TSLAon` are both accepted.
- Swaps route through [Jupiter](https://jup.ag/) (`swap/v2` → Ultra fallbacks → Metis). Quotes with bad slippage are refreshed across market makers; a route that would fail on-chain is excluded and requoted.
- **Every swap is verified before signing**: the transaction is simulated on-chain and the CLI refuses to sign unless the balance deltas match the quote (debit ≤ expected, credit ≥ quote − slippage). Wallet keys never leave the machine; encrypted storage is `age` with your passphrase.
- `portfolio` JSON: `cash.{sol,usdc}` + `gm_positions.{value_usd, change_24h_usd, positions[]}`. Surfaced `error_kind` values: `market_closed`, `not_tradable`, `slippage_too_high`, `cost_too_high`, `route_unfillable`, `rpc_unavailable`, `insufficient_funds`, `amount_below_minimum`, `no_position`, `confirmation_timeout`, `on_chain_failure`, `execute_unavailable`.

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

~19k lines of pure Rust (no C deps, no `unsafe`), 400+ tests including adversarial sign-time verification and spawned-binary JSON-contract tests.

</details>

## Development

```bash
cargo build && cargo test --workspace && cargo clippy --all-targets
```

## License

MIT
