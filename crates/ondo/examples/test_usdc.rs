//! Example: fetch portfolio balances for a Solana wallet.
//! Run: cargo run -p rwa-ondo --example test_usdc

use rwa_ondo::{solana, token_list};

#[tokio::main]
async fn main() {
    let wallet = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Dn9EqxugBePrno7gzCjbGeYxY3VJE9RB2WE2FH7t7qmH".to_string());

    let tokens = token_list::get_token_list().await;

    match solana::get_portfolio_balances(&wallet, &tokens, None).await {
        Ok(b) => {
            println!("SOL:  {:.6}", b.sol);
            println!("USDC: {:.2}", b.usdc);
            println!("GM positions: {}", b.gm_tokens.len());
            for t in &b.gm_tokens {
                println!("  {:<10} {:.6}", t.symbol, t.balance);
            }
        }
        Err(e) => eprintln!("Error: {e}"),
    }
}
