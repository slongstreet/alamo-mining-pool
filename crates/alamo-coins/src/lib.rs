//! Coin definitions and node integration.
//!
//! A [`Coin`] knows its proof-of-work algorithm, address formats, and how to talk to its
//! node. Merge mining is modeled as a parent coin whose coinbase commits to aux coins.

#![forbid(unsafe_code)]

pub mod config;
pub mod doge;
pub mod ltc;
pub mod merged;
pub mod rpc;
pub mod template;
pub mod zmq;

use alamo_core::{AddressParams, Algorithm};
use std::sync::Arc;

pub use config::CoinConfig;
pub use doge::Dogecoin;
pub use ltc::Litecoin;
pub use merged::merge;
pub use rpc::{RpcClient, RpcError};
pub use template::{NodeHealth, RawTemplate, TemplateError, TemplateSource};
pub use zmq::{ZmqError, ZmqStatus, ZmqSubscriber};

/// Which network a node is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chain {
    /// Production network.
    Main,
    /// Public test network.
    Test,
    /// Local regression test network.
    Regtest,
}

impl std::fmt::Display for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Chain::Main => "main",
            Chain::Test => "test",
            Chain::Regtest => "regtest",
        })
    }
}

impl Chain {
    /// Parse the `chain` field of `getblockchaininfo`.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "main" => Some(Chain::Main),
            "test" | "testnet" | "testnet4" | "signet" => Some(Chain::Test),
            "regtest" => Some(Chain::Regtest),
            _ => None,
        }
    }
}

/// Static properties of a mineable coin.
pub trait Coin: Send + Sync + 'static {
    /// Ticker symbol, upper case (`"LTC"`).
    fn symbol(&self) -> &'static str;
    /// Human-readable name (`"Litecoin"`).
    fn name(&self) -> &'static str;
    /// Proof-of-work algorithm.
    fn algorithm(&self) -> Algorithm;
    /// Merged-mining chain id, if this coin can be mined as an aux chain.
    fn aux_chain_id(&self) -> Option<u32>;
    /// Address encoding for the given network.
    fn address_params(&self, chain: Chain) -> AddressParams;
    /// Rules to pass to `getblocktemplate`.
    fn template_rules(&self) -> &'static [&'static str];
    /// The block version to mine given the template's version. Aux chains add their
    /// chain id and the auxpow flag.
    fn block_version(&self, template_version: i32) -> i32 {
        template_version
    }
    /// Confirmations before a coinbase can be spent; a block is final after this many.
    fn coinbase_maturity(&self, chain: Chain) -> i64;
    /// Coin-specific bytes appended after the transactions in a serialized block.
    fn extra_block_payload(&self, _raw: &RawTemplate) -> Result<Vec<u8>, TemplateError> {
        Ok(Vec::new())
    }
}

/// Look up a built-in coin by its config key (`"ltc"`, `"doge"`).
pub fn builtin(key: &str) -> Option<Arc<dyn Coin>> {
    match key.to_ascii_lowercase().as_str() {
        "ltc" => Some(Arc::new(Litecoin)),
        "doge" => Some(Arc::new(Dogecoin)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_lookup() {
        assert_eq!(builtin("ltc").unwrap().symbol(), "LTC");
        assert_eq!(builtin("DOGE").unwrap().symbol(), "DOGE");
        assert!(builtin("btc").is_none());
    }

    #[test]
    fn chain_parse() {
        assert_eq!(Chain::parse("regtest"), Some(Chain::Regtest));
        assert_eq!(Chain::parse("main"), Some(Chain::Main));
        assert_eq!(Chain::parse("nope"), None);
    }
}
