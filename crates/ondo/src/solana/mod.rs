mod balance;
mod rpc;
mod fee;
mod transaction;
mod transfer;

use eyre::{Result, eyre};

// Re-export public API
pub use crate::USDC_MINT;
use balance::TOKEN_2022_PROGRAM;
pub use balance::{
    get_sol_balance, get_sol_balance_raw,
    get_usdc_balance, get_usdc_balance_raw,
    get_all_balances, get_balance,
    SolanaTokenBalance, PortfolioBalances, get_portfolio_balances,
};
#[cfg(any(test, feature = "test-util"))]
pub use balance::get_portfolio_balances_with_urls;
pub use fee::{estimate_gas_needed, estimate_tx_fee, estimate_tx_fee_lamports};
pub use transaction::{TransactionResult, confirm_transaction, send_signed_transaction};
pub use transfer::{
    transfer_sol, transfer_spl,
    EmptyTokenAccount, get_empty_token_accounts, close_empty_accounts,
};

/// Validate a Solana base58 address (32-44 chars, valid base58, decodes to 32 bytes).
pub fn validate_address(addr: &str) -> Result<()> {
    if addr.len() < 32 || addr.len() > 44 || addr.chars().any(|c| c.is_whitespace()) {
        return Err(eyre!("Invalid Solana address: {addr}"));
    }
    let bytes = bs58::decode(addr).into_vec()
        .map_err(|e| eyre!("Invalid Solana address (bad base58): {e}"))?;
    if bytes.len() != 32 {
        return Err(eyre!("Invalid Solana address: expected 32 bytes, got {}", bytes.len()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_valid_address() {
        assert!(validate_address("11111111111111111111111111111111").is_ok());
    }

    #[test]
    fn validate_real_wallet_address() {
        assert!(validate_address("5CjgV1J2FE8yyxsHKGs2v4GJULBS7AiYtRo7DFYiuZ47").is_ok());
    }

    #[test]
    fn validate_usdc_mint() {
        assert!(validate_address("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v").is_ok());
    }

    #[test]
    fn reject_too_short() {
        assert!(validate_address("abc").is_err());
    }

    #[test]
    fn reject_too_long() {
        assert!(validate_address(&"1".repeat(50)).is_err());
    }

    #[test]
    fn reject_whitespace() {
        assert!(validate_address("5CjgV1J2FE8yyxsHKGs2v4GJULBS7AiY tRo7DFYiuZ47").is_err());
    }

    #[test]
    fn reject_invalid_base58() {
        assert!(validate_address("OOOOOOOOOOOOOOOOOOOOOOOOOOOOOOOO").is_err());
    }

    #[test]
    fn reject_empty() {
        assert!(validate_address("").is_err());
    }
}
