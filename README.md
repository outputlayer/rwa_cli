# rwa — Trade Tokenized Stocks on Solana

> **⚠️ Warning:** This project is pre-v1 (early alpha) and should be considered unstable. Breaking changes may be introduced without warning. This is not financial advice — use at your own risk. Always verify transactions before confirming.

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
curl -fsSL https://raw.githubusercontent.com/outputlayer/rwa_cli/main/install.sh | RWA_VERSION=v0.1.0 sh
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

Or see [outputlayer/rwa_skills](https://github.com/outputlayer/rwa_skills) for manual install (Cursor, Claude Code, OpenCode, etc.).

## Quick Start

```bash
rwa keys generate               # Create Solana wallet
rwa keys generate --encrypt     # Create encrypted wallet
rwa gm hours                    # Market session + tradable count
rwa gm hours --tradable         # List all tradable tokens right now
rwa gm list                     # See all 264 tokens (with tradable status)
rwa gm list --search biotech    # Search tokens by keyword
rwa gm buy TSLA 100 --dry-run   # Validate + preview buy
rwa gm buy TSLA 100 -y          # Execute buy
rwa gm sell TSLA 50% --dry-run  # Validate + preview sell
rwa gm sell TSLA 50% -y         # Sell 50% of TSLA position
rwa gm close-all -y             # Sell ALL positions (sequential)
rwa gm close-all 50% -y         # Sell 50% of every position
rwa gm portfolio                # View GM holdings + cash balances
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
| `rwa gm buy <SYM> <AMT> --dry-run` | Validate + preview buy quote without executing |
| `rwa gm buy <SYM> <AMT> -y` | Buy with USDC |
| `rwa gm sell <SYM> <AMT> --dry-run` | Validate + preview sell quote without executing |
| `rwa gm sell <SYM> <AMT> -y` | Sell for USDC (exact, `50%`, or `all`) |
| `rwa gm close-all -y` | Sell ALL positions sequentially |
| `rwa gm close-all <PCT> -y` | Sell percentage of every position (e.g. `10%`, `50%`) |
| `rwa gm portfolio [WALLET]` | GM holdings + allocation + 24h change |
| `rwa gm history <SYM> [-r RANGE]` | Price history (1D/1W/1M/3M/1Y/ALL) |
| `rwa gm send <TOKEN> <AMT> <TO> -y` | Send SOL/USDC/tokens to another wallet |
| `rwa gm reclaim` | Close empty token accounts, reclaim SOL rent |
| `rwa gm reclaim --token <SYM>` | Reclaim only for a specific token |
| `rwa keys generate` | Create new wallet |
| `rwa keys import --seed-phrase "..."` | Import from mnemonic |
| `rwa keys show` | Show address + key file path |

### Flags

- `--json` — Machine-readable JSON output on any command
- `-y` — Skip confirmation on buy/sell/send/close-all
- `--slippage <BPS>` — Max slippage in basis points (e.g. `50` = 0.5%). Default: 1% (100 bps)
- `--rpc-url <URL>` — Custom Solana RPC (or set `RWA_RPC_URL`)
- `--dry-run` — Validate and preview a trade/transfer without executing it

### Amount Formats

- Exact: `100` (100 USDC or 100 tokens)
- Percentage: `50%` (half of balance)
- All: `all` (entire balance)

Amounts are converted with exact token precision. Inputs with too many decimal places are rejected instead of being silently rounded.

## JSON Output

Every command supports `--json` for agent/script integration:

```bash
rwa --json gm portfolio
rwa --json gm list --search biotech
rwa --json gm hours
rwa --json gm buy TSLA 100 --dry-run
rwa --json gm buy TSLA 100 -y
rwa --json gm sell TSLA 50% -y
rwa --json gm close-all -y
rwa --json gm close-all 50% -y
rwa --json gm send USDC all <ADDR> -y
```

`portfolio` returns `sol` and `usdc` separately. Current `total_value_usd`, `change_24h_usd`, `change_24h_pct`, and `positions[].alloc_pct` describe GM positions only, not wallet cash plus positions combined.

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
| `gm buy --dry-run` | 0.8s | Jupiter quote + validation |
| `gm portfolio` | 1.0s | RPC + Ondo (parallel) |
| `gm list` | 1.3s | Ondo API (264 tokens) |
| `gm buy/sell` | 2–4s | Preflight + Jupiter execute |

RPC health tracking: auto-remembers the last good endpoint. 2 public Solana RPCs with fast failover. Set `RWA_RPC_URL` to a private RPC for even faster responses.

## Architecture

```
bin/rwa/              → Binary entry point
crates/cli/           → CLI parsing (clap v4), output formatting
crates/ondo/          → Solana RPC, Jupiter API, Ondo API, wallet
  src/amounts.rs      → Exact amount parsing, formatting, percent math
  src/usecases/gm.rs  → Buy/sell/close-all application flows
  src/solana/
    rpc.rs            → RPC retry + URL rotation
    fee.rs            → Priority fees + rent estimation
    transaction.rs    → Transaction building + sending
    transfer.rs       → SOL/SPL transfers + ATA management
    mod.rs            → Balances, validation, portfolio queries
  src/jupiter.rs      → Jupiter Ultra swap API
  src/api.rs          → Ondo session/pricing API
  src/wallet.rs       → Ed25519 wallet (SLIP-10, BIP39)
```

~5,500 lines of Rust, ~3 MB binary, 100+ tests. Pure Rust, no native C dependencies, no `unsafe`. Uses `rustls-tls`, `ed25519-dalek` (with zeroize), and exact amount conversion helpers shared across trade and transfer flows.

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
