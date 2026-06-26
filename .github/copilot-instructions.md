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
- Solana RPC: use `tokio::join!` for 2-3 independent calls, avoid excessive concurrency
- `rpc_call_with_retry` handles rate limit retries with backoff
- Wallet stored as JSON keypair in `~/.config/rwa/key.json`
- Solana RPC transport: `crates/ondo/src/solana/rpc.rs`
- Solana operations: `crates/ondo/src/solana/mod.rs`
- Ondo API: `crates/ondo/src/api.rs`
- Jupiter swap: `crates/ondo/src/jupiter.rs` (Ultra API on api.jup.ag)
- Jupiter gasless: MM pays gas for swaps (JupiterZ RFQ), user only needs SOL for ATA rent

## Slippage Protection

- Default slippage: 100 bps (1%) sent to Jupiter when `--slippage` not specified
- Retry: if quote shows >1% slippage, retries up to 5x with fresh quotes (cycles through different MMs)
- Hard block: >3% slippage blocked after all retries exhausted
- Close-all skips positions < $1.50 (market makers reject small swaps)

## Commands

```
rwa gm hours                       # Market status
rwa gm list                        # All 438 tokens
rwa gm buy <SYM> <AMT> --dry-run   # Preview trade (no separate quote command)
rwa gm buy/sell <SYM> <AMT> -y     # Execute trade
rwa gm close-all -y                # Sell ALL positions (sequential, skips <$1.50)
rwa gm close-all 50% -y            # Sell 50% of every position
rwa gm buy-basket SYM AMT ... -y   # Buy multiple tokens at once
rwa gm sell-basket SYM AMT ... -y  # Sell multiple tokens at once
rwa gm portfolio [WALLET]          # Holdings + P&L
rwa gm history <SYM> [-r RANGE]    # Price chart data
rwa gm send <TOKEN> <AMT> <TO> -y  # Transfer tokens
rwa gm reclaim                     # Close empty token accounts, reclaim SOL rent
rwa keys generate|import|show      # Wallet management
rwa keys encrypt|decrypt           # Wallet encryption
```

## Development

```bash
cargo build                  # Build
cargo run -- gm hours        # Test
cargo clippy                 # Lint
cargo install --path bin/rwa # Install locally
```

## When modifying Solana RPC code

- Use `tokio::join!` for 2-3 independent RPC calls, avoid excessive concurrency
- Use `rpc_call_with_retry` for any new RPC call
- USDC mint: `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v`
- GM tokens use Token-2022 program: `TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb`
