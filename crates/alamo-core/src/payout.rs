//! Mapping stratum usernames to payout scripts.

use crate::address::{payout_script, AddressError, AddressParams};

/// Where a worker's block reward goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Payout {
    /// The address that will be paid, as a string for display.
    pub address: String,
    /// The scriptPubKey paying that address.
    pub script: Vec<u8>,
    /// True when the username was not a valid address and the fallback was used.
    pub fallback: bool,
}

/// Resolves stratum usernames (`address` or `address.worker`) to payouts for one coin,
/// substituting a fallback address when the username is not a valid address.
#[derive(Clone, Debug)]
pub struct PayoutTable {
    params: AddressParams,
    fallback_address: String,
    fallback_script: Vec<u8>,
}

impl PayoutTable {
    /// Build a table. Fails if the fallback address is itself invalid.
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

    /// Resolve a username.
    pub fn resolve(&self, username: &str) -> Payout {
        let (address, _worker) = split_username(username);
        match payout_script(address, &self.params) {
            Ok(script) => Payout {
                address: address.to_string(),
                script,
                fallback: false,
            },
            Err(_) => Payout {
                address: self.fallback_address.clone(),
                script: self.fallback_script.clone(),
                fallback: true,
            },
        }
    }
}

/// Split a stratum username into its address part and worker suffix.
pub fn split_username(username: &str) -> (&str, &str) {
    match username.split_once('.') {
        Some((address, worker)) => (address.trim(), worker.trim()),
        None => (username.trim(), ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::encode_segwit;

    #[test]
    fn splits_worker_suffix() {
        assert_eq!(split_username("addr.rig1"), ("addr", "rig1"));
        assert_eq!(split_username("addr"), ("addr", ""));
        assert_eq!(split_username(" addr . rig.2 "), ("addr", "rig.2"));
    }

    #[test]
    fn valid_address_pays_itself_else_fallback() {
        let params = AddressParams {
            p2pkh_prefix: 111,
            p2sh_prefixes: &[58, 196],
            bech32_hrp: Some("rltc"),
        };
        let fallback = encode_segwit("rltc", 0, &[9; 20]).unwrap();
        let table = PayoutTable::new(params, &fallback).unwrap();
        let mine = encode_segwit("rltc", 0, &[1; 20]).unwrap();
        let p = table.resolve(&format!("{mine}.rig1"));
        assert_eq!(p.address, mine);
        assert!(!p.fallback);
        let p = table.resolve("bogus.rig2");
        assert_eq!(p.address, fallback);
        assert!(p.fallback);
    }

    #[test]
    fn bad_fallback_is_an_error() {
        let params = AddressParams {
            p2pkh_prefix: 111,
            p2sh_prefixes: &[58],
            bech32_hrp: Some("rltc"),
        };
        assert!(PayoutTable::new(params, "nope").is_err());
    }
}
