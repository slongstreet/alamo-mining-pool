//! Block header serialization.

use crate::hash::{sha256d, Hash256};

/// The 80-byte block header shared by all Bitcoin-derived chains.
///
/// Hashes are stored in internal (little-endian) byte order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockHeader {
    /// Block version.
    pub version: i32,
    /// Hash of the previous block.
    pub prev_hash: Hash256,
    /// Merkle root of the block's transactions.
    pub merkle_root: Hash256,
    /// Block timestamp (seconds since the Unix epoch).
    pub time: u32,
    /// Compact-encoded target.
    pub bits: u32,
    /// Nonce.
    pub nonce: u32,
}

impl BlockHeader {
    /// Serialized header length in bytes.
    pub const LEN: usize = 80;

    /// Serialize the header into its 80-byte wire form.
    pub fn serialize(&self) -> [u8; Self::LEN] {
        let mut out = [0u8; Self::LEN];
        out[0..4].copy_from_slice(&self.version.to_le_bytes());
        out[4..36].copy_from_slice(&self.prev_hash);
        out[36..68].copy_from_slice(&self.merkle_root);
        out[68..72].copy_from_slice(&self.time.to_le_bytes());
        out[72..76].copy_from_slice(&self.bits.to_le_bytes());
        out[76..80].copy_from_slice(&self.nonce.to_le_bytes());
        out
    }

    /// Parse an 80-byte wire header.
    pub fn deserialize(bytes: &[u8; Self::LEN]) -> Self {
        let mut prev_hash = [0u8; 32];
        let mut merkle_root = [0u8; 32];
        prev_hash.copy_from_slice(&bytes[4..36]);
        merkle_root.copy_from_slice(&bytes[36..68]);
        Self {
            version: i32::from_le_bytes(bytes[0..4].try_into().unwrap()),
            prev_hash,
            merkle_root,
            time: u32::from_le_bytes(bytes[68..72].try_into().unwrap()),
            bits: u32::from_le_bytes(bytes[72..76].try_into().unwrap()),
            nonce: u32::from_le_bytes(bytes[76..80].try_into().unwrap()),
        }
    }

    /// The block identity hash (double SHA-256 of the serialized header).
    pub fn block_hash(&self) -> Hash256 {
        sha256d(&self.serialize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::{from_display_hex, scrypt_pow, to_display_hex};
    use crate::target::Target;

    /// Litecoin mainnet genesis block.
    pub(crate) fn ltc_genesis() -> BlockHeader {
        BlockHeader {
            version: 1,
            prev_hash: [0u8; 32],
            merkle_root: from_display_hex(
                "97ddfbbae6be97fd6cdf3e7ca13232a3afff2353e29badfab7f73011edd4ced9",
            )
            .unwrap(),
            time: 1_317_972_665,
            bits: 0x1e0f_fff0,
            nonce: 2_084_524_493,
        }
    }

    #[test]
    fn serialize_round_trip() {
        let h = ltc_genesis();
        let bytes = h.serialize();
        assert_eq!(BlockHeader::deserialize(&bytes), h);
    }

    #[test]
    fn ltc_genesis_block_hash() {
        let h = ltc_genesis();
        assert_eq!(
            to_display_hex(&h.block_hash()),
            "12a765e31ffd4059bada1e25190f6e98c99d9714d334efa41a195a7e7e04bfe2"
        );
    }

    #[test]
    fn ltc_genesis_pow_hash_meets_its_target() {
        let h = ltc_genesis();
        let pow = scrypt_pow(&h.serialize());
        let target = Target::from_compact(h.bits);
        assert!(
            target.is_met_by(&pow),
            "genesis scrypt hash must satisfy its own bits"
        );
        assert_eq!(
            to_display_hex(&pow),
            "0000050c34a64b415b6b15b37f2216634b5b1669cb9a2e38d76f7213b0671e00"
        );
    }
}
