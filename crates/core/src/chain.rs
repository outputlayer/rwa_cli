use alloy_primitives::Address;
use serde::{Deserialize, Serialize};

/// Supported chains for RWA protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Chain {
    BnbMainnet,
    EthereumMainnet,
    SolanaMainnet,
}

impl Chain {
    pub fn default_rpc_url(&self) -> &'static str {
        match self {
            Self::BnbMainnet => "https://bsc-dataseed.binance.org",
            Self::EthereumMainnet => "https://ethereum-rpc.publicnode.com",
            Self::SolanaMainnet => "https://api.mainnet-beta.solana.com",
        }
    }

    pub fn explorer_url_str(&self, address: &str) -> String {
        match self {
            Self::BnbMainnet => format!("https://bscscan.com/address/{address}"),
            Self::EthereumMainnet => format!("https://etherscan.io/address/{address}"),
            Self::SolanaMainnet => format!("https://solscan.io/account/{address}"),
        }
    }

    pub fn explorer_url(&self, address: Address) -> String {
        self.explorer_url_str(&address.to_string())
    }
}

impl std::fmt::Display for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BnbMainnet => write!(f, "BNB Chain"),
            Self::EthereumMainnet => write!(f, "Ethereum"),
            Self::SolanaMainnet => write!(f, "Solana"),
        }
    }
}
