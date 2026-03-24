use alloy_primitives::Address;
use serde::{Deserialize, Serialize};

/// Supported chains for RWA protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Chain {
    BnbMainnet,
}

impl Chain {
    pub const fn chain_id(&self) -> u64 {
        match self {
            Self::BnbMainnet => 56,
        }
    }

    pub fn default_rpc_url(&self) -> &'static str {
        match self {
            Self::BnbMainnet => "https://bsc-dataseed.binance.org",
        }
    }

    pub fn explorer_url(&self, address: Address) -> String {
        match self {
            Self::BnbMainnet => format!("https://bscscan.com/address/{address}"),
        }
    }
}

impl std::fmt::Display for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BnbMainnet => write!(f, "BNB Chain"),
        }
    }
}
