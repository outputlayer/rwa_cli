use alloy_primitives::{Address, U256};
use alloy_provider::Provider;
use alloy_sol_types::SolCall;
use eyre::{Result, eyre};
use rwa_core::chain::Chain;
use rwa_core::contracts::{IERC20, IMulticall3, MULTICALL3};
use rwa_core::types::{GmToken, TokenBalance};

use crate::token_list::GmTokenEntry;

/// Resolve a GM token by symbol or contract address (case-insensitive).
/// Accepts "TSLA", "TSLAon" formats, a hex address like "0x2494...", or a Solana base58 address.
pub fn resolve_token<'a>(symbol: &str, tokens: &'a [GmTokenEntry]) -> Result<&'a GmTokenEntry> {
    // Try to parse as EVM contract address first
    if let Ok(addr) = symbol.parse::<Address>() {
        return tokens
            .iter()
            .find(|t| t.bsc_address == Some(addr) || t.eth_address == Some(addr))
            .ok_or_else(|| eyre!("No GM token found for address {symbol}. Use `rwa gm list` to see available tokens."));
    }

    // Try Solana address lookup (base58, typically ends with "ondo")
    if symbol.len() >= 32 && !symbol.starts_with("0x") {
        if let Some(entry) = tokens.iter().find(|t| t.solana_address.as_deref() == Some(symbol)) {
            return Ok(entry);
        }
    }

    let normalized = symbol.to_uppercase();
    let lookup = if normalized.ends_with("ON") {
        normalized
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

    let name = erc20.name().call().await?;
    let symbol = erc20.symbol().call().await?;
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

/// Get the EVM address for a token on the given chain.
pub fn token_address_for_chain(entry: &GmTokenEntry, chain: Chain) -> Option<Address> {
    match chain {
        Chain::BnbMainnet => entry.bsc_address,
        Chain::EthereumMainnet => entry.eth_address,
        Chain::SolanaMainnet => None,
    }
}

/// Fetch balances for all GM tokens via Multicall3 (single RPC call).
/// Works on any EVM chain (BSC, Ethereum) — just pass the right chain.
pub async fn get_all_balances<P: Provider>(
    provider: &P,
    wallet: Address,
    tokens: &[GmTokenEntry],
    chain: Chain,
) -> Result<Vec<TokenBalance>> {
    let entries: Vec<&GmTokenEntry> = tokens
        .iter()
        .filter(|e| token_address_for_chain(e, chain).is_some())
        .collect();

    let calls: Vec<IMulticall3::Call3> = entries
        .iter()
        .map(|e| IMulticall3::Call3 {
            target: token_address_for_chain(e, chain).unwrap(),
            allowFailure: true,
            callData: IERC20::balanceOfCall { account: wallet }.abi_encode().into(),
        })
        .collect();

    let multicall = IMulticall3::new(MULTICALL3, provider);
    let results = multicall.aggregate3(calls).call().await?;

    let mut balances = Vec::new();
    for (i, result) in results.iter().enumerate() {
        if !result.success || result.returnData.len() < 32 {
            continue;
        }
        let balance = U256::from_be_slice(&result.returnData[result.returnData.len() - 32..]);
        if balance > U256::ZERO {
            let entry = entries[i];
            balances.push(TokenBalance {
                token: GmToken {
                    symbol: entry.symbol.clone(),
                    name: entry.name.clone(),
                    address: token_address_for_chain(entry, chain).unwrap(),
                    decimals: entry.decimals,
                },
                balance,
            });
        }
    }

    Ok(balances)
}
