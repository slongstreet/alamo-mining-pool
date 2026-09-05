//! Litecoin: the parent chain for merge mining.

use crate::Coin;
use alamo_core::Algorithm;

/// Litecoin (scrypt). Acts as the parent chain; its coinbase carries aux commitments.
#[derive(Clone, Copy, Debug, Default)]
pub struct Litecoin;

impl Coin for Litecoin {
    fn symbol(&self) -> &'static str {
        "LTC"
    }

    fn name(&self) -> &'static str {
        "Litecoin"
    }

    fn algorithm(&self) -> Algorithm {
        Algorithm::Scrypt
    }

    fn aux_chain_id(&self) -> Option<u32> {
        None
    }
}
