//! Address parsing for Bitcoin-derived coins, producing a scriptPubKey to pay.

use bitcoin::bech32::{self, Fe32, Hrp};

/// Address encoding parameters for one coin on one network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddressParams {
    /// Base58 version byte for pay-to-pubkey-hash addresses.
    pub p2pkh_prefix: u8,
    /// Base58 version bytes accepted for pay-to-script-hash addresses.
    pub p2sh_prefixes: &'static [u8],
    /// Bech32 human-readable part, if the coin supports segwit addresses.
    pub bech32_hrp: Option<&'static str>,
}

/// Why an address could not be turned into a script.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AddressError {
    /// Neither base58check nor bech32 decoded it.
    #[error("not a valid address")]
    Malformed,
    /// Decoded, but for a different coin or network.
    #[error("address is for a different coin or network")]
    WrongNetwork,
    /// The witness program has an invalid length for its version.
    #[error("invalid witness program")]
    BadWitnessProgram,
}

/// Decode `address` under `params` into the scriptPubKey that pays it.
pub fn payout_script(address: &str, params: &AddressParams) -> Result<Vec<u8>, AddressError> {
    let address = address.trim();
    if address.is_empty() {
        return Err(AddressError::Malformed);
    }
    if let Some(hrp) = params.bech32_hrp {
        if let Ok((got_hrp, version, program)) = bech32::segwit::decode(address) {
            if !got_hrp.as_str().eq_ignore_ascii_case(hrp) {
                return Err(AddressError::WrongNetwork);
            }
            return segwit_script(version.to_u8(), &program);
        }
        // Valid bech32 for some other hrp: say so rather than "malformed".
        if bech32::decode(address).is_ok() {
            return Err(AddressError::WrongNetwork);
        }
    }
    let payload = bitcoin::base58::decode_check(address).map_err(|_| AddressError::Malformed)?;
    if payload.len() != 21 {
        return Err(AddressError::Malformed);
    }
    let (version, hash) = (payload[0], &payload[1..]);
    if version == params.p2pkh_prefix {
        let mut s = Vec::with_capacity(25);
        s.extend_from_slice(&[0x76, 0xa9, 0x14]);
        s.extend_from_slice(hash);
        s.extend_from_slice(&[0x88, 0xac]);
        Ok(s)
    } else if params.p2sh_prefixes.contains(&version) {
        let mut s = Vec::with_capacity(23);
        s.extend_from_slice(&[0xa9, 0x14]);
        s.extend_from_slice(hash);
        s.push(0x87);
        Ok(s)
    } else {
        Err(AddressError::WrongNetwork)
    }
}

fn segwit_script(version: u8, program: &[u8]) -> Result<Vec<u8>, AddressError> {
    let valid = match version {
        0 => program.len() == 20 || program.len() == 32,
        1..=16 => (2..=40).contains(&program.len()),
        _ => false,
    };
    if !valid {
        return Err(AddressError::BadWitnessProgram);
    }
    let mut s = Vec::with_capacity(2 + program.len());
    s.push(if version == 0 { 0x00 } else { 0x50 + version });
    s.push(program.len() as u8);
    s.extend_from_slice(program);
    Ok(s)
}

/// Encode a segwit address. Useful for tests and for showing the fallback address.
pub fn encode_segwit(hrp: &str, version: u8, program: &[u8]) -> Result<String, AddressError> {
    let hrp = Hrp::parse(hrp).map_err(|_| AddressError::Malformed)?;
    let version = Fe32::try_from(version).map_err(|_| AddressError::BadWitnessProgram)?;
    bech32::segwit::encode(hrp, version, program).map_err(|_| AddressError::BadWitnessProgram)
}

/// Encode a base58check address from a version byte and 20-byte hash.
pub fn encode_base58(version: u8, hash160: &[u8; 20]) -> String {
    let mut payload = Vec::with_capacity(21);
    payload.push(version);
    payload.extend_from_slice(hash160);
    bitcoin::base58::encode_check(&payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LTC_MAIN: AddressParams = AddressParams {
        p2pkh_prefix: 48,
        p2sh_prefixes: &[50, 5],
        bech32_hrp: Some("ltc"),
    };
    const LTC_REGTEST: AddressParams = AddressParams {
        p2pkh_prefix: 111,
        p2sh_prefixes: &[58, 196],
        bech32_hrp: Some("rltc"),
    };
    const DOGE_MAIN: AddressParams = AddressParams {
        p2pkh_prefix: 30,
        p2sh_prefixes: &[22],
        bech32_hrp: None,
    };

    #[test]
    fn litecoin_p2pkh() {
        // Litecoin Foundation donation address (well known, starts with L).
        let script = payout_script("LTdsVS8VDw6syvfQADdhf2PHAm3rMGJvPX", &LTC_MAIN).unwrap();
        assert_eq!(script.len(), 25);
        assert_eq!(&script[..3], &[0x76, 0xa9, 0x14]);
        assert_eq!(&script[23..], &[0x88, 0xac]);
    }

    #[test]
    fn litecoin_bech32_round_trip() {
        let program = [0x42u8; 20];
        let addr = encode_segwit("ltc", 0, &program).unwrap();
        assert!(addr.starts_with("ltc1q"));
        let script = payout_script(&addr, &LTC_MAIN).unwrap();
        assert_eq!(script[0], 0x00);
        assert_eq!(script[1], 20);
        assert_eq!(&script[2..], &program);
        // Regtest params reject a mainnet address and vice versa.
        assert_eq!(
            payout_script(&addr, &LTC_REGTEST),
            Err(AddressError::WrongNetwork)
        );
        let raddr = encode_segwit("rltc", 0, &program).unwrap();
        assert!(payout_script(&raddr, &LTC_REGTEST).is_ok());
        assert_eq!(
            payout_script(&raddr, &LTC_MAIN),
            Err(AddressError::WrongNetwork)
        );
    }

    #[test]
    fn taproot_style_program() {
        let program = [0x77u8; 32];
        let addr = encode_segwit("ltc", 1, &program).unwrap();
        let script = payout_script(&addr, &LTC_MAIN).unwrap();
        assert_eq!(script[0], 0x51);
        assert_eq!(script[1], 32);
    }

    #[test]
    fn dogecoin_p2pkh_and_wrong_coin() {
        let addr = encode_base58(30, &[0x11; 20]);
        assert!(addr.starts_with('D'));
        let script = payout_script(&addr, &DOGE_MAIN).unwrap();
        assert_eq!(script.len(), 25);
        assert_eq!(
            payout_script(&addr, &LTC_MAIN),
            Err(AddressError::WrongNetwork)
        );
        assert_eq!(
            payout_script("ltc1qxyz", &DOGE_MAIN),
            Err(AddressError::Malformed)
        );
    }

    #[test]
    fn garbage_is_malformed() {
        assert_eq!(payout_script("", &LTC_MAIN), Err(AddressError::Malformed));
        assert_eq!(
            payout_script("not-an-address", &LTC_MAIN),
            Err(AddressError::Malformed)
        );
        assert_eq!(
            payout_script("LTdsVS8VDw6syvfQADdhf2PHAm3rMGJvPY", &LTC_MAIN),
            Err(AddressError::Malformed)
        );
    }
}
