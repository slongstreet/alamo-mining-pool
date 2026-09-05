//! Dogecoin: merge-mined against Litecoin.

use crate::{Chain, Coin};
use alamo_core::auxpow::aux_block_version;
use alamo_core::{AddressParams, Algorithm};

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

    fn address_params(&self, chain: Chain) -> AddressParams {
        match chain {
            Chain::Main => AddressParams {
                p2pkh_prefix: 30,
                p2sh_prefixes: &[22],
                bech32_hrp: None,
            },
            Chain::Test => AddressParams {
                p2pkh_prefix: 113,
                p2sh_prefixes: &[196],
                bech32_hrp: None,
            },
            Chain::Regtest => AddressParams {
                p2pkh_prefix: 111,
                p2sh_prefixes: &[196],
                bech32_hrp: None,
            },
        }
    }

    fn template_rules(&self) -> &'static [&'static str] {
        &[]
    }

    fn block_version(&self, template_version: i32) -> i32 {
        aux_block_version(template_version, CHAIN_ID)
    }

    fn coinbase_maturity(&self, chain: Chain) -> i64 {
        match chain {
            Chain::Regtest => 60,
            _ => 240,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dogecoin_auxpow_version() {
        // Dogecoin templates carry 0x00620004; mined blocks carry the auxpow flag too.
        assert_eq!(Dogecoin.block_version(0x0062_0004), 0x0062_0104);
        assert_eq!(Dogecoin.block_version(4), 0x0062_0104);
    }
}
