# RWA Trading — Ondo GM Tokenized Stocks on Solana

## CLI Commands

```
rwa gm hours                          # OPEN/CLOSED + countdown
rwa gm quote <SYMBOL> <USDC_AMOUNT>   # Jupiter swap quote (buy)
rwa gm quote <SYMBOL> <AMOUNT> --sell # Jupiter swap quote (sell)
rwa gm buy <SYMBOL> <AMOUNT> -y       # Execute buy (USDC -> token)
rwa gm sell <SYMBOL> <AMOUNT> -y      # Execute sell (token -> USDC)
rwa gm portfolio [WALLET]             # Positions + PnL (default: local wallet)
rwa gm history <SYMBOL> [-r RANGE]    # Price history (1D, 1W, 1M, 3M, 1Y, ALL)
```

Add `--json` for machine-readable output on any command.

Amount accepts: exact number (`100`), percentage (`50%`), or `all`.

## Workflow

1. `rwa gm hours` — if CLOSED, stop. Trading: Sun 8pm – Fri 8pm ET (24/5)
2. `rwa gm quote SYMBOL AMOUNT` — check price/slippage before executing
3. `rwa gm buy/sell SYMBOL AMOUNT -y` — execute trade

For multiple trades: call `hours` once, then `buy`/`sell` sequentially with `-y`.

## Analysis Workflow

Use `rwa gm history` to analyze price trends before trading:
- `rwa --json gm history TSLA -r 1D` — intraday (minute candles)
- `rwa --json gm history TSLA -r 1W` — weekly (minute candles)
- `rwa --json gm history TSLA -r 1M` — monthly (hourly candles)
- `rwa --json gm history TSLA -r 1Y` — yearly (daily candles)

Returns: candle count, open, close, high, low, change_pct.

## Popular Tokens (symbol -> company)

TSLA=Tesla, AAPL=Apple, NVDA=Nvidia, AMZN=Amazon, GOOGL=Google, META=Meta,
MSFT=Microsoft, NFLX=Netflix, AMD=AMD, COIN=Coinbase, MSTR=MicroStrategy,
JPM=JPMorgan, GS=Goldman Sachs, BAC=BankOfAmerica, V=Visa, MA=Mastercard,
DIS=Disney, NKE=Nike, KO=Coca-Cola, PEP=Pepsi, MCD=McDonald's,
UBER=Uber, SHOP=Shopify, PLTR=Palantir, HOOD=Robinhood, SOFI=SoFi,
RIVN=Rivian, NIO=NIO, LI=Li Auto, GRAB=Grab, MELI=MercadoLibre,
SPY=S&P500 ETF, QQQ=Nasdaq ETF, IVV=S&P500 ETF, VTI=Total Market ETF,
IBIT=Bitcoin ETF, GLD=Gold ETF, SLV=Silver ETF, TLT=20Y Treasury ETF,
USO=Oil ETF, URA=Uranium ETF, SOXX=Semiconductor ETF, EEM=Emerging Markets

All 264 symbols use format: `SYMBOLon` (e.g. TSLAon, AAPLon, SPYon).
Both `TSLA` and `TSLAon` are accepted.

## JSON Output Examples

```json
// rwa --json gm hours
{"status":"open","now":"Tuesday 06:00 PM ET","countdown":"closes in 74h 0m"}

// rwa --json gm quote TSLAon 100
{"input":"100.00","input_token":"USDC","output":"0.2594","output_token":"TSLAon","input_usd":100.0,"output_usd":99.85,"slippage_pct":-0.15}

// rwa --json gm buy TSLAon 100 -y
{"status":"success","amount":"0.2594","token":"TSLAon","counter_amount":"100.00","counter_token":"USDC","tx":"https://solscan.io/tx/..."}

// rwa --json gm portfolio
{"wallet":"...","sol":1.5,"usdc":500.0,"positions":[{"token":"TSLAon","balance":0.26,"price":385.0,"value_usd":100.1,"change_pct_24h":1.2}],"total_value_usd":600.1,"change_24h_usd":5.2,"change_24h_pct":0.87}

// rwa --json gm history TSLAon -r 1M
{"symbol":"TSLAON","range":"1M","candles":527,"first":{"timestamp":1771712400000,"price":407.04},"last":{"timestamp":1774390500000,"price":385.75},"high":419.42,"low":356.83,"change_pct":-5.23}
```

## Wallet

```
rwa keys generate                          # New wallet
rwa keys import --seed-phrase "word1 ..."  # From seed
rwa keys import --private-key <BASE58>     # From key
rwa keys show                              # Show address + key file path
```

Key file location: `~/.config/rwa/key.json` (macOS/Linux)

Fund wallet with SOL (gas) + USDC (trading) before first trade.
