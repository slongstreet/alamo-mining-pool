//! Litecoin: the parent chain for merge mining.

use crate::{Chain, Coin};
use alamo_core::{AddressParams, Algorithm};

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

    fn address_params(&self, chain: Chain) -> AddressParams {
        match chain {
            Chain::Main => AddressParams {
                p2pkh_prefix: 48,
                p2sh_prefixes: &[50, 5],
                bech32_hrp: Some("ltc"),
            },
            Chain::Test => AddressParams {
                p2pkh_prefix: 111,
                p2sh_prefixes: &[58, 196],
                bech32_hrp: Some("tltc"),
            },
            Chain::Regtest => AddressParams {
                p2pkh_prefix: 111,
                p2sh_prefixes: &[58, 196],
                bech32_hrp: Some("rltc"),
            },
        }
    }

    fn template_rules(&self) -> &'static [&'static str] {
        &["segwit", "mweb"]
    }
}
