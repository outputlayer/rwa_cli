# rwa — Trade Tokenized Stocks on Solana

> **⚠️ Warning:** This project is pre-v1 (beta) and may still introduce breaking changes. This is not financial advice — use at your own risk. Always verify transactions before confirming.

CLI for buying & selling tokenized stocks and ETFs ([Ondo Global Markets](https://ondo.finance/)) on Solana via Jupiter.

264 tokens available — TSLA, AAPL, NVDA, SPY, QQQ, and more.

## Install

```bash
# Recommended: binary-first install script
curl -fsSL https://raw.githubusercontent.com/outputlayer/rwa_cli/main/install.sh | sh

# Or directly via cargo:
cargo install --git https://github.com/outputlayer/rwa_cli --bin rwa
```

Pre-built binaries for Linux, macOS, and Windows are published on [GitHub Releases](https://github.com/outputlayer/rwa_cli/releases).

Install a specific release:

```bash
curl -fsSL https://raw.githubusercontent.com/outputlayer/rwa_cli/main/install.sh | RWA_VERSION=v0.2.9 sh
```

## Agent Skills

For agents:

```bash
curl -fsSL https://raw.githubusercontent.com/outputlayer/rwa_cli/main/install.sh | sh
npx skills add outputlayer/rwa_skills -g -y
```

Then tell the agent the task directly:

```
Buy $100 of TSLAon
Show my portfolio
Create a wallet
Send all USDC to <ADDRESS>
```

The skills repo is [outputlayer/rwa_skills](https://github.com/outputlayer/rwa_skills).

| Skill | Best for |
|-------|----------|
| `rwa-trade` | Buy, sell, market hours, tradable tokens, close-all |
| `rwa-portfolio` | Holdings, allocation, 24h change, history |
| `rwa-wallet` | Wallet creation/import, encryption, send/withdraw, reclaim |

Recommended agent behavior:

- Prefer `rwa --json` for machine-readable output
- Use `--dry-run` before large or uncertain trades
- Never run buy/sell/send commands in parallel for the same wallet
- For multi-token liquidation, use `rwa gm close-all` instead of manual loops
- Treat JSON output as a stable contract; do not rely on legacy flat `portfolio` fields

Or see [outputlayer/rwa_skills](https://github.com/outputlayer/rwa_skills) for manual install (Cursor, Claude Code, OpenCode, etc.).

## Quick Start

```bash
rwa keys generate               # Create Solana wallet
rwa keys generate --encrypt     # Create encrypted wallet
rwa gm hours                    # Market session + tradable count
rwa gm hours --tradable         # List all tradable tokens right now
rwa gm list                     # See all 264 tokens (with tradable status)
rwa gm search --search biotech  # Search tokens by keyword
rwa gm tradable TSLA NVDA       # Check tradable status for specific symbols
rwa gm search --tradable-only --sector Energy   # Bulk tradable filter without Python
rwa gm buy TSLA 100 --dry-run   # Validate + preview buy
rwa gm buy TSLA 100 -y          # Execute buy
rwa gm sell TSLA 50% --dry-run  # Validate + preview sell
rwa gm sell TSLA 50% -y         # Sell 50% of TSLA position
rwa gm close-all -y                          # Sell ALL positions (sequential)
rwa gm close-all --parallel -y               # Sell ALL positions in parallel (~4x faster)
rwa gm close-all 50% -y                      # Sell 50% of every position
rwa gm buy-basket AAPL 50 TSLA 50 NVDA 50 -y              # Buy multiple tokens sequentially
rwa gm buy-basket AAPL 50 TSLA 50 NVDA 50 --parallel -y   # Buy multiple tokens in parallel
rwa gm sell-basket AAPL 2 TSLA 1 NVDA all -y              # Sell multiple tokens sequentially
rwa gm sell-basket AAPL 2 TSLA 1 NVDA all --parallel -y   # Sell multiple tokens in parallel
rwa gm portfolio                # View GM holdings + cash balances
rwa gm send USDC 100 <ADDR> -y  # Send USDC to another wallet
rwa gm reclaim                  # Close empty accounts, reclaim SOL rent
```

Fund your wallet with SOL (gas) and USDC (trading) before your first trade.

## Common Flows

Preview then buy:

```bash
rwa --json gm buy TSLA 100 --dry-run
rwa --json gm buy TSLA 100 -y
```

Preview then sell part of a position:

```bash
rwa --json gm sell TSLA 50% --dry-run
rwa --json gm sell TSLA 50% -y
```

Preview then exit everything:

```bash
rwa --json gm close-all --dry-run
rwa --json gm close-all -y                   # sequential: 3s gap between swaps
rwa --json gm close-all --parallel -y        # parallel: all swaps at once (~4–11x faster)
rwa --json gm reclaim
```

Buy a basket of tokens:

```bash
# Preview all quotes at once
rwa --json gm buy-basket AAPL 25 TSLA 25 NVDA 25 SPY 25 --dry-run

# Execute in parallel (all swaps simultaneous)
rwa --json gm buy-basket AAPL 25 TSLA 25 NVDA 25 SPY 25 --parallel -y

# Execute sequentially
rwa --json gm buy-basket AAPL 25 TSLA 25 NVDA 25 SPY 25 -y

# Different amount per token
rwa --json gm buy-basket AAPL 100 TSLA 50 NVDA 25 --parallel -y
```

Sell a basket of specific tokens:

```bash
# Preview
rwa --json gm sell-basket SPY 5 TSLA 3 NVDA all --dry-run

# Execute in parallel
rwa --json gm sell-basket SPY 5 TSLA 3 NVDA all --parallel -y

# Sell % of each
rwa --json gm sell-basket SPY 50% TSLA 50% --parallel -y
```

Withdraw after liquidation:

```bash
rwa --json gm send USDC all <ADDR> -y
rwa --json gm send SOL all <ADDR> -y
```

## Commands

| Command | Description |
|---------|-------------|
| `rwa gm hours` | Current session + tradable count |
| `rwa gm hours --tradable` | List all tradable tokens in current session |
| `rwa gm list` | All 264 tokens with tradable status |
| `rwa gm search --search <keyword>` | Search tokens by name/symbol/sector |
| `rwa gm search --tradable-only --sector <SECTOR> --type <stock\|etf> --name-keyword <WORD>` | Bulk filtering without external scripts |
| `rwa gm tradable [SYM ...]` | Check tradable status for one, many, or all tokens |
| `rwa gm buy <SYM> <AMT> --dry-run` | Validate + preview buy quote without executing |
| `rwa gm buy <SYM> <AMT> -y` | Buy with USDC |
| `rwa gm sell <SYM> <AMT> --dry-run` | Validate + preview sell quote without executing |
| `rwa gm sell <SYM> <AMT> -y` | Sell for USDC (exact, `50%`, or `all`) |
| `rwa gm close-all -y` | Sell ALL positions sequentially |
| `rwa gm close-all --parallel -y` | Sell ALL positions in parallel (much faster for 4+ tokens) |
| `rwa gm close-all <PCT> -y` | Sell percentage of every position (e.g. `10%`, `50%`) |
| `rwa gm buy-basket <SYM AMT ...> -y` | Buy multiple tokens (per-token amounts) sequentially |
| `rwa gm buy-basket <SYM AMT ...> --parallel -y` | Buy multiple tokens in parallel |
| `rwa gm sell-basket <SYM AMT ...> -y` | Sell multiple tokens (per-token amounts) sequentially |
| `rwa gm sell-basket <SYM AMT ...> --parallel -y` | Sell multiple tokens in parallel |
| `rwa gm portfolio [WALLET]` | GM holdings + allocation + 24h change |
| `rwa gm history <SYM> [-r RANGE]` | Price history (1D/1W/1M/3M/1Y/ALL) |
| `rwa gm send <TOKEN> <AMT> <TO> -y` | Send SOL/USDC/tokens to another wallet |
| `rwa gm reclaim` | Close empty token accounts, reclaim SOL rent |
| `rwa gm reclaim --token <SYM>` | Reclaim only for a specific token |
| `rwa keys generate` | Create new wallet |
| `rwa keys import --seed-phrase "..."` | Import from mnemonic |
| `rwa keys show` | Show address + key file path |
| `rwa update` | Update rwa to the latest release (`--check` to preview, `-y` to skip confirm) |

### Flags

- `--json` — Machine-readable JSON output on any command
- `-y` — Skip confirmation on buy/sell/send/close-all
- `--slippage <BPS>` — Max slippage in basis points (e.g. `50` = 0.5%). Default: 1% (100 bps)
- `--rpc-url <URL>` — Custom Solana RPC (or set `RWA_RPC_URL`)
- `--dry-run` — Validate and preview a trade/transfer without executing it
- `--parallel` — On `close-all` and `buy-basket`: fetch all orders then execute all swaps simultaneously instead of sequentially

For agents and scripts:

- Default to `rwa --json ...`
- Use `--dry-run` before large trades, basket exits, or user-visible previews
- Prefer the CLI's returned values over manual arithmetic after execution

### Amount Formats

- Exact: `100` (100 USDC or 100 tokens)
- Percentage: `50%` (half of balance)
- All: `all` (entire balance)

Amounts are converted with exact on-chain precision. Inputs with too many decimal places are rejected instead of being silently rounded.

## JSON Output

Every command supports `--json` for agent/script integration:

```bash
rwa --json gm portfolio
rwa --json gm search --search biotech
rwa --json gm hours
rwa --json gm buy TSLA 100 --dry-run
rwa --json gm buy TSLA 100 -y
rwa --json gm sell TSLA 50% -y
rwa --json gm close-all -y
rwa --json gm close-all 50% -y
rwa --json gm send USDC all <ADDR> -y
```

`portfolio` now separates cash from GM positions in the JSON contract:

- `cash.sol`
- `cash.usdc`
- `gm_positions.value_usd`
- `gm_positions.change_24h_usd`
- `gm_positions.change_24h_pct`
- `gm_positions.positions[].gm_alloc_pct`

Trade/send/close-all JSON shapes are covered by regression tests and are intended to remain stable for agents and scripts.

Malformed Ondo market prices now fail loudly instead of silently becoming `0.0`, so bad upstream data is easier to detect.

Common surfaced trade/runtime error kinds now map more cleanly to the real failure:

- `market_closed`
- `not_tradable`
- `slippage_too_high`
- `confirmation_timeout`
- `on_chain_failure`

## About Ondo GM Tokens

- 264 tokenized stocks & ETFs on Solana
- **Total return trackers** — dividends reinvested, 1 token ≠ 1 share
- Both `TSLA` and `TSLAon` symbol formats accepted
- Swaps via [Jupiter](https://jup.ag/) on Solana
- Public Jupiter routing is tried in this order: `lite-api.jup.ag/swap/v2`, then `ultra-api.jup.ag`, then `lite-api.jup.ag/ultra/v1`, with `lite-api.jup.ag/swap/v1` kept as the final manual fallback

### Trading Sessions (ET)

| Session | Hours |
|---------|-------|
| Pre-Market | 4:00 AM – 9:29 AM |
| Regular | 9:30 AM – 3:59 PM |
| Post-Market | 4:00 PM – 7:59 PM |
| Overnight | 8:00 PM – 3:59 AM |
| Closed | Weekend / NYSE holidays |

Not all tokens are tradable in every session. Use `rwa gm hours --tradable` or the `tradable` field in `--json` output to check.

### Close-All and Buy-Basket Limits

- Positions worth less than **$1.50** are automatically skipped during `close-all` (Jupiter market makers reject small swaps)
- Skipped tokens are reported separately in both text and JSON output
- `buy-basket` validates total USDC balance before any swap begins

## Performance

| Command | Time | Notes |
|---------|------|-------|
| `--help` | 7ms | Local |
| `keys show` | 7ms | Local |
| `gm hours` | 0.5s | Ondo API |
| `gm history` | 0.5s | Ondo API |
| `gm buy --dry-run` | 0.8s | Jupiter quote + validation |
| `gm portfolio` | 1.0s | RPC + Ondo (parallel) |
| `gm list` | 1.3s | Ondo API (264 tokens) |
| `gm buy/sell` | ~22s | Jupiter swap confirmation |
| `gm close-all` (N tokens, sequential) | N×22s + (N-1)×3s | e.g. 6 tokens ≈ 147s |
| `gm close-all --parallel` (N tokens) | ~22s flat | All swaps concurrent |
| `gm buy-basket` (N tokens, sequential) | N×22s + (N-1)×3s | Same as close-all |
| `gm buy-basket --parallel` (N tokens) | ~22s flat | All swaps concurrent |
| `gm sell-basket` (N tokens, sequential) | N×22s + (N-1)×3s | Per-token sell amounts |
| `gm sell-basket --parallel` (N tokens) | ~22s flat | All swaps concurrent |

### Parallel Speedup (empirical)

| Tokens | Sequential | Parallel | Saved | Speedup |
|--------|-----------|---------|-------|---------|
| 2 | ~47s | ~22s | 25s | 2.1× |
| 4 | ~97s | ~22s | 75s | 4.4× |
| 6 | ~147s | ~22s | 125s | 6.7× |
| 8 | ~197s | ~22s | 175s | 9.0× |
| 10 | ~247s | ~22s | 225s | 11.2× |

**Parallel-safe**: parallel trades of *different* tokens from the same wallet work at normal spread. jupiterz routes through multiple market makers internally — if one MM returns a bad quote on small orders, the CLI automatically retries to get a better one. Live-tested: 4-token parallel buy → immediate parallel sell → all 8 swaps at normal spread (0.2–0.7%).

RPC health tracking: auto-remembers the last good endpoint. 2 public Solana RPCs with fast failover; transient transport errors and rate limits are retried with exponential backoff before failover.

### Reliability & rate limits

The default public Solana endpoints (`api.mainnet-beta.solana.com`, `solana-rpc.publicnode.com`) are rate-limited and carry no SLA — Solana's own docs recommend against them for anything beyond development. Under load you may see:

```
Solana RPC error [unavailable] ... all RPC endpoints failed: [... HTTP 429 ...]
```

The fix is a dedicated endpoint via `RWA_RPC_URL` (or `--rpc-url`). Free tiers from Alchemy, Helius, QuickNode, Chainstack, or dRPC are far more reliable than the public nodes and need only a free API key:

```bash
export RWA_RPC_URL="https://<your-endpoint>"
rwa --json gm portfolio
```

When a custom URL is set the CLI uses only that endpoint (no silent fallback) — set one you trust.

## Architecture

```
bin/rwa/              → Binary entry point
crates/cli/           → CLI parsing (clap v4), output formatting, process lock
  src/cmd/gm/         → trade, close_all, basket, reclaim, list, portfolio, send
crates/ondo/          → Solana RPC, Jupiter API, Ondo API, wallet
  src/amounts.rs      → Exact amount parsing, formatting, percent math
  src/audit.rs        → Persistent JSONL audit log for swaps
  src/usecases/       → Buy/sell/close-all flows (gm, gm_execute, gm_order, gm_internal)
  src/solana/
    rpc/              → RPC retry + URL rotation (mod, error, sequential, race)
    fee.rs            → Priority fees + rent estimation
    transaction.rs    → Transaction building + sending
    transfer.rs       → SOL/SPL transfers + ATA management
    balance.rs        → Balances + portfolio queries
    mod.rs            → Validation + shared helpers
  src/jupiter.rs      → Jupiter swap backends (swap/v2 first, Ultra/public fallbacks)
  src/jupiter/        → order, execute, types
  src/api/            → Ondo session/pricing/history API (mod, assets, history, session)
  src/wallet/         → Ed25519 wallet (SLIP-10, BIP39) + transaction verification
```

~13k lines of Rust, ~3 MB binary, 200+ tests. Pure Rust, no native C dependencies, no `unsafe`. Uses `rustls-tls`, `ed25519-dalek` (with zeroize), and exact amount conversion helpers shared across trade and transfer flows.

## Development

```bash
cargo build                  # Build
cargo run -- gm hours        # Test
cargo clippy                 # Lint
cargo install --path bin/rwa # Install locally
```

## Cross-Platform

- **macOS + Linux**: supported
- **Windows**: release artifacts are targeted; if no matching binary is available yet, the install script falls back to source install

## License

MIT
