//! Litecoin: the parent chain for merge mining.

use crate::template::{RawTemplate, TemplateError};
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

    fn coinbase_maturity(&self) -> i64 {
        100
    }

    /// Litecoin serializes the MWEB extension block after the transactions as an optional
    /// pointer: a 0x01 presence byte followed by the block. The node only reads it when the
    /// last transaction is the HogEx, which the template already includes, so nothing is
    /// appended before activation.
    fn extra_block_payload(&self, raw: &RawTemplate) -> Result<Vec<u8>, TemplateError> {
        let Some(mweb) = &raw.mweb else {
            return Ok(Vec::new());
        };
        let mut payload = vec![0x01];
        payload.extend(hex::decode(mweb).map_err(|_| TemplateError::Hex("mweb"))?);
        Ok(payload)
    }
}
