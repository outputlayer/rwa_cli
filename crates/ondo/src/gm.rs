use alloy_primitives::{Address, U256};
use alloy_provider::Provider;
use eyre::{Result, eyre};
use rwa_core::contracts::IERC20;
use rwa_core::types::{GmToken, TokenBalance};

use crate::token_list::GM_TOKENS;

/// Resolve a GM token by symbol (case-insensitive).
/// Accepts both "TSLA" and "TSLAon" formats.
pub fn resolve_token(symbol: &str) -> Result<&'static (&'static str, &'static str)> {
    let normalized = symbol.to_uppercase();
    let lookup = if normalized.ends_with("ON") {
        normalized.clone()
    } else {
        format!("{normalized}ON")
    };

    GM_TOKENS
        .iter()
        .find(|(sym, _)| sym.to_uppercase() == lookup)
        .ok_or_else(|| eyre!("Unknown GM token: {symbol}. Use `rwa gm list` to see available tokens."))
}

/// Fetch on-chain token info (name, symbol, decimals).
pub async fn get_token_info<P: Provider>(provider: &P, address: Address) -> Result<GmToken> {
    let erc20 = IERC20::new(address, provider);

    let name = erc20.name().call().await?.into();
    let symbol = erc20.symbol().call().await?.into();
    let decimals = erc20.decimals().call().await?;

    Ok(GmToken {
        symbol,
        name,
        address,
        decimals,
    })
}

/// Fetch balance for a specific token.
pub async fn get_balance<P: Provider>(
    provider: &P,
    token_address: Address,
    wallet: Address,
) -> Result<U256> {
    let erc20 = IERC20::new(token_address, provider);
    let balance = erc20.balanceOf(wallet).call().await?;
    Ok(balance)
}

/// Fetch balances for all GM tokens that a wallet holds (non-zero).
pub async fn get_all_balances<P: Provider>(
    provider: &P,
    wallet: Address,
) -> Result<Vec<TokenBalance>> {
    let mut balances = Vec::new();

    for &(symbol, addr_str) in GM_TOKENS.iter() {
        let address: Address = addr_str.parse()?;
        let erc20 = IERC20::new(address, provider);
        let balance = erc20.balanceOf(wallet).call().await?;

        if balance > U256::ZERO {
            balances.push(TokenBalance {
                token: GmToken {
                    symbol: symbol.to_string(),
                    name: String::new(),
                    address,
                    decimals: 18,
                },
                balance,
            });
        }
    }

    Ok(balances)
}
