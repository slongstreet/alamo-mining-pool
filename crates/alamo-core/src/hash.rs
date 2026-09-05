//! Hash functions used for proof of work and block identity.

use sha2::{Digest, Sha256};

/// A 32-byte hash in internal (little-endian) byte order, as it appears on the wire.
pub type Hash256 = [u8; 32];

/// Double SHA-256, the block identity hash for Bitcoin-derived chains.
pub fn sha256d(data: &[u8]) -> Hash256 {
    let first = Sha256::digest(data);
    let second = Sha256::digest(first);
    let mut out = [0u8; 32];
    out.copy_from_slice(&second);
    out
}

/// scrypt(N=1024, r=1, p=1, dkLen=32) with the input as both password and salt.
///
/// This is the proof-of-work hash for Litecoin, Dogecoin, and other scrypt chains.
pub fn scrypt_pow(header: &[u8]) -> Hash256 {
    let params = scrypt::Params::new(10, 1, 1).expect("scrypt params are valid");
    let mut out = [0u8; 32];
    scrypt::scrypt(header, header, &params, &mut out).expect("output length is valid");
    out
}

/// Render a hash the way explorers and RPC do: byte-reversed, lowercase hex.
pub fn to_display_hex(hash: &Hash256) -> String {
    let mut reversed = *hash;
    reversed.reverse();
    hex::encode(reversed)
}

/// Parse a display-order (byte-reversed) hex string into internal byte order.
pub fn from_display_hex(s: &str) -> Result<Hash256, hex::FromHexError> {
    let bytes = hex::decode(s)?;
    let mut out: Hash256 = bytes
        .as_slice()
        .try_into()
        .map_err(|_| hex::FromHexError::InvalidStringLength)?;
    out.reverse();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256d_of_empty_input() {
        // Well-known: sha256d("") = 5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456
        let h = sha256d(b"");
        assert_eq!(
            hex::encode(h),
            "5df6e0e2761359d30a8275058e299fcc0381534545f55cf43e41983f5d4c9456"
        );
    }

    #[test]
    fn display_hex_round_trip() {
        let display = "12a765e31ffd4059bada1e25190f6e98c99d9714d334efa41a195a7e7e04bfe2";
        let internal = from_display_hex(display).unwrap();
        assert_eq!(to_display_hex(&internal), display);
        assert_eq!(internal[0], 0xe2);
        assert_eq!(internal[31], 0x12);
    }

    #[test]
    fn display_hex_rejects_wrong_length() {
        assert!(from_display_hex("abcd").is_err());
    }
}
