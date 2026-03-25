# RWA CLI

Rust CLI for trading tokenized stocks & ETFs (Ondo Global Markets) on Solana via Jupiter.

## Build / Test / Lint

```bash
cargo build                  # Build all crates
cargo build --release        # Release build (LTO, strip → ~3.5 MB)
cargo clippy --all-targets   # Lint (must pass with 0 warnings)
cargo run -- gm hours        # Quick smoke test
cargo install --path bin/rwa # Install locally
```

## Project Structure

- `bin/rwa/` — Binary entry point (thin — just calls `rwa_cli::run()`)
- `crates/cli/` — CLI layer: clap v4 derive commands, output formatting, `--json` flag
- `crates/cli/src/cmd/gm.rs` — All GM command implementations (~1340 lines)
- `crates/ondo/` — Protocol layer: Solana RPC, Jupiter API, Ondo API, wallet
- `crates/ondo/src/solana.rs` — All Solana RPC calls (retry + URL rotation)
- `crates/ondo/src/jupiter.rs` — Jupiter Ultra swap API
- `crates/ondo/src/api.rs` — Ondo API (prices, sectors, history)

## Code Conventions

- **Error handling**: `eyre` everywhere. No `thiserror`, no `anyhow`, no `.unwrap()` on fallible ops.
- **Dependencies**: centralized `[workspace.dependencies]` in root Cargo.toml. Add versions there, reference with `.workspace = true` in crate Cargo.toml.
- **Solana RPC**: NEVER fire concurrent RPC calls — sequential only, via `rpc_call_with_retry`. Public endpoints rate-limit at ~10 req/s.
- **RPC rotation**: 5 fallback URLs in `RPC_URLS` (publicnode, extrnode, solana, ankr, drpc). User can override with `--rpc-url` or `RWA_RPC_URL` env var.
- **Token symbols**: both `TSLA` and `TSLAon` accepted — resolved in `gm::resolve_token`.
- **Amounts**: exact number (`100`), percentage (`50%`), or `all`.
- **HTTP**: `reqwest` with `rustls-tls` only. No native TLS, no OpenSSL.
- **Wallet**: JSON keypair at `~/.config/rwa/id.json`. Permissions `0o600` enforced on Unix.

## Key Constants

- USDC mint: `EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v`
- Token-2022 program: `TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb`
- GM tokens use Token-2022 (not Token program)

## What NOT to Do

- Don't add EVM/alloy/ethers — this is Solana-only
- Don't fire concurrent Solana RPC calls (use sequential + retry)
- Don't add native C deps — keep pure Rust for cross-platform
- Don't use `.unwrap()` — use `?` with eyre context

## Commands

```
rwa gm hours                       # Current trading session + tradable count
rwa gm hours --tradable            # List all tradable tokens in current session
rwa gm list                        # All 264 tokens (with tradable status)
rwa gm list --search <keyword>     # Search tokens (includes tradable field)
rwa gm quote <SYM> <AMT>          # Swap quote
rwa gm buy <SYM> <AMT> -y         # Buy token
rwa gm buy <SYM> <AMT> -y --slippage 50  # Buy with max 0.5% slippage
rwa gm sell <SYM> <AMT> -y        # Sell token
rwa gm close-all -y               # Sell ALL positions (sequential, skips <$1)
rwa gm close-all 50% -y           # Sell 50% of every position
rwa gm portfolio [WALLET]          # Holdings + P&L
rwa gm history <SYM> [-r RANGE]   # Price chart data
rwa gm send <TOKEN> <AMT> <TO> -y # Transfer tokens
rwa gm reclaim                    # Close empty token accounts, reclaim SOL rent
rwa gm reclaim --token <SYM>      # Reclaim only for a specific token
rwa keys generate|import|show      # Wallet management
```

## Trading Sessions (ET)

- **Pre-Market**: 4:00 AM – 9:29 AM
- **Regular**: 9:30 AM – 3:59 PM
- **Post-Market**: 4:00 PM – 7:59 PM
- **Overnight**: 8:00 PM – 3:59 AM
- **Closed**: Fri 8 PM – Sun 8 PM

Not all tokens are tradable in every session. `list` and `hours --tradable` show which tokens can be traded now.

## Close-All Limits

- Positions < $1 are skipped automatically (Jupiter rejects small swaps)
- Skipped tokens are listed separately in JSON (`skipped` array with `reason`)

## Agent Usage Rules (for AI agents running `rwa` commands)

**CRITICAL — NEVER run rwa commands in parallel (`&`).** Jupiter API rejects concurrent requests from the same wallet (HTTP 400, "Failed to get quotes"). Always run one command at a time, sequentially. This applies to ALL commands: quotes, trades, history, portfolio.

### Command execution
- Do NOT prepend `export PATH=...` to every command. `rwa` is in PATH after install.
- Run commands **one at a time**: `rwa gm buy TSLA 100 -y && sleep 5 && rwa gm buy AAPL 100 -y`
- Add `sleep 3` between consecutive commands
- Always use `--json` flag and `-y` for buy/sell
- **Quote requires amount**: `rwa --json gm quote <SYMBOL> <AMOUNT>` — amount is mandatory
- **To sell all positions**: use `rwa --json gm close-all -y` — do NOT sell each token manually
- **To reduce all positions**: use `rwa --json gm close-all 50% -y` — sells given % of every position
- **send ≠ sell**: `send` transfers tokens/USDC to another wallet, `sell` swaps for USDC
- **NEVER use `send USDC all`** when user wants to send just sell proceeds — use exact amount from sell result
- **After selling all positions**: run `rwa --json gm reclaim` to close empty token accounts and reclaim SOL rent

### Token search
- **Always** use `rwa --json gm list --search <keyword>` to filter tokens (searches symbol, name, and sector)
- Each result includes `tradable: true/false` for the current session
- To check if a specific token is tradable: look at the `tradable` field in search results
- To get all tradable tokens: `rwa --json gm hours --tradable`
- Sectors: Technology, Healthcare, Financials, Consumer Discretionary, Energy, Industrials, Materials, Utilities, Real Estate, Infrastructure

### Error handling
- With `--json`, errors return `{"status":"error","error":"..."}` on stdout (exit code 1)
- CLI auto-retries swap errors (max 2 retries with fresh orders). Do NOT retry manually after a swap error.
- "Solana RPC unavailable" → wait **at least 5 seconds** before retry. After 3 failures, stop and ask user to set `RWA_RPC_URL`
- "No swap route found" / "HTTP 400" → you are running commands in parallel. Stop. Run sequentially
- "HTTP 429" → rate limited. Wait 5s, retry
