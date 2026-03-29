# RWA CLI

Rust CLI for trading tokenized stocks & ETFs (Ondo Global Markets) on Solana via Jupiter.

## Build / Test / Lint

```bash
cargo build
cargo build --release
cargo clippy --all-targets
cargo test --workspace
cargo run -- gm hours
cargo install --path bin/rwa
```

## Install / Release model

- `install.sh` is binary-first: it downloads a pre-built release asset when available
- `install.sh` falls back to `cargo install --git ...` when a release asset is unavailable
- Release assets are produced by `.github/workflows/release.yml`
- Supported release targets: Linux, macOS, Windows

## Workspace structure

- `bin/rwa/` — thin binary entry point
- `crates/cli/` — clap parsing, human/JSON output, process lock, command orchestration
- `crates/cli/src/cmd/gm/` — trade, list, portfolio, send, shared preflight helpers
- `crates/ondo/` — protocol layer: Solana RPC, Jupiter, Ondo API, wallet
- `crates/ondo/src/solana/` — RPC retry, balances, fees, transactions, transfers
- `crates/ondo/src/jupiter.rs` — Jupiter Swap V2 API
- `crates/ondo/src/api.rs` — Ondo prices, history, session limits

## Code conventions

- Use `eyre` for errors
- Keep dependencies in root `[workspace.dependencies]`
- Avoid `.unwrap()` on fallible runtime paths
- Keep the repo pure Rust; no native C dependencies
- Avoid excessive concurrent Solana RPC calls
- Wallet-changing commands must remain sequential

## Product conventions

- Both `TSLA` and `TSLAon` are accepted token symbols
- Amounts can be exact (`100`), percentage (`50%`), or `all`
- Inputs with too many decimal places must be rejected, not silently rounded
- `send` and `sell` are different actions
- There is no `quote` command; preview uses `buy/sell --dry-run`
- `close-all` is the canonical path for selling many positions

## Commands

```bash
rwa gm hours
rwa gm hours --tradable
rwa gm list
rwa gm list --search <keyword>

rwa gm buy <SYM> <AMT> --dry-run
rwa gm buy <SYM> <AMT> -y
rwa gm buy <SYM> <AMT> -y --slippage 50
rwa gm sell <SYM> <AMT> --dry-run
rwa gm sell <SYM> <AMT> -y
rwa gm close-all --dry-run
rwa gm close-all -y
rwa gm close-all 50% -y

rwa gm portfolio [WALLET]
rwa gm history <SYM> [-r RANGE]

rwa gm send <TOKEN> <AMT> <TO> --dry-run
rwa gm send <TOKEN> <AMT> <TO> -y
rwa gm reclaim
rwa gm reclaim --token <SYM>

rwa keys generate
rwa keys generate --encrypt
rwa keys import --seed-phrase|--private-key|--file
rwa keys encrypt
rwa keys decrypt
rwa keys show
```

`history` default range is `1M`.

## Trading / market behavior

- Trading sessions are ET-based: Pre-Market, Regular, Post-Market, Overnight, Closed
- `buy` and `sell` check tradability before calling Jupiter
- `close-all` skips tiny positions and non-tradable tokens
- `close-all` and basket trading must remain sequential, with 3s spacing between swaps

## Jupiter behavior

- Jupiter handles gas for swaps in many cases; users still need SOL for transfers
- Default slippage is 100 bps
- Quotes with >1% slippage are refreshed up to 3 times
- Swaps with >3% slippage are blocked
- CLI auto-retries transient swap failures; agents should not retry manually

## Wallet behavior

- Plaintext wallet: `~/.config/rwa/key.json`
- Encrypted wallet: `~/.config/rwa/key.age`
- Unix permissions should stay `0o600`
- `RWA_PASSPHRASE` can be used for scripted access to encrypted wallets

## Agent usage rules

- Always prefer `rwa --json`
- Use `-y` only for real execution
- Use `--dry-run` for large or uncertain actions
- Never run wallet-changing commands in parallel
- Use `list --search <SYM>` to check one token
- Use `hours --tradable` only when the user wants the full currently tradable set
- For full exit: `close-all -> reclaim -> send USDC all -> send SOL all`

## What not to do

- Do not add EVM code or dependencies
- Do not reintroduce a separate `quote` command without strong product reason
- Do not silently round user-entered amounts
- Do not replace `close-all` with manual multi-sell flows
- Do not let docs, skills, `llms.txt`, and CLI drift out of sync
