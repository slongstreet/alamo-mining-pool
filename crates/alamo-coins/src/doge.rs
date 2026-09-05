//! Dogecoin: merge-mined against Litecoin.

use crate::Coin;
use alamo_core::Algorithm;

/// Dogecoin's merged-mining chain id (`0x0062`).
pub const CHAIN_ID: u32 = 98;

/// Dogecoin (scrypt, auxpow).
#[derive(Clone, Copy, Debug, Default)]
pub struct Dogecoin;

impl Coin for Dogecoin {
    fn symbol(&self) -> &'static str {
        "DOGE"
    }

    fn name(&self) -> &'static str {
        "Dogecoin"
    }

    fn algorithm(&self) -> Algorithm {
        Algorithm::Scrypt
    }

    fn aux_chain_id(&self) -> Option<u32> {
        Some(CHAIN_ID)
    }
}
