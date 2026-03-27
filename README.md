# rwa — Trade Tokenized Stocks on Solana

> **⚠️ Warning:** This project is pre-v1 (early alpha) and should be considered unstable. Breaking changes may be introduced without warning. This is not financial advice — use at your own risk. Always verify transactions before confirming.

CLI for buying & selling tokenized stocks and ETFs ([Ondo Global Markets](https://ondo.finance/)) on Solana via Jupiter.

264 tokens available — TSLA, AAPL, NVDA, SPY, QQQ, and more.

## Install

```bash
# Install script (builds from source, installs Rust if needed):
curl -fsSL https://raw.githubusercontent.com/outputlayer/rwa_cli/main/install.sh | sh

# Or directly via cargo:
cargo install --git https://github.com/outputlayer/rwa_cli --bin rwa
```

Pre-built binaries for macOS and Linux are available on [GitHub Releases](https://github.com/outputlayer/rwa_cli/releases).

## Agent Skills

**Send this to any AI agent to get started:**

```
I'd like to trade tokenized stocks on Solana.

First install the CLI: curl -fsSL https://raw.githubusercontent.com/outputlayer/rwa_cli/main/install.sh | bash

Then if npm is available, install skills: npx skills add outputlayer/rwa_skills -g -y
```

Install the rwa skill for AI agents via [Agent Skills](https://agentskills.io/):

```bash
# 1. Install the CLI first (required)
curl -fsSL https://raw.githubusercontent.com/outputlayer/rwa_cli/main/install.sh | bash

# 2. Then install skills for your AI agent
npx skills add outputlayer/rwa_skills -g -y
```

Or see [outputlayer/rwa_skills](https://github.com/outputlayer/rwa_skills) for manual install (Cursor, Claude Code, OpenCode, etc.).

## Quick Start

```bash
rwa keys generate               # Create Solana wallet
rwa gm hours                    # Market session + tradable count
rwa gm hours --tradable         # List all tradable tokens right now
rwa gm list                     # See all 264 tokens (with tradable status)
rwa gm list --search biotech    # Search tokens by keyword
rwa gm quote TSLA 100           # Quote: 100 USDC → TSLA
rwa gm buy TSLA 100 -y          # Execute buy
rwa gm sell TSLA 50% -y         # Sell 50% of TSLA position
rwa gm close-all -y             # Sell ALL positions (sequential)
rwa gm close-all 50% -y         # Sell 50% of every position
rwa gm portfolio                # View holdings + P&L
rwa gm send USDC 100 <ADDR> -y  # Send USDC to another wallet
rwa gm reclaim                  # Close empty accounts, reclaim SOL rent
```

Fund your wallet with SOL (gas) and USDC (trading) before your first trade.

## Commands

| Command | Description |
|---------|-------------|
| `rwa gm hours` | Current session + tradable count |
| `rwa gm hours --tradable` | List all tradable tokens in current session |
| `rwa gm list` | All 264 tokens with tradable status |
| `rwa gm list --search <keyword>` | Search tokens by name/symbol/sector (includes `tradable` field) |
| `rwa gm quote <SYM> <AMT>` | Swap quote (buy) |
| `rwa gm quote <SYM> <AMT> --sell` | Swap quote (sell) |
| `rwa gm buy <SYM> <AMT> -y` | Buy with USDC |
| `rwa gm sell <SYM> <AMT> -y` | Sell for USDC (exact, `50%`, or `all`) |
| `rwa gm close-all -y` | Sell ALL positions sequentially |
| `rwa gm close-all <PCT> -y` | Sell percentage of every position (e.g. `10%`, `50%`) |
| `rwa gm portfolio [WALLET]` | Holdings + allocation + 24h change |
| `rwa gm history <SYM> [-r RANGE]` | Price history (1D/1W/1M/3M/1Y/ALL) |
| `rwa gm send <TOKEN> <AMT> <TO> -y` | Send SOL/USDC/tokens to another wallet |
| `rwa gm reclaim` | Close empty token accounts, reclaim SOL rent |
| `rwa gm reclaim --token <SYM>` | Reclaim only for a specific token |
| `rwa keys generate` | Create new wallet |
| `rwa keys import --seed-phrase "..."` | Import from mnemonic |
| `rwa keys show` | Show address + key file path |

### Flags

- `--json` — Machine-readable JSON output on any command
- `-y` — Skip confirmation on buy/sell
- `--slippage <BPS>` — Max slippage in basis points (e.g. `50` = 0.5%). Default: auto (Jupiter RTSE)
- `--rpc-url <URL>` — Custom Solana RPC (or set `RWA_RPC_URL`)

### Amount Formats

- Exact: `100` (100 USDC or 100 tokens)
- Percentage: `50%` (half of balance)
- All: `all` (entire balance)

## JSON Output

Every command supports `--json` for agent/script integration:

```bash
rwa --json gm portfolio
rwa --json gm list --search biotech
rwa --json gm hours
rwa --json gm quote TSLA 100
rwa --json gm buy TSLA 100 -y
rwa --json gm sell TSLA 50% -y
rwa --json gm close-all -y
rwa --json gm close-all 50% -y
```

## About Ondo GM Tokens

- 264 tokenized stocks & ETFs on Solana
- **Total return trackers** — dividends reinvested, 1 token ≠ 1 share
- Both `TSLA` and `TSLAon` symbol formats accepted
- Swaps via [Jupiter](https://jup.ag/) on Solana

### Trading Sessions (ET)

| Session | Hours |
|---------|-------|
| Pre-Market | 4:00 AM – 9:29 AM |
| Regular | 9:30 AM – 3:59 PM |
| Post-Market | 4:00 PM – 7:59 PM |
| Overnight | 8:00 PM – 3:59 AM |
| Closed | Fri 8 PM – Sun 8 PM |

Not all tokens are tradable in every session. Use `rwa gm hours --tradable` or the `tradable` field in `--json` output to check.

### Close-All Limits

- Positions worth less than **$1.50** are automatically skipped during `close-all` (Jupiter market makers reject small swaps)
- Skipped tokens are reported separately in both text and JSON output

## Performance

| Command | Time | Notes |
|---------|------|-------|
| `--help` | 7ms | Local |
| `keys show` | 7ms | Local |
| `gm hours` | 0.5s | Ondo API |
| `gm history` | 0.5s | Ondo API |
| `gm quote` | 0.8s | Jupiter API |
| `gm portfolio` | 1.0s | RPC + Ondo (parallel) |
| `gm list` | 1.3s | Ondo API (264 tokens) |
| `gm buy/sell` | 2–4s | Preflight + Jupiter execute |

RPC health tracking: auto-remembers the fastest working endpoint. 5 public Solana RPCs with instant failover (100ms). Set `RWA_RPC_URL` to a private RPC for even faster responses.

## Architecture

```
bin/rwa/          → Binary entry point
crates/cli/       → CLI parsing (clap v4), output formatting
crates/ondo/      → Solana RPC, Jupiter API, Ondo API, wallet
  src/solana/     → RPC infrastructure (rpc.rs) + Solana operations (mod.rs)
  src/jupiter.rs  → Jupiter Ultra swap API
  src/api.rs      → Ondo session/pricing API
  src/wallet.rs   → Ed25519 wallet (SLIP-10, BIP39)
```

~5,400 lines of Rust, ~3 MB binary, 110 tests. Pure Rust — no native C dependencies, no `unsafe`. Uses `rustls-tls`, `ed25519-dalek` (with zeroize), fully cross-platform.

## Development

```bash
cargo build                  # Build
cargo run -- gm hours        # Test
cargo clippy                 # Lint
cargo install --path bin/rwa # Install locally
```

## Cross-Platform

- **macOS + Linux**: fully supported
- **Windows**: works via WSL 2 or Git Bash

## License

MIT
