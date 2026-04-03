mod close_all;
mod list;
mod portfolio;
mod send;
mod trade;
mod types;

use clap::Subcommand;
use eyre::Result;
use rwa_ondo::{gm, token_list, types::{Mint, Symbol}, usecases, wallet};
use serde::Serialize;
use std::io::{self, Write};

pub(super) use types::{
    BuyBasketItemJson, BuyBasketResultJson, CloseAllResultJson, CloseFailJson, CloseItemJson,
    CloseSkipJson, HistoryCandleJson, HistoryJson, HoursJson, ListItemJson, PortfolioCashJson,
    PortfolioGmPositionsJson, PortfolioJson, PositionJson, ReclaimJson, SellBasketItemJson,
    SellBasketResultJson, SendJson, TradeJson, TradableItemJson, TradableResultJson,
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
    },

    /// Portfolio positions and P&L (Solana)
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

    /// Close all GM positions — sell every token for USDC sequentially
    CloseAll {
        /// Percentage of each position to sell (e.g. 10%, 50%). Sells 100% if omitted.
        amount: Option<String>,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
        /// Show what would be sold without executing
        #[arg(long)]
        dry_run: bool,
        /// Fetch all orders and execute all swaps in parallel (faster for many positions)
        #[arg(long)]
        parallel: bool,
    },

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
        /// Fetch all orders and execute all swaps in parallel (faster for many tokens)
        #[arg(long)]
        parallel: bool,
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
        /// Fetch all orders and execute all swaps in parallel (faster for many tokens)
        #[arg(long)]
        parallel: bool,
    },
}

pub async fn execute(action: GmAction, json: bool, rpc_url: Option<&str>) -> Result<()> {
    match action {
        GmAction::Hours { tradable } => list::hours(json, tradable).await,
        GmAction::Buy {
            symbol,
            amount,
            yes,
            slippage,
            dry_run,
        } => {
            trade::buy(
                &symbol,
                &amount,
                yes,
                dry_run,
                json,
                rpc_url,
                Some(slippage.unwrap_or(usecases::gm::DEFAULT_SLIPPAGE_BPS)),
            )
            .await
        }
        GmAction::Sell {
            symbol,
            amount,
            yes,
            slippage,
            dry_run,
        } => {
            trade::sell(
                &symbol,
                &amount,
                yes,
                dry_run,
                json,
                rpc_url,
                slippage,
            )
            .await
        }
        GmAction::Portfolio { wallet } => {
            portfolio::portfolio(wallet.as_deref(), json, rpc_url).await
        }
        GmAction::History { symbol, range } => portfolio::history(&symbol, &range, json).await,
        GmAction::List => list::list(json).await,
        GmAction::Search {
            search,
            tradable_only,
            sectors,
            kind,
            name_keywords,
        } => list::search(
            json,
            &search,
            tradable_only,
            &sectors,
            kind.as_deref(),
            &name_keywords,
        )
        .await,
        GmAction::Tradable { symbols } => list::tradable(json, &symbols).await,
        GmAction::Send {
            token,
            amount,
            to,
            yes,
            dry_run,
        } => send::send(&token, &amount, &to, yes, dry_run, json, rpc_url).await,
        GmAction::CloseAll {
            amount,
            yes,
            dry_run,
            parallel,
        } => close_all::close_all(amount.as_deref(), yes, dry_run, parallel, json, rpc_url).await,
        GmAction::Reclaim { token } => trade::reclaim(token.as_deref(), json, rpc_url).await,
        GmAction::BuyBasket { tokens, yes, dry_run, parallel } =>
            trade::buy_basket(&tokens, yes, dry_run, parallel, json, rpc_url).await,
        GmAction::SellBasket { tokens, yes, dry_run, parallel } =>
            trade::sell_basket(&tokens, yes, dry_run, parallel, json, rpc_url).await,
    }
}

// ── Shared helpers ─────────────────────────────────────────

fn json_out(v: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(v)?);
    Ok(())
}

fn confirm(msg: &str) -> bool {
    print!("{msg} [y/N] ");
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

fn load_wallet() -> Result<wallet::Wallet> {
    if wallet::is_wallet_encrypted() {
        let passphrase = match std::env::var("RWA_PASSPHRASE") {
            Ok(p) => p,
            Err(_) => rpassword::prompt_password("Wallet passphrase: ")
                .map_err(|e| eyre::eyre!("Failed to read passphrase: {e}"))?,
        };
        return wallet::Wallet::load_default_encrypted(&passphrase);
    }
    wallet::Wallet::load_default().map_err(|_| {
        eyre::eyre!(
            "No wallet found.\n\n\
             Create or import one first:\n  \
             rwa keys generate                          Create a new wallet\n  \
             rwa keys import --seed-phrase \"word1 ...\"   Import from seed phrase\n  \
             rwa keys import --private-key <BASE58>     Import from private key\n  \
             rwa keys import --file <PATH>              Import from key file"
        )
    })
}

fn resolve_gm_mint(symbol: &str, tokens: &[token_list::GmTokenEntry]) -> Result<(Symbol, Mint)> {
    let entry = gm::resolve_token(symbol, tokens)?;
    let mint = entry
        .solana_address
        .ok_or_else(|| eyre::eyre!("No Solana address for {}", entry.symbol))?;
    Ok((Symbol::from(entry.symbol), Mint::from(mint)))
}

fn clean_name(name: &str) -> String {
    name.replace(" (Ondo Tokenized)", "")
}

fn token_type_from_name(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if n.contains("etf")
        || n.contains(" fund")
        || n.contains(" trust")
        || n.contains(" index")
        || n.contains(" shares")
    {
        "etf"
    } else {
        "stock"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    // ── clean_name ────────────────────────────────────────────

    #[test]
    fn clean_name_removes_suffix() {
        assert_eq!(clean_name("Tesla (Ondo Tokenized)"), "Tesla");
    }

    #[test]
    fn clean_name_no_suffix() {
        assert_eq!(clean_name("Apple"), "Apple");
    }

    // ── token_type_from_name ──────────────────────────────────

    #[test]
    fn detect_etf() {
        assert_eq!(token_type_from_name("SPDR S&P 500 ETF Trust"), "etf");
        assert_eq!(
            token_type_from_name("Vanguard Total Stock Market Index Fund"),
            "etf"
        );
    }

    #[test]
    fn detect_stock() {
        assert_eq!(token_type_from_name("Tesla"), "stock");
        assert_eq!(token_type_from_name("Apple Inc"), "stock");
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
    }

    #[test]
    fn trade_json_serializes_optional_fields_and_rounding() {
        let json = serde_json::to_value(TradeJson {
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
