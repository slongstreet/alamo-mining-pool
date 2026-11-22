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
        self.parse(address).unwrap_or_else(|| self.fallback())
    }

    /// The payout for `address`, if it is valid for this coin.
    pub fn parse(&self, address: &str) -> Option<Payout> {
        let script = payout_script(address, &self.params).ok()?;
        Some(Payout {
            address: address.to_string(),
            script,
            fallback: false,
        })
    }

    /// The fallback payout.
    pub fn fallback(&self) -> Payout {
        Payout {
            address: self.fallback_address.clone(),
            script: self.fallback_script.clone(),
            fallback: true,
        }
    }
}

/// Payout tables for the parent chain and every aux chain.
///
/// The parent address is the stratum username. Aux addresses come from the password field,
/// either bare (`DAddress`) or tagged (`doge=DAddress`), separated by commas or spaces;
/// anything else in the password (`x`, or `d=1024`, which [`password_difficulty`] reads)
/// is ignored here.
#[derive(Clone, Debug)]
pub struct PayoutSet {
    /// The parent chain.
    pub parent: PayoutTable,
    /// Aux chains, in the order they are mined.
    pub aux: Vec<AuxPayoutTable>,
}

/// One aux chain's payout table.
#[derive(Clone, Debug)]
pub struct AuxPayoutTable {
    /// Ticker (`"DOGE"`).
    pub coin: &'static str,
    /// Address resolver.
    pub table: PayoutTable,
}

/// Where one worker's rewards go on every chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Payouts {
    /// Parent chain payout.
    pub parent: Payout,
    /// Aux chain payouts, in the same order as [`PayoutSet::aux`].
    pub aux: Vec<AuxPayout>,
}

/// An aux chain payout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuxPayout {
    /// Ticker.
    pub coin: &'static str,
    /// The payout.
    pub payout: Payout,
}

impl PayoutSet {
    /// Resolve a username and password to payouts on every chain.
    pub fn resolve(&self, username: &str, password: &str) -> Payouts {
        let tokens: Vec<&str> = password
            .split([',', ' ', '\t'])
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect();
        let aux = self
            .aux
            .iter()
            .map(|aux| {
                let payout = tokens
                    .iter()
                    .find_map(|token| match token.split_once('=') {
                        Some((key, value)) if key.eq_ignore_ascii_case(aux.coin) => {
                            aux.table.parse(value.trim())
                        }
                        Some(_) => None,
                        None => aux.table.parse(token),
                    })
                    .unwrap_or_else(|| aux.table.fallback());
                AuxPayout {
                    coin: aux.coin,
                    payout,
                }
            })
            .collect();
        Payouts {
            parent: self.parent.resolve(username),
            aux,
        }
    }
}

/// The starting difficulty a miner asks for in its stratum password (`d=<number>`,
/// alongside anything else there, separated by commas or spaces), if it asks for a
/// usable one. Zero, negative, and non-numeric values are ignored.
pub fn password_difficulty(password: &str) -> Option<f64> {
    password
        .split([',', ' ', '\t'])
        .filter_map(|token| token.trim().split_once('='))
        .filter(|(key, _)| key.trim().eq_ignore_ascii_case("d"))
        .filter_map(|(_, value)| value.trim().parse::<f64>().ok())
        .find(|d| d.is_finite() && *d > 0.0)
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
    fn aux_address_comes_from_the_password() {
        let ltc = PayoutTable::new(
            AddressParams {
                p2pkh_prefix: 111,
                p2sh_prefixes: &[58, 196],
                bech32_hrp: Some("rltc"),
            },
            &encode_segwit("rltc", 0, &[9; 20]).unwrap(),
        )
        .unwrap();
        let doge_params = AddressParams {
            p2pkh_prefix: 111,
            p2sh_prefixes: &[196],
            bech32_hrp: None,
        };
        let doge_fallback = crate::address::encode_base58(111, &[7; 20]);
        let doge = PayoutTable::new(doge_params, &doge_fallback).unwrap();
        let set = PayoutSet {
            parent: ltc,
            aux: vec![AuxPayoutTable {
                coin: "DOGE",
                table: doge,
            }],
        };
        let mine = encode_segwit("rltc", 0, &[1; 20]).unwrap();
        let doge_mine = crate::address::encode_base58(111, &[2; 20]);

        let p = set.resolve(&format!("{mine}.rig"), &doge_mine);
        assert_eq!(p.parent.address, mine);
        assert_eq!(p.aux[0].payout.address, doge_mine);
        assert!(!p.aux[0].payout.fallback);

        let p = set.resolve(&mine, &format!("d=512, DOGE={doge_mine}"));
        assert_eq!(p.aux[0].payout.address, doge_mine);

        let p = set.resolve(&mine, "x");
        assert_eq!(p.aux[0].payout.address, doge_fallback);
        assert!(p.aux[0].payout.fallback);

        // A parent-chain address in the password is not a Dogecoin address.
        let p = set.resolve(&mine, &mine);
        assert!(p.aux[0].payout.fallback);
    }

    #[test]
    fn difficulty_comes_from_d_in_the_password() {
        assert_eq!(password_difficulty("d=4096"), Some(4096.0));
        assert_eq!(password_difficulty("x"), None);
        assert_eq!(password_difficulty(""), None);
        assert_eq!(password_difficulty("doge=DAddr,d=4096"), Some(4096.0));
        assert_eq!(password_difficulty("DAddr d=0.001"), Some(0.001));
        assert_eq!(password_difficulty("D=512, doge=DAddr"), Some(512.0));
        assert_eq!(password_difficulty("d = 2e3"), None, "spaces split tokens");
        assert_eq!(password_difficulty("d=2e3"), Some(2000.0));
        // Garbage is ignored, and a later good value still counts.
        for bad in ["d=", "d=abc", "d=0", "d=-5", "d=NaN", "d=inf", "dd=5", "d5"] {
            assert_eq!(password_difficulty(bad), None, "{bad}");
        }
        assert_eq!(password_difficulty("d=abc,d=64"), Some(64.0));
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
