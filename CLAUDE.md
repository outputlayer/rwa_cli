# RWA CLI

A Rust CLI tool for interacting with Real World Asset (RWA) protocols on-chain. Currently focused on **Ondo Global Markets (GM) tokens on BNB Chain**.

## Project Structure

```
rwa-cli/
├── bin/rwa/           # Binary entry point (thin — just calls rwa_cli::run())
├── crates/
│   ├── cli/           # CLI layer: clap commands, output formatting
│   │   └── src/cmd/   # Subcommand implementations (gm.rs, etc.)
│   ├── core/          # Shared types, config, contract ABIs, chain definitions
│   └── ondo/          # Ondo protocol provider: GM tokens, oracle, token list
├── Cargo.toml         # Workspace root with centralized dependencies
└── CLAUDE.md          # This file
```

## Architecture

- **Cargo workspace** with centralized `[workspace.dependencies]` — all versions in root Cargo.toml
- **alloy** stack (not ethers-rs) for all EVM interaction: `alloy-provider`, `alloy-contract`, `alloy-sol-types`, `alloy-primitives`
- **clap v4 derive** for CLI parsing
- **tokio** async runtime, **reqwest** (rustls) for HTTP
- Contract interfaces defined via `sol!` macro in `crates/core/src/contracts.rs`

## Key Commands

```bash
rwa gm list                         # List available GM tokens
rwa gm price TSLAon                 # Oracle price data
rwa gm balance <wallet>             # All GM balances
rwa gm balance <wallet> -t TSLA     # Specific token balance
rwa gm info TSLAon                  # Detailed token info
```

## Ondo GM on BNB Chain — Key Contracts

| Contract | Address |
|---|---|
| GMTokenManager | `0x91f8Aff3738825e8eB16FC6f6b1A7A4647bDB299` |
| USDon | `0x1f8955E640Cbd9abc3C3Bb408c9E2E1f5F20DfE6` |
| SyntheticSharesOracle | `0xF4Fd8a1B412633e10527454137A29Db7Aa35F15e` |
| GMTokenLimitOrder | `0x96b525B1a93f31E65F4aAf18C53842eD28525D48` |
| OndoIDRegistry | `0x898128f9f22c0192da0c5acd394d9eeac461d911` |

## How GM Tokens Work

- 264 tokenized stocks/ETFs (AAPLon, TSLAon, NVDAon, SPYon, etc.)
- All ERC-20, 18 decimals, beacon proxy pattern (shared implementation)
- **Total return trackers** — dividends reinvested, so 1 token ≠ 1 share (diverges over time)
- Pricing via `SyntheticSharesOracle.assetData(token)` → returns `sharesPerToken` (18 decimals)
- Mint via `GMTokenManager.subscribe(token, usdOnAmount, minTokensOut)` — atomic, instant
- Payment in USDon (1:1 USDC conversion via stablecoin swapper)
- Contracts are NOT verified on BscScan, ABIs reconstructed from on-chain analysis

## Development

```bash
cargo build                  # Build all crates
cargo run -- gm list         # Run CLI
cargo run -- --rpc-url <URL> gm price TSLAon  # Custom RPC
RWA_RPC_URL=<URL> cargo run -- gm price TSLAon  # Via env var
```

## Adding New Protocols

1. Create a new crate under `crates/` (e.g., `crates/maple/`)
2. Add contract ABIs to `crates/core/src/contracts.rs` or protocol-specific file
3. Add new subcommand in `crates/cli/src/cmd/`
4. Register the subcommand in `crates/cli/src/lib.rs` Commands enum

## Conventions

- Use `alloy` for all EVM interaction, never ethers-rs
- Contract addresses as `const Address` in `crates/core/src/contracts.rs`
- ABI bindings via `sol!` macro with `#[sol(rpc)]`
- Token symbols accept both "TSLA" and "TSLAon" formats
- Default chain: BNB Mainnet (chain ID 56)
- Error handling: `eyre` in binary/CLI, `thiserror` in libraries
