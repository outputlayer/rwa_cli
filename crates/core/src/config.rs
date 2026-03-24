use crate::chain::Chain;
use serde::{Deserialize, Serialize};

/// Global CLI configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub chain: Chain,
    pub rpc_url: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            chain: Chain::BnbMainnet,
            rpc_url: None,
        }
    }
}

impl Config {
    pub fn rpc_url(&self) -> &str {
        self.rpc_url
            .as_deref()
            .unwrap_or(self.chain.default_rpc_url())
    }
}
