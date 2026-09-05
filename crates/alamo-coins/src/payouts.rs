//! Username to payout-script resolution for one coin.

use alamo_core::address::{payout_script, AddressError, AddressParams};
use alamo_core::payout::{split_username, Payout, PayoutResolver};

/// Resolves usernames as addresses of one coin, with a fallback for invalid ones.
#[derive(Clone, Debug)]
pub struct CoinPayouts {
    params: AddressParams,
    fallback_address: String,
    fallback_script: Vec<u8>,
}

impl CoinPayouts {
    /// Build a resolver. Fails if the fallback address is itself invalid.
    pub fn new(params: AddressParams, fallback_address: &str) -> Result<Self, AddressError> {
        let fallback_script = payout_script(fallback_address, &params)?;
        Ok(Self {
            params,
            fallback_address: fallback_address.to_string(),
            fallback_script,
        })
    }

    /// The configured fallback address.
    pub fn fallback_address(&self) -> &str {
        &self.fallback_address
    }
}

impl PayoutResolver for CoinPayouts {
    fn resolve(&self, username: &str) -> Payout {
        let (address, worker) = split_username(username);
        match payout_script(address, &self.params) {
            Ok(script) => Payout {
                address: address.to_string(),
                script,
                worker: worker.to_string(),
                fallback: false,
            },
            Err(err) => {
                tracing::warn!(username, %err, fallback = %self.fallback_address, "username is not a valid address; paying fallback");
                Payout {
                    address: self.fallback_address.clone(),
                    script: self.fallback_script.clone(),
                    worker: worker.to_string(),
                    fallback: true,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alamo_core::address::encode_segwit;

    #[test]
    fn valid_address_pays_itself_else_fallback() {
        let params = AddressParams {
            p2pkh_prefix: 111,
            p2sh_prefixes: &[58],
            bech32_hrp: Some("rltc"),
        };
        let fallback = encode_segwit("rltc", 0, &[9; 20]).unwrap();
        let payouts = CoinPayouts::new(params, &fallback).unwrap();
        let mine = encode_segwit("rltc", 0, &[1; 20]).unwrap();
        let p = payouts.resolve(&format!("{mine}.rig1"));
        assert_eq!(p.address, mine);
        assert_eq!(p.worker, "rig1");
        assert!(!p.fallback);
        let p = payouts.resolve("bogus.rig2");
        assert_eq!(p.address, fallback);
        assert!(p.fallback);
        assert_eq!(p.worker, "rig2");
    }

    #[test]
    fn bad_fallback_is_an_error() {
        let params = AddressParams {
            p2pkh_prefix: 111,
            p2sh_prefixes: &[58],
            bech32_hrp: Some("rltc"),
        };
        assert!(CoinPayouts::new(params, "nope").is_err());
    }
}
