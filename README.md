# rwa — Trade Tokenized Stocks on Solana

CLI for buying & selling tokenized stocks and ETFs ([Ondo Global Markets](https://ondo.finance/)) on Solana via Jupiter.

264 tokens available — TSLA, AAPL, NVDA, SPY, QQQ, and more.

## Install

```bash
# From source (requires Rust 1.91+):
cargo install --git https://github.com/outputlayer/rwa_cli --bin rwa

# Or via install script:
curl -fsSL https://raw.githubusercontent.com/outputlayer/rwa_cli/main/install.sh | sh
```

## Agent Skills

**Send this to any AI agent to get started:**

```
I'd like to trade tokenized stocks on Solana.

Install skills if npm is available: npx skills add outputlayer/rwa_skills -g

Otherwise install the CLI directly: curl -fsSL https://raw.githubusercontent.com/outputlayer/rwa_cli/main/install.sh | bash
```

Install the rwa skill for AI agents via [Agent Skills](https://agentskills.io/):

```bash
npx skills add outputlayer/rwa_skills -g
```

Or see [outputlayer/rwa_skills](https://github.com/outputlayer/rwa_skills) for manual install (Cursor, Claude Code, OpenCode, etc.).

## Quick Start

```bash
rwa keys generate               # Create Solana wallet
rwa gm hours                    # Check market (Sun 8pm – Fri 8pm ET)
rwa gm list                     # See all 264 tokens
rwa gm quote TSLA 100           # Quote: 100 USDC → TSLA
rwa gm buy TSLA 100 -y          # Execute buy
rwa gm portfolio                # View holdings + P&L
```

Fund your wallet with SOL (gas) and USDC (trading) before your first trade.

## Commands

| Command | Description |
|---------|-------------|
| `rwa gm hours` | Market status (OPEN/CLOSED + countdown) |
| `rwa gm list` | All 264 available tokens |
| `rwa gm quote <SYM> <AMT>` | Swap quote (buy) |
| `rwa gm quote <SYM> <AMT> --sell` | Swap quote (sell) |
| `rwa gm buy <SYM> <AMT> -y` | Buy with USDC |
| `rwa gm sell <SYM> <AMT> -y` | Sell for USDC |
| `rwa gm portfolio [WALLET]` | Holdings + allocation + 24h change |
| `rwa gm history <SYM> [-r RANGE]` | Price history (1D/1W/1M/3M/1Y/ALL) |
| `rwa keys generate` | Create new wallet |
| `rwa keys import --seed-phrase "..."` | Import from mnemonic |
| `rwa keys show` | Show address + key file path |

### Flags

- `--json` — Machine-readable JSON output on any command
- `-y` — Skip confirmation on buy/sell
- `--rpc-url <URL>` — Custom Solana RPC (or set `RWA_RPC_URL`)

### Amount Formats

- Exact: `100` (100 USDC or 100 tokens)
- Percentage: `50%` (half of balance)
- All: `all` (entire balance)

## JSON Output

Every command supports `--json` for agent/script integration:

```bash
rwa --json gm portfolio
rwa --json gm list
rwa --json gm hours
rwa --json gm quote TSLA 100
```

## About Ondo GM Tokens

- 264 tokenized stocks & ETFs on Solana
- **Total return trackers** — dividends reinvested, 1 token ≠ 1 share
- Trading hours: 24/5 (Sunday 8 PM — Friday 8 PM ET)
- Both `TSLA` and `TSLAon` symbol formats accepted
- Swaps via [Jupiter](https://jup.ag/) on Solana

## Architecture

```
bin/rwa/     → Binary entry point
crates/cli/  → CLI parsing (clap v4), output formatting
crates/ondo/ → Solana RPC, Jupiter API, Ondo API, wallet
```

Pure Rust — no native C dependencies. Uses `rustls-tls`, `ed25519-dalek`, fully cross-platform.

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
