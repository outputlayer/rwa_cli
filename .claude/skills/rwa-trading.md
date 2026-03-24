# RWA Trading — Ondo GM Tokenized Stocks on Solana

## Install

```bash
# From source (requires Rust):
cargo install --git https://github.com/outputlayer/rwa_cli --bin rwa

# Or via install script (installs Rust if missing):
curl -fsSL https://raw.githubusercontent.com/outputlayer/rwa_cli/main/install.sh | sh
```

## Agent Install

> Copy this prompt to any AI agent:

```
Install rwa — CLI for trading tokenized stocks on Solana:
curl -fsSL https://raw.githubusercontent.com/outputlayer/rwa_cli/main/install.sh | bash

Then run: rwa --json gm hours
```

All commands support `--json` for machine-readable output and `-y` to skip confirmation on trades.

Verify:
```bash
rwa --version
```

All commands support `--json` for machine-readable output. `-y` skips confirmation on trades.

## Commands

```
rwa gm hours                          # OPEN/CLOSED + countdown
rwa gm list                           # All 264 available tokens
rwa gm quote <SYMBOL> <AMOUNT>        # Swap quote (buy)
rwa gm quote <SYMBOL> <AMOUNT> --sell # Swap quote (sell)
rwa gm buy <SYMBOL> <AMOUNT> -y       # Buy with USDC
rwa gm sell <SYMBOL> <AMOUNT> -y      # Sell for USDC
rwa gm portfolio [WALLET]             # Positions + allocation + 24h change
rwa gm history <SYMBOL> [-r RANGE]    # Price history (1D/1W/1M/3M/1Y/ALL)
rwa keys generate                     # New wallet
rwa keys import --seed-phrase "..."   # Import wallet
rwa keys show                         # Address + key file path
```

`--json` on any command for machine-readable output. Amount: number, `50%`, or `all`.

## Workflow

1. `rwa gm hours` — check market (Sun 8pm – Fri 8pm ET)
2. `rwa gm list` or `rwa --json gm list` — discover tokens
3. `rwa gm quote SYMBOL AMOUNT` — preview before trade
4. `rwa gm buy/sell SYMBOL AMOUNT -y` — execute

## Token Discovery

264 tokenized stocks & ETFs. Use `rwa --json gm list` to get all symbols with names and Solana mint addresses. Both `TSLA` and `TSLAon` formats accepted.

## Key JSON Outputs

```json
// rwa --json gm list → array of all tokens
[{"symbol":"TSLAon","name":"Tesla","mint":"..."},...]

// rwa --json gm portfolio
{"wallet":"...","sol":1.5,"usdc":500.0,"positions":[{"token":"TSLAon","balance":0.26,"price":385.0,"value_usd":100.1,"alloc_pct":15.2,"change_pct_24h":1.2}],"total_value_usd":600.1}

// rwa --json gm history TSLAon -r 1M
{"symbol":"TSLAON","range":"1M","candles":527,"first":{"timestamp":...,"price":407.04},"last":{"timestamp":...,"price":385.75},"high":419.42,"low":356.83,"change_pct":-5.23}
```

Fund wallet with SOL (gas) + USDC (trading) before first trade.
