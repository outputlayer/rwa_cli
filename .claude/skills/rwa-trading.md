# RWA Trading — Tokenized Stocks on Solana

Trade 264 tokenized stocks & ETFs (Ondo Global Markets) on Solana via Jupiter.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/outputlayer/rwa_cli/main/install.sh | bash
```

## Trading Workflow

1. Check market hours (24/5, Sun 8pm – Fri 8pm ET):
```bash
rwa --json gm hours
```

2. Discover tokens:
```bash
rwa --json gm list
```

3. Get a quote before trading:
```bash
rwa --json gm quote TSLA 100          # buy quote
rwa --json gm quote TSLA 5 --sell     # sell quote
```

4. Execute trade:
```bash
rwa gm buy TSLA 100 -y               # buy with USDC
rwa gm sell TSLA all -y              # sell all holdings
```

5. Check portfolio:
```bash
rwa --json gm portfolio              # own wallet
rwa --json gm portfolio <ADDRESS>    # any wallet
```

## Flags

- `--json` — machine-readable output (use on every command for agent workflows)
- `-y` — skip confirmation on buy/sell (required for non-interactive agents)

## Amount Formats

- Exact: `100` (USDC for buy, tokens for sell)
- Percentage: `50%` (half of balance)
- All: `all` (entire balance)

## Key JSON Outputs

```json
// rwa --json gm hours
{"status":"OPEN","next_close":"2026-03-28T00:00:00Z","countdown":"2d 15h 30m"}

// rwa --json gm portfolio
{"wallet":"...","sol":1.5,"usdc":500.0,"positions":[{"token":"TSLAon","balance":0.26,"price":385.0,"value_usd":100.1,"alloc_pct":15.2,"change_pct_24h":1.2}],"total_value_usd":600.1}

// rwa --json gm list
[{"symbol":"TSLAon","name":"Tesla","mint":"..."},...]
```

## Notes

- Both `TSLA` and `TSLAon` symbol formats accepted
- Fund wallet with SOL (gas) + USDC (trading) before first trade
- If RPC errors occur, retry or set `RWA_RPC_URL` to a private endpoint
