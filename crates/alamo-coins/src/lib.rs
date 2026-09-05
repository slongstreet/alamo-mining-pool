//! Coin definitions and node integration.
//!
//! A [`Coin`] knows its proof-of-work algorithm and how to talk to its node. Merge mining
//! is modeled as a parent coin whose coinbase commits to one or more aux coins.

#![forbid(unsafe_code)]

pub mod auxpow;
pub mod config;
pub mod doge;
pub mod ltc;
pub mod rpc;

use alamo_core::Algorithm;

pub use config::CoinConfig;
pub use doge::Dogecoin;
pub use ltc::Litecoin;
pub use rpc::{RpcClient, RpcError};

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
}

/// Look up a built-in coin by its config key (`"ltc"`, `"doge"`).
pub fn builtin(key: &str) -> Option<Box<dyn Coin>> {
    match key.to_ascii_lowercase().as_str() {
        "ltc" => Some(Box::new(Litecoin)),
        "doge" => Some(Box::new(Dogecoin)),
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
}
