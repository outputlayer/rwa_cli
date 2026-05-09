//! Cross-cutting helpers shared by every gm subcommand.
//!
//! Includes wallet loading (with passphrase prompting and one-shot env-var
//! warning), JSON output, interactive y/N confirmation, mint resolution from
//! the static token list, and small string utilities used by the list/search
//! flows.

use eyre::Result;
use rwa_ondo::{gm, token_list, types::{Mint, Symbol}, wallet};
use serde::Serialize;
use std::io::{self, Write};

pub(super) fn solscan_tx_url(sig: &str) -> String {
    format!("https://solscan.io/tx/{sig}")
}

pub(super) fn json_out(v: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string(v)?);
    Ok(())
}

pub(super) fn confirm(msg: &str) -> bool {
    print!("{msg} [y/N] ");
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

fn warn_passphrase_env_once() {
    use std::sync::OnceLock;
    static WARNED: OnceLock<()> = OnceLock::new();
    WARNED.get_or_init(|| {
        eprintln!(
            "WARNING: RWA_PASSPHRASE in environment leaks via shell history / ps. \
            Prefer interactive passphrase prompt."
        );
    });
}

pub(super) fn load_wallet() -> Result<wallet::Wallet> {
    if wallet::is_wallet_encrypted() {
        let passphrase = match std::env::var("RWA_PASSPHRASE") {
            Ok(p) => {
                warn_passphrase_env_once();
                p
            }
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

pub(super) fn resolve_gm_mint(
    symbol: &str,
    tokens: &[token_list::GmTokenEntry],
) -> Result<(Symbol, Mint)> {
    let entry = gm::resolve_token(symbol, tokens)?;
    let mint = entry
        .solana_address
        .ok_or_else(|| eyre::eyre!("No Solana address for {}", entry.symbol))?;
    Ok((Symbol::from(entry.symbol), Mint::from(mint)))
}

pub(super) fn clean_name(name: &str) -> String {
    name.replace(" (Ondo Tokenized)", "")
}

pub(super) fn token_type_from_name(name: &str) -> &'static str {
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

    #[test]
    fn clean_name_removes_suffix() {
        assert_eq!(clean_name("Tesla (Ondo Tokenized)"), "Tesla");
    }

    #[test]
    fn clean_name_no_suffix() {
        assert_eq!(clean_name("Apple"), "Apple");
    }

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
}
