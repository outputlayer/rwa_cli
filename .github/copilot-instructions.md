# RWA CLI — Copilot Instructions

Rust CLI for trading tokenized stocks & ETFs (Ondo Global Markets) on Solana via Jupiter.

## Architecture

Cargo workspace: `bin/rwa` (entry point) → `crates/cli` (clap parsing, output) → `crates/ondo` (protocol: Solana RPC, Jupiter API, Ondo API, wallet).

- **Solana-only** — no EVM, no ethers, no alloy
- **clap v4 derive** — `--json` flag for machine-readable output on every command
- **tokio** async runtime, **reqwest** (rustls-tls) for HTTP
- **eyre** for all error handling (no thiserror, no anyhow)
- Centralized `[workspace.dependencies]` in root Cargo.toml

## Key conventions

- Token symbols: both `TSLA` and `TSLAon` accepted (resolved in `gm::resolve_token`)
- Amounts: exact number (`100`), percentage (`50%`), or `all`
- Solana RPC calls are sequential (not concurrent) to avoid 429 rate limits on public endpoints
- `rpc_call_with_retry` handles rate limit retries with backoff
- Wallet stored as JSON keypair in `~/.config/rwa/id.json`
- All HTTP to Solana goes through `crates/ondo/src/solana.rs`
- All HTTP to Ondo goes through `crates/ondo/src/api.rs`
- Jupiter swap via `crates/ondo/src/jupiter.rs`

## Commands

```
rwa gm hours                       # Market status
rwa gm list                        # All 264 tokens
rwa gm quote <SYM> <AMT>           # Swap quote
rwa gm buy/sell <SYM> <AMT> -y     # Execute trade
rwa gm buy <SYM> <AMT> -y --slippage 50  # Trade with max 0.5% slippage
rwa gm close-all -y                # Sell ALL positions (sequential, skips failures)
rwa gm close-all 50% -y            # Sell 50% of every position
rwa gm portfolio [WALLET]          # Holdings + P&L
rwa gm history <SYM> [-r RANGE]    # Price chart data
rwa gm send <TOKEN> <AMT> <TO> -y  # Transfer tokens
rwa gm reclaim                     # Close empty token accounts, reclaim SOL rent
rwa keys generate|import|show      # Wallet management
```

## Development

```bash
cargo build                  # Build
cargo run -- gm hours        # Test
cargo clippy                 # Lint
cargo install --path bin/rwa # Install locally
```

## When modifying Solana RPC code

- Never fire multiple Solana RPC calls concurrently — use sequential with shared `reqwest::Client`
- Use `rpc_call_with_retry` for any new RPC call
- USDC mint: `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v`
- GM tokens use Token-2022 program: `TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb`
