mod basket;
mod close_all;
mod helpers;
mod list;
mod pnl;
mod portfolio;
mod reclaim;
mod send;
mod trade;
mod types;

use clap::Subcommand;
use eyre::Result;
use rwa_ondo::usecases;

use helpers::*;

/// Resolve `RWA_MAX_BPS` from its raw env value: a valid `u32`, else `None`.
/// CLI value, falling back to the `RWA_MAX_BPS` env default — the single
/// chokepoint so a new trading subcommand can't forget the env fallback.
fn effective_max_bps(cli: Option<u32>) -> Option<u32> {
    cli.or_else(|| parse_max_bps_env(std::env::var("RWA_MAX_BPS").ok()))
}

fn parse_max_bps_env(raw: Option<String>) -> Option<u32> {
    raw.and_then(|s| s.trim().parse::<u32>().ok())
}
pub(super) use types::{
    BuyBasketItemJson, BuyBasketResultJson, CloseAllResultJson, CloseFailJson, CloseItemJson,
    CloseSkipJson, GasRefuelJson, HistoryCandleJson, HistoryJson, HoursJson, ListItemJson, PortfolioCashJson,
    PnlJson, PnlTokenJson, PnlTotalsJson,
    PortfolioGmPositionsJson, PortfolioJson, PortfolioUnavailableJson, PositionJson, ReclaimJson,
    SellBasketItemJson, SellBasketResultJson, SendJson, TradeJson, TradableItemJson,
    TradableResultJson,
};

// ── Subcommand enum ────────────────────────────────────────

#[derive(Subcommand, Debug)]
pub enum GmAction {
    /// Check market status, current session, and tradable tokens
    Hours {
        /// Show full list of tradable tokens (omitted by default in --json)
        #[arg(long)]
        tradable: bool,
    },

    /// Buy GM token with USDC via Jupiter (Solana)
    Buy {
        /// Token symbol (e.g. TSLA, TSLAon)
        symbol: String,
        /// USDC amount, percentage of balance, or "all" (e.g. 100, 50%, all)
        amount: String,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
        /// Max slippage in basis points (e.g. 50 = 0.5%). Default: 100 (1%)
        #[arg(long)]
        slippage: Option<u32>,
        /// Show quote and validate without executing the swap
        #[arg(long)]
        dry_run: bool,
        /// Preview a quote for any size, skipping the balance check (implies dry-run, never executes; a wallet is still required as the swap taker)
        #[arg(long, conflicts_with = "yes")]
        quote_only: bool,
        /// Reject the trade if quoted all-in cost (spread + fee) exceeds this many bps. Overrides RWA_MAX_BPS.
        #[arg(long)]
        max_bps: Option<u32>,
        /// Execute only if the quoted price (USDC per token) is at or better
        /// than this limit (buy: <=). Otherwise fails with condition_not_met.
        #[arg(long, conflicts_with = "quote_only")]
        limit_price: Option<String>,
    },

    /// Sell GM token for USDC via Jupiter (Solana)
    Sell {
        /// Token symbol (e.g. TSLA, TSLAon)
        symbol: String,
        /// Token amount, percentage of holdings, or "all" (e.g. 5, 50%, all)
        amount: String,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
        /// Max slippage in basis points (e.g. 50 = 0.5%). Default: 100 (1%)
        #[arg(long)]
        slippage: Option<u32>,
        /// Show quote and validate without executing the swap
        #[arg(long)]
        dry_run: bool,
        /// Reject the trade if quoted all-in cost (spread + fee) exceeds this many bps. Overrides RWA_MAX_BPS.
        #[arg(long)]
        max_bps: Option<u32>,
        /// Execute only if the quoted price (USDC per token) is at or better
        /// than this limit (sell: >=). Otherwise fails with condition_not_met.
        #[arg(long)]
        limit_price: Option<String>,
    },

    /// Portfolio positions and 24h change (Solana); for entry prices/P&L use `pnl`
    Portfolio {
        /// Wallet address (default: local wallet)
        wallet: Option<String>,
    },

    /// Price history for a GM token (1D, 1W, 1M, 3M, 1Y, ALL)
    History {
        /// Token symbol (e.g. TSLA, TSLAon)
        symbol: String,
        /// Time range: 1D, 1W, 1M, 3M, 1Y, ALL (default: 1M)
        #[arg(short, long, default_value = "1M")]
        range: String,
    },

    /// List all available GM tokens with tradable status
    List,

    /// Search/filter GM tokens in bulk without external scripts
    Search {
        /// Filter tokens by keyword (repeatable; searches symbol, name, and sector)
        #[arg(short, long)]
        search: Vec<String>,
        /// Only include tokens tradable in the current session
        #[arg(long)]
        tradable_only: bool,
        /// Filter by sector (repeatable)
        #[arg(long = "sector")]
        sectors: Vec<String>,
        /// Filter by token type (`stock` or `etf`)
        #[arg(long = "type")]
        kind: Option<String>,
        /// Filter by company-name keyword (repeatable)
        #[arg(long = "name-keyword")]
        name_keywords: Vec<String>,
        /// Filter by any Ondo tag: asset class, region, factor/risk
        /// (e.g. --tag asia, --tag dividend, --tag "fixed income"; repeatable)
        #[arg(long = "tag")]
        tags: Vec<String>,
    },

    /// Check tradable status for one or more symbols, or list all tradable tokens
    Tradable {
        /// Symbols to check (e.g. TSLA TSLAon NVDA); omit to list all tradable tokens
        #[arg(num_args = 0..)]
        symbols: Vec<String>,
    },

    /// Send SOL, USDC, or GM tokens to another wallet
    Send {
        /// What to send: SOL, USDC, or token symbol (e.g. TSLA)
        token: String,
        /// Amount to send (e.g. 100, 50%, all)
        amount: String,
        /// Recipient Solana address
        to: String,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
        /// Validate and show transfer details without sending
        #[arg(long)]
        dry_run: bool,
    },

    /// Close all GM positions — sell every token for USDC (parallel by default)
    CloseAll {
        /// Percentage of each position to sell (e.g. 10%, 50%). Sells 100% if omitted.
        amount: Option<String>,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
        /// Show what would be sold without executing
        #[arg(long)]
        dry_run: bool,
        /// Parallel execution is the default; this flag is a no-op kept for compatibility
        #[arg(long, conflicts_with = "sequential")]
        parallel: bool,
        /// Execute swaps one at a time with 3s spacing (rate-limit-friendly fallback)
        #[arg(long)]
        sequential: bool,
        /// Max slippage in basis points (e.g. 50 = 0.5%). Default: 100 (1%)
        #[arg(long)]
        slippage: Option<u32>,
        /// Skip positions whose quoted all-in cost (spread + fee) exceeds this many bps
        #[arg(long)]
        max_bps: Option<u32>,
    },

    /// Cost basis and P&L from your CLI trades (average entry, realized/unrealized)
    Pnl,

    /// Close empty token accounts and reclaim SOL rent
    Reclaim {
        /// Only reclaim for a specific token (symbol or mint address)
        #[arg(long)]
        token: Option<String>,
    },

    /// Buy multiple GM tokens with USDC in one go (basket buy).
    /// Syntax: SYMBOL AMOUNT [SYMBOL AMOUNT ...], e.g. AAPL 4 TSLA 3 NVDA 5
    BuyBasket {
        /// Alternating symbol/amount pairs: AAPL 4 TSLA 3 NVDA 5
        #[arg(required = true, num_args = 2..)]
        tokens: Vec<String>,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
        /// Show quotes without executing
        #[arg(long)]
        dry_run: bool,
        /// Parallel execution is the default; this flag is a no-op kept for compatibility
        #[arg(long, conflicts_with = "sequential")]
        parallel: bool,
        /// Execute swaps one at a time with 3s spacing (rate-limit-friendly fallback)
        #[arg(long)]
        sequential: bool,
        /// Max slippage in basis points (e.g. 50 = 0.5%). Default: 100 (1%)
        #[arg(long)]
        slippage: Option<u32>,
        /// Reject items whose quoted all-in cost (spread + fee) exceeds this many bps
        #[arg(long)]
        max_bps: Option<u32>,
    },

    /// Sell multiple GM tokens for USDC in one go (basket sell).
    /// Syntax: SYMBOL AMOUNT [SYMBOL AMOUNT ...], e.g. SPY 5 TSLA 3 NVDA all
    SellBasket {
        /// Alternating symbol/amount pairs: SPY 5 TSLA 3 NVDA all
        #[arg(required = true, num_args = 2..)]
        tokens: Vec<String>,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
        /// Show quotes without executing
        #[arg(long)]
        dry_run: bool,
        /// Parallel execution is the default; this flag is a no-op kept for compatibility
        #[arg(long, conflicts_with = "sequential")]
        parallel: bool,
        /// Execute swaps one at a time with 3s spacing (rate-limit-friendly fallback)
        #[arg(long)]
        sequential: bool,
        /// Max slippage in basis points (e.g. 50 = 0.5%). Default: 100 (1%)
        #[arg(long)]
        slippage: Option<u32>,
        /// Reject items whose quoted all-in cost (spread + fee) exceeds this many bps
        #[arg(long)]
        max_bps: Option<u32>,
    },
}

pub async fn execute(action: GmAction, json: bool, rpc_url: Option<&str>, selected: Option<&str>) -> Result<()> {
    match action {
        GmAction::Hours { tradable } => list::hours(json, tradable).await,
        GmAction::Buy { symbol, amount, yes, slippage, dry_run, quote_only, max_bps, limit_price } => {
            let max_bps = effective_max_bps(max_bps);
            trade::buy(
                &symbol,
                &amount,
                yes,
                dry_run,
                json,
                rpc_url,
                Some(slippage.unwrap_or(usecases::gm::DEFAULT_SLIPPAGE_BPS)),
                quote_only,
                max_bps,
                limit_price.as_deref(),
                selected,
            )
            .await
        }
        GmAction::Sell { symbol, amount, yes, slippage, dry_run, max_bps, limit_price } => {
            let max_bps = effective_max_bps(max_bps);
            trade::sell(&symbol, &amount, yes, dry_run, json, rpc_url, slippage, max_bps, limit_price.as_deref(), selected).await
        }
        GmAction::Portfolio { wallet } => {
            portfolio::portfolio(wallet.as_deref(), json, rpc_url, selected).await
        }
        GmAction::History { symbol, range } => portfolio::history(&symbol, &range, json).await,
        GmAction::List => list::list(json).await,
        GmAction::Search {
            search,
            tradable_only,
            sectors,
            kind,
            name_keywords,
            tags,
        } => list::search(
            json,
            &search,
            tradable_only,
            &sectors,
            kind.as_deref(),
            &name_keywords,
            &tags,
        )
        .await,
        GmAction::Tradable { symbols } => list::tradable(json, &symbols).await,
        GmAction::Send {
            token,
            amount,
            to,
            yes,
            dry_run,
        } => send::send(&token, &amount, &to, yes, dry_run, json, rpc_url, selected).await,
        GmAction::CloseAll {
            amount,
            yes,
            dry_run,
            parallel: _,
            sequential,
            slippage,
            max_bps,
        } => {
            let max_bps = effective_max_bps(max_bps);
            // Parallel is the default (bounded by the order/execute semaphores);
            // --sequential opts into one-at-a-time with 3s spacing.
            close_all::close_all(amount.as_deref(), yes, dry_run, !sequential, json, rpc_url, slippage, max_bps, selected).await
        }
        GmAction::Pnl => pnl::pnl(json, selected).await,
        GmAction::Reclaim { token } => reclaim::reclaim(token.as_deref(), json, rpc_url, selected).await,
        GmAction::BuyBasket { tokens, yes, dry_run, parallel: _, sequential, slippage, max_bps } => {
            let max_bps = effective_max_bps(max_bps);
            basket::buy_basket(&tokens, yes, dry_run, !sequential, json, rpc_url, slippage, max_bps, selected).await
        }
        GmAction::SellBasket { tokens, yes, dry_run, parallel: _, sequential, slippage, max_bps } => {
            let max_bps = effective_max_bps(max_bps);
            basket::sell_basket(&tokens, yes, dry_run, !sequential, json, rpc_url, slippage, max_bps, selected).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use clap::Parser;

    #[test]
    fn parse_max_bps_env_reads_valid_u32_only() {
        assert_eq!(parse_max_bps_env(Some("50".to_string())), Some(50));
        assert_eq!(parse_max_bps_env(Some("  75 ".to_string())), Some(75));
        assert_eq!(parse_max_bps_env(Some("abc".to_string())), None);
        assert_eq!(parse_max_bps_env(None), None);
    }

    #[test]
    fn quote_only_conflicts_with_yes_at_parse_time() {
        let res = crate::Cli::try_parse_from([
            "rwa", "gm", "buy", "TSLA", "100", "--quote-only", "--yes",
        ]);
        assert!(res.is_err(), "--quote-only with -y must be rejected by clap");
    }

    #[test]
    fn basket_and_close_all_accept_slippage_and_max_bps() {
        for cmd in [
            ["rwa", "gm", "close-all", "--slippage", "50", "--max-bps", "30"].as_slice(),
            ["rwa", "gm", "buy-basket", "AAPL", "10", "--slippage", "50", "--max-bps", "30"].as_slice(),
            ["rwa", "gm", "sell-basket", "SPY", "5", "--slippage", "50", "--max-bps", "30"].as_slice(),
        ] {
            let res = crate::Cli::try_parse_from(cmd);
            assert!(res.is_ok(), "should parse {cmd:?}: {:?}", res.err());
        }
    }

    #[test]
    fn quote_only_alone_parses() {
        let res = crate::Cli::try_parse_from([
            "rwa", "gm", "buy", "TSLA", "100", "--quote-only",
        ]);
        assert!(res.is_ok(), "--quote-only alone should parse");
    }

    #[test]
    fn portfolio_json_serializes_nested_cash_and_gm_positions() {
        let json = serde_json::to_value(PortfolioJson {
            wallet: "wallet123".into(),
            cash: PortfolioCashJson {
                sol: 1.25,
                usdc: 42.5,
            },
            gm_positions: PortfolioGmPositionsJson {
                positions: vec![PositionJson {
                    token: "TSLAon".into(),
                    balance: 0.25,
                    price: 385.75,
                    value_usd: 96.44,
                    gm_alloc_pct: 100.0,
                    change_pct_24h: 1.23,
                }],
                value_usd: 96.44,
                change_24h_usd: 1.17,
                change_24h_pct: 1.23,
            },
            unavailable: vec![],
            source: None,
        })
        .unwrap();

        assert_eq!(json.get("wallet"), Some(&Value::String("wallet123".into())));
        assert_eq!(json.pointer("/cash/sol"), Some(&Value::from(1.25)));
        assert_eq!(json.pointer("/cash/usdc"), Some(&Value::from(42.5)));
        assert_eq!(json.pointer("/gm_positions/value_usd"), Some(&Value::from(96.44)));
        assert_eq!(
            json.pointer("/gm_positions/positions/0/gm_alloc_pct"),
            Some(&Value::from(100.0))
        );
        assert!(json.get("sol").is_none());
        assert!(json.get("usdc").is_none());
        assert!(json.get("gm_positions_value_usd").is_none());
        // unavailable omitted when empty
        assert!(json.get("unavailable").is_none());
    }

    #[test]
    fn portfolio_json_omits_unavailable_when_empty() {
        let json = serde_json::to_value(PortfolioJson {
            wallet: "w".into(),
            cash: PortfolioCashJson { sol: 0.0, usdc: 0.0 },
            gm_positions: PortfolioGmPositionsJson {
                positions: vec![],
                value_usd: 0.0,
                change_24h_usd: 0.0,
                change_24h_pct: 0.0,
            },
            unavailable: vec![],
            source: None,
        })
        .unwrap();
        assert!(json.get("unavailable").is_none());
    }

    #[test]
    fn portfolio_json_includes_unavailable_when_present() {
        let json = serde_json::to_value(PortfolioJson {
            wallet: "w".into(),
            cash: PortfolioCashJson { sol: 0.0, usdc: 0.0 },
            gm_positions: PortfolioGmPositionsJson {
                positions: vec![],
                value_usd: 0.0,
                change_24h_usd: 0.0,
                change_24h_pct: 0.0,
            },
            unavailable: vec![PortfolioUnavailableJson {
                symbol: "TSLAon".into(),
                reason: "market data unavailable: not found".into(),
            }],
            source: None,
        })
        .unwrap();
        assert_eq!(
            json.pointer("/unavailable/0/symbol"),
            Some(&Value::from("TSLAon"))
        );
        assert_eq!(
            json.pointer("/unavailable/0/reason"),
            Some(&Value::from("market data unavailable: not found"))
        );
    }

    #[test]
    fn trade_json_serializes_optional_fields_and_rounding() {
        let json = serde_json::to_value(TradeJson {
            gas_refuel: None,
            status: "success",
            amount: "0.2584".into(),
            token: "TSLAon".into(),
            counter_amount: "100.00".into(),
            counter_token: "USDC",
            tx: "https://solscan.io/tx/abc".into(),
            slippage_pct: Some(0.51234),
            actual_slippage_pct: Some(-0.6789),
            price_impact_pct: Some(-0.12345),
            fee_bps: Some(5),
            gasless: Some(false),
            router: Some("jupiterz".into()),
            limit_price: None,
        })
        .unwrap();

        assert_eq!(json.pointer("/status"), Some(&Value::from("success")));
        assert_eq!(json.pointer("/slippage_pct"), Some(&Value::from(0.5123)));
        assert_eq!(json.pointer("/actual_slippage_pct"), Some(&Value::from(-0.6789)));
        assert_eq!(json.pointer("/price_impact_pct"), Some(&Value::from(-0.1235)));
        assert_eq!(json.pointer("/fee_bps"), Some(&Value::from(5)));
        assert_eq!(json.pointer("/gasless"), Some(&Value::from(false)));
        assert_eq!(json.pointer("/router"), Some(&Value::from("jupiterz")));
    }

    #[test]
    fn send_json_keeps_stable_shape() {
        let json = serde_json::to_value(SendJson {
            gas_refuel: None,
            status: "dry_run",
            token: "SOL".into(),
            amount: "0.999995".into(),
            recipient: "recipient123".into(),
            tx: String::new(),
        })
        .unwrap();

        assert_eq!(json.pointer("/status"), Some(&Value::from("dry_run")));
        assert_eq!(json.pointer("/token"), Some(&Value::from("SOL")));
        assert_eq!(json.pointer("/amount"), Some(&Value::from("0.999995")));
        assert_eq!(json.pointer("/recipient"), Some(&Value::from("recipient123")));
        assert_eq!(json.pointer("/tx"), Some(&Value::from("")));
    }

    #[test]
    fn close_all_json_omits_skipped_when_empty() {
        let json = serde_json::to_value(CloseAllResultJson {
            status: "success",
            sold: vec![CloseItemJson {
                token: "TSLAon".into(),
                amount: "0.2500".into(),
                usdc: "96.44".into(),
                tx: "https://solscan.io/tx/abc".into(),
            }],
            failed: vec![CloseFailJson {
                token: "AAPLon".into(),
                error: "not tradable".into(),
                error_kind: None,
            }],
            skipped: vec![],
            total_usdc: "96.44".into(),
        })
        .unwrap();

        assert_eq!(json.pointer("/status"), Some(&Value::from("success")));
        assert_eq!(json.pointer("/sold/0/token"), Some(&Value::from("TSLAon")));
        assert_eq!(json.pointer("/failed/0/token"), Some(&Value::from("AAPLon")));
        assert_eq!(json.pointer("/total_usdc"), Some(&Value::from("96.44")));
        assert!(json.get("skipped").is_none());
    }

    #[test]
    fn close_skip_json_market_data_unavailable_has_correct_shape() {
        let skip = CloseSkipJson {
            token: "TSLAon".to_string(),
            estimated_usd: 0.0,
            reason: "market data unavailable",
        };
        let json = serde_json::to_value(&skip).unwrap();
        assert_eq!(json.pointer("/token"), Some(&serde_json::Value::from("TSLAon")));
        assert_eq!(json.pointer("/reason"), Some(&serde_json::Value::from("market data unavailable")));
    }

    #[test]
    fn portfolio_json_includes_source_only_when_set() {
        let with = serde_json::to_value(PortfolioJson {
            wallet: "w".into(),
            cash: PortfolioCashJson { sol: 0.0, usdc: 0.0 },
            gm_positions: PortfolioGmPositionsJson { positions: vec![], value_usd: 0.0, change_24h_usd: 0.0, change_24h_pct: 0.0 },
            unavailable: vec![],
            source: Some("jupiter"),
        }).unwrap();
        assert_eq!(with.pointer("/source"), Some(&serde_json::Value::from("jupiter")));

        let without = serde_json::to_value(PortfolioJson {
            wallet: "w".into(),
            cash: PortfolioCashJson { sol: 0.0, usdc: 0.0 },
            gm_positions: PortfolioGmPositionsJson { positions: vec![], value_usd: 0.0, change_24h_usd: 0.0, change_24h_pct: 0.0 },
            unavailable: vec![],
            source: None,
        }).unwrap();
        assert!(without.get("source").is_none());
    }

    #[test]
    fn global_wallet_flag_parses_before_and_after_subcommand() {
        let a = crate::Cli::try_parse_from(["rwa", "--wallet", "cold", "gm", "list"]);
        assert!(a.is_ok(), "global --wallet before subcommand: {:?}", a.err());
        let b = crate::Cli::try_parse_from(["rwa", "gm", "portfolio", "--wallet", "cold"]);
        assert!(b.is_ok(), "global --wallet after subcommand: {:?}", b.err());
        // positional portfolio address still works and is distinct from --wallet
        let c = crate::Cli::try_parse_from(["rwa", "gm", "portfolio", "SomeAddress"]);
        assert!(c.is_ok(), "positional wallet address: {:?}", c.err());
    }

    #[test]
    fn limit_price_parses_and_conflicts_with_quote_only() {
        let ok = crate::Cli::try_parse_from([
            "rwa", "gm", "buy", "TSLA", "100", "--limit-price", "400.50", "-y",
        ]);
        assert!(ok.is_ok(), "{:?}", ok.err());
        let ok = crate::Cli::try_parse_from([
            "rwa", "gm", "sell", "TSLA", "all", "--limit-price", "450",
        ]);
        assert!(ok.is_ok(), "{:?}", ok.err());
        let conflict = crate::Cli::try_parse_from([
            "rwa", "gm", "buy", "TSLA", "100", "--limit-price", "400", "--quote-only",
        ]);
        assert!(conflict.is_err(), "--limit-price must conflict with --quote-only");
    }

    #[test]
    fn close_all_json_serializes_skipped_when_present() {
        let json = serde_json::to_value(CloseAllResultJson {
            status: "dry_run",
            sold: vec![],
            failed: vec![],
            skipped: vec![CloseSkipJson {
                token: "QQQon".into(),
                estimated_usd: 1.234,
                reason: "below $1.50 minimum",
            }],
            total_usdc: "0.00".into(),
        })
        .unwrap();

        assert_eq!(json.pointer("/skipped/0/token"), Some(&Value::from("QQQon")));
        assert_eq!(json.pointer("/skipped/0/estimated_usd"), Some(&Value::from(1.23)));
        assert_eq!(
            json.pointer("/skipped/0/reason"),
            Some(&Value::from("below $1.50 minimum"))
        );
    }
}
