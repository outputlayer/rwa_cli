# RWA CLI

A Rust CLI for trading tokenized stocks & ETFs (Ondo Global Markets) on **Solana** via Jupiter.

## Project Structure

```
rwa-cli/
├── bin/rwa/           # Binary entry point (thin — just calls rwa_cli::run())
├── crates/
│   ├── cli/           # CLI layer: clap commands, output formatting
│   │   └── src/cmd/   # Command implementations (gm.rs, keys.rs)
│   └── ondo/          # Ondo protocol: token list, Jupiter, Solana RPC, wallet
│       └── src/       # api.rs, gm.rs, jupiter.rs, solana.rs, token_list.rs, wallet.rs
├── .claude/skills/    # Agent skills (rwa-trading.md)
├── .github/           # copilot-instructions.md
├── install.sh         # Cross-platform installer (installs Rust if missing)
├── Cargo.toml         # Workspace root with centralized dependencies
└── CLAUDE.md          # This file
```

## Architecture

- **Cargo workspace** with centralized `[workspace.dependencies]` — all versions in root Cargo.toml
- **Solana-only**: all trading via Jupiter Ultra API, balances via Solana RPC
- **Ondo API** for asset data, price history (no on-chain EVM calls)
- **clap v4 derive** for CLI parsing, `--json` flag for agent-friendly output
- **tokio** async runtime, **reqwest** (rustls) for HTTP

## Commands

```bash
rwa gm hours                          # Check market status (OPEN/CLOSED)
rwa gm quote TSLAon 100               # Get swap quote (USDC -> token)
rwa gm quote TSLAon 5 --sell          # Get swap quote (token -> USDC)
rwa gm buy TSLAon 100 -y              # Buy with USDC
rwa gm sell TSLAon all -y             # Sell all holdings
rwa gm portfolio                      # Portfolio (local wallet)
rwa gm portfolio <WALLET>             # Portfolio (any address)
rwa gm history TSLAon                 # Price history (default 1M)
rwa gm history TSLAon -r 1W           # Price history (1D/1W/1M/3M/1Y/ALL)
rwa gm list                           # List all 264 available tokens
rwa --json gm hours                   # JSON output for any command
rwa keys generate                     # Generate wallet
rwa keys show                         # Show address + key file path
```

## Ondo GM Tokens

- 264 tokenized stocks/ETFs (AAPLon, TSLAon, NVDAon, SPYon, etc.)
- **Total return trackers** — dividends reinvested, 1 token ≠ 1 share
- Trading via Jupiter (Solana): 24/5, Sun 8pm — Fri 8pm ET
- Both "TSLA" and "TSLAon" accepted as symbol format

## Development

```bash
cargo build                  # Build all crates
cargo run -- gm hours        # Run CLI
cargo install --path bin/rwa # Install to ~/.cargo/bin
```

## Installation

```bash
# From source (requires Rust):
cargo install --git https://github.com/user/rwa_cli --bin rwa

# Or via install script:
curl -fsSL https://raw.githubusercontent.com/user/rwa_cli/main/install.sh | sh
```

## Conventions

- Token symbols accept both "TSLA" and "TSLAon" formats
- Amount accepts: exact (`100`), percentage (`50%`), or `all`
- `--json` flag on any command for machine-readable output
- `-y` flag skips confirmation on buy/sell
- Error handling: `eyre` for all error types
- Solana RPC calls must be sequential (not concurrent) — public endpoints rate-limit aggressively
- `rpc_call_with_retry` handles 429 retries with exponential backoff
- Portfolio uses `get_portfolio_balances()` — sequential RPC with shared HTTP client

## Cross-Platform

- macOS + Linux: fully supported
- Windows: works via WSL 2 or Git Bash; wallet file permissions (`0o600`) only enforced on Unix
- Pure Rust dependencies: `rustls-tls`, `ed25519-dalek`, `bip39` — no native C deps
