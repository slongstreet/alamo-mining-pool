//! Merged mining (auxpow) primitives.
//!
//! The parent chain's coinbase scriptSig carries a commitment of the form
//! `magic || aux_merkle_root || merkle_size (u32 LE) || merkle_nonce (u32 LE)`.
//! An aux block is then proven by an `AuxPow` structure holding the parent coinbase,
//! its merkle branch to the parent merkle root, the branch from the aux block hash to
//! the aux merkle root, and the parent block header.
//!
//! Serialization and merkle tree construction land in Wave 2.

/// The merged-mining magic bytes (`"\xfa\xbemm"`) that prefix the aux commitment.
pub const MERGED_MINING_MAGIC: [u8; 4] = [0xfa, 0xbe, 0x6d, 0x6d];

/// Bit set in the block version of a chain that requires auxpow.
pub const VERSION_AUXPOW_FLAG: i32 = 1 << 8;

/// Build the block version for an aux chain: chain id in the upper 16 bits, the auxpow
/// flag, and the base version in the low 8 bits.
pub fn aux_block_version(base_version: i32, chain_id: u32) -> i32 {
    (base_version & 0xff) | VERSION_AUXPOW_FLAG | ((chain_id as i32) << 16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dogecoin_auxpow_version() {
        // Dogecoin auxpow blocks carry version 0x00620102 (chain id 98, auxpow flag, v2).
        assert_eq!(aux_block_version(2, crate::doge::CHAIN_ID), 0x0062_0102);
    }
}
