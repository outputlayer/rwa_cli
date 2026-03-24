use alloy_primitives::{Address, U256};
use alloy_provider::Provider;
use eyre::{Result, eyre};
use rwa_core::contracts::IERC20;
use rwa_core::types::{GmToken, TokenBalance};

use crate::token_list::GmTokenEntry;

/// Resolve a GM token by symbol (case-insensitive).
/// Accepts both "TSLA" and "TSLAon" formats.
pub fn resolve_token<'a>(symbol: &str, tokens: &'a [GmTokenEntry]) -> Result<&'a GmTokenEntry> {
    let normalized = symbol.to_uppercase();
    let lookup = if normalized.ends_with("ON") {
        normalized.clone()
    } else {
        format!("{normalized}ON")
    };

    tokens
        .iter()
        .find(|t| t.symbol.to_uppercase() == lookup)
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
    tokens: &[GmTokenEntry],
) -> Result<Vec<TokenBalance>> {
    let mut balances = Vec::new();

    for entry in tokens {
        let address = match entry.bsc_address {
            Some(a) => a,
            None => continue,
        };
        let erc20 = IERC20::new(address, provider);
        let balance = erc20.balanceOf(wallet).call().await?;

        if balance > U256::ZERO {
            balances.push(TokenBalance {
                token: GmToken {
                    symbol: entry.symbol.clone(),
                    name: entry.name.clone(),
                    address,
                    decimals: entry.decimals,
                },
                balance,
            });
        }
    }

    Ok(balances)
}
