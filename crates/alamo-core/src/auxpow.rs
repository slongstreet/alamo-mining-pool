//! Merged mining (auxpow) primitives, as implemented by Namecoin and Dogecoin.
//!
//! The parent chain's coinbase scriptSig carries a commitment of the form
//! `magic || aux_merkle_root || merkle_size (u32 LE) || merkle_nonce (u32 LE)`, where the
//! root is written in display (reversed) byte order. An aux block then proves its work with
//! an [`AuxPow`]: the parent coinbase, its merkle branch to the parent merkle root, the
//! branch from the aux block hash to the aux merkle root, and the parent block header.
//!
//! Every byte here is checked by `CAuxPow::check` in the aux node, so this module is
//! covered by a known-answer test against a real Dogecoin block.

use crate::encode::{varint_len, write_varint};
use crate::hash::Hash256;
use crate::header::BlockHeader;
use crate::merkle;

/// The merged-mining magic bytes (`"\xfa\xbemm"`) that prefix the aux commitment.
pub const MERGED_MINING_MAGIC: [u8; 4] = [0xfa, 0xbe, 0x6d, 0x6d];

/// Bit set in the block version of an aux chain block that carries an auxpow.
pub const VERSION_AUXPOW_FLAG: i32 = 1 << 8;

/// Length of the commitment placed in the parent coinbase.
pub const COMMITMENT_LEN: usize = 4 + 32 + 4 + 4;

/// Longest chain merkle branch an aux node accepts.
pub const MAX_CHAIN_BRANCH_LEN: usize = 30;

/// Build the block version for an aux chain block: the low 8 bits of `version` (the base
/// version), the auxpow flag, and the chain id in the upper 16 bits.
pub fn aux_block_version(version: i32, chain_id: u32) -> i32 {
    (version & 0xff) | VERSION_AUXPOW_FLAG | ((chain_id as i32) << 16)
}

/// Chain id encoded in an aux chain block version.
pub fn chain_id_of(version: i32) -> u32 {
    (version >> 16) as u32
}

/// The slot an aux chain must occupy in a chain merkle tree of `2^height` leaves. Mirrors
/// `CAuxPow::getExpectedIndex`.
pub fn expected_index(nonce: u32, chain_id: u32, height: u32) -> u32 {
    let mut rand = nonce;
    rand = rand.wrapping_mul(1_103_515_245).wrapping_add(12345);
    rand = rand.wrapping_add(chain_id);
    rand = rand.wrapping_mul(1_103_515_245).wrapping_add(12345);
    rand % (1u32 << height)
}

/// The aux chain merkle tree committed to in the parent coinbase.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuxTree {
    /// Merkle root over the leaves, internal byte order.
    pub root: Hash256,
    /// Number of leaves (a power of two).
    pub size: u32,
    /// Nonce that places every chain in a distinct slot.
    pub nonce: u32,
    /// Per chain, in the order given to [`AuxTree::build`]: slot index and merkle branch.
    pub proofs: Vec<ChainProof>,
}

/// Where one aux chain's block hash sits in the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainProof {
    /// Leaf index.
    pub index: u32,
    /// Merkle branch from the leaf to the root.
    pub branch: Vec<Hash256>,
}

/// Why a tree could not be built.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuxTreeError {
    /// No chains were given.
    #[error("no aux chains")]
    Empty,
    /// Too many chains for the maximum branch length.
    #[error("too many aux chains")]
    TooMany,
    /// The same chain id appears twice.
    #[error("duplicate aux chain id {0}")]
    DuplicateChain(u32),
}

impl AuxTree {
    /// Build a tree for `(chain_id, aux_block_hash)` pairs. Slots are assigned by
    /// [`expected_index`]; the smallest nonce that gives every chain its own slot is used.
    pub fn build(chains: &[(u32, Hash256)]) -> Result<Self, AuxTreeError> {
        if chains.is_empty() {
            return Err(AuxTreeError::Empty);
        }
        for (i, (id, _)) in chains.iter().enumerate() {
            if chains[..i].iter().any(|(other, _)| other == id) {
                return Err(AuxTreeError::DuplicateChain(*id));
            }
        }
        let mut height = (chains.len() as u32).next_power_of_two().trailing_zeros();
        loop {
            if height as usize > MAX_CHAIN_BRANCH_LEN {
                return Err(AuxTreeError::TooMany);
            }
            let size = 1u32 << height;
            // Slots collide only with unlucky nonces; widening the tree makes them rarer.
            for nonce in 0..64u32 {
                let slots: Vec<u32> = chains
                    .iter()
                    .map(|(id, _)| expected_index(nonce, *id, height))
                    .collect();
                let distinct = slots
                    .iter()
                    .enumerate()
                    .all(|(i, s)| !slots[..i].contains(s));
                if !distinct {
                    continue;
                }
                let mut leaves = vec![[0u8; 32]; size as usize];
                for ((_, hash), slot) in chains.iter().zip(&slots) {
                    leaves[*slot as usize] = *hash;
                }
                let proofs = slots
                    .iter()
                    .map(|slot| ChainProof {
                        index: *slot,
                        branch: merkle::branch_at(&leaves, *slot as usize),
                    })
                    .collect();
                return Ok(Self {
                    root: merkle::merkle_root(&leaves),
                    size,
                    nonce,
                    proofs,
                });
            }
            height += 1;
        }
    }

    /// A tree holding one aux chain: the block hash is the root.
    pub fn single(aux_block_hash: Hash256) -> Self {
        Self {
            root: aux_block_hash,
            size: 1,
            nonce: 0,
            proofs: vec![ChainProof {
                index: 0,
                branch: Vec::new(),
            }],
        }
    }

    /// The bytes to place in the parent coinbase scriptSig.
    pub fn commitment(&self) -> [u8; COMMITMENT_LEN] {
        let mut out = [0u8; COMMITMENT_LEN];
        out[..4].copy_from_slice(&MERGED_MINING_MAGIC);
        let mut root = self.root;
        root.reverse();
        out[4..36].copy_from_slice(&root);
        out[36..40].copy_from_slice(&self.size.to_le_bytes());
        out[40..44].copy_from_slice(&self.nonce.to_le_bytes());
        out
    }
}

/// Proof that an aux block was mined as part of a parent block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuxPow<'a> {
    /// Parent coinbase transaction, non-witness serialization.
    pub coinbase: &'a [u8],
    /// Hash of the parent block, internal byte order.
    pub parent_hash: Hash256,
    /// Merkle branch from the coinbase to the parent merkle root.
    pub coinbase_branch: &'a [Hash256],
    /// Merkle branch from the aux block hash to the aux tree root.
    pub chain_branch: &'a [Hash256],
    /// Index of the aux block in the aux tree.
    pub chain_index: u32,
    /// The parent block header carrying the proof of work.
    pub parent_header: BlockHeader,
}

impl AuxPow<'_> {
    /// Serialized length.
    pub fn len(&self) -> usize {
        self.coinbase.len()
            + 32
            + varint_len(self.coinbase_branch.len() as u64)
            + 32 * self.coinbase_branch.len()
            + 4
            + varint_len(self.chain_branch.len() as u64)
            + 32 * self.chain_branch.len()
            + 4
            + BlockHeader::LEN
    }

    /// Always false: an auxpow carries at least a coinbase and a header.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Append the wire form: `CMerkleTx` (tx, hashBlock, vMerkleBranch, nIndex = 0), then
    /// `vChainMerkleBranch`, `nChainIndex`, and the parent header.
    pub fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(self.coinbase);
        out.extend_from_slice(&self.parent_hash);
        write_varint(out, self.coinbase_branch.len() as u64);
        for h in self.coinbase_branch {
            out.extend_from_slice(h);
        }
        out.extend_from_slice(&0i32.to_le_bytes());
        write_varint(out, self.chain_branch.len() as u64);
        for h in self.chain_branch {
            out.extend_from_slice(h);
        }
        out.extend_from_slice(&(self.chain_index as i32).to_le_bytes());
        out.extend_from_slice(&self.parent_header.serialize());
    }

    /// The wire form as a new vector.
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.len());
        self.write(&mut out);
        out
    }
}

/// An auxpow decoded from its wire form. Test support only: the pool never parses one.
#[cfg(any(test, feature = "test-util"))]
#[derive(Clone, Debug)]
pub struct DecodedAuxPow {
    /// Parent coinbase bytes.
    pub coinbase: Vec<u8>,
    /// `hashBlock`.
    pub parent_hash: Hash256,
    /// Coinbase merkle branch.
    pub coinbase_branch: Vec<Hash256>,
    /// Chain merkle branch.
    pub chain_branch: Vec<Hash256>,
    /// Chain index.
    pub chain_index: u32,
    /// Parent header.
    pub parent_header: BlockHeader,
    /// Bytes consumed.
    pub len: usize,
}

/// Length of the non-witness transaction at the front of `bytes`. Test support only.
#[cfg(any(test, feature = "test-util"))]
pub fn tx_len(bytes: &[u8]) -> usize {
    use crate::encode::read_varint;
    let mut pos = 4;
    let (n_in, used) = read_varint(&bytes[pos..]);
    pos += used;
    for _ in 0..n_in {
        pos += 36;
        let (script_len, used) = read_varint(&bytes[pos..]);
        pos += used + script_len as usize + 4;
    }
    let (n_out, used) = read_varint(&bytes[pos..]);
    pos += used;
    for _ in 0..n_out {
        pos += 8;
        let (script_len, used) = read_varint(&bytes[pos..]);
        pos += used + script_len as usize;
    }
    pos + 4
}

/// Decode an auxpow from the front of `bytes`. Test support only; panics on bad input.
#[cfg(any(test, feature = "test-util"))]
pub fn decode_for_test(bytes: &[u8]) -> DecodedAuxPow {
    use crate::encode::read_varint;
    let mut pos = tx_len(bytes);
    let coinbase = bytes[..pos].to_vec();
    let mut parent_hash = [0u8; 32];
    parent_hash.copy_from_slice(&bytes[pos..pos + 32]);
    pos += 32;
    let read_hashes = |pos: &mut usize| {
        let (n, used) = read_varint(&bytes[*pos..]);
        *pos += used;
        (0..n)
            .map(|_| {
                let mut h = [0u8; 32];
                h.copy_from_slice(&bytes[*pos..*pos + 32]);
                *pos += 32;
                h
            })
            .collect::<Vec<_>>()
    };
    let coinbase_branch = read_hashes(&mut pos);
    assert_eq!(&bytes[pos..pos + 4], &[0, 0, 0, 0], "nIndex must be 0");
    pos += 4;
    let chain_branch = read_hashes(&mut pos);
    let chain_index = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap());
    pos += 4;
    let parent_header = BlockHeader::deserialize(bytes[pos..pos + 80].try_into().unwrap());
    pos += 80;
    DecodedAuxPow {
        coinbase,
        parent_hash,
        coinbase_branch,
        chain_branch,
        chain_index,
        parent_header,
        len: pos,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::{sha256d, to_display_hex};
    use crate::target::Target;
    use crate::Algorithm;

    #[test]
    fn dogecoin_auxpow_version() {
        // Dogecoin auxpow blocks carry version 0x00620104 (chain id 98, auxpow flag, v4).
        assert_eq!(aux_block_version(4, 98), 0x0062_0104);
        // Applying it to a version that already has the chain id is idempotent.
        assert_eq!(aux_block_version(0x0062_0004, 98), 0x0062_0104);
        assert_eq!(chain_id_of(0x0062_0104), 98);
    }

    #[test]
    fn expected_index_matches_reference() {
        // Height 0 trees have one slot.
        assert_eq!(expected_index(0, 98, 0), 0);
        assert_eq!(expected_index(12345, 98, 0), 0);
        // Reference values computed with the Dogecoin formula.
        let mut r: u32 = 7;
        r = r.wrapping_mul(1_103_515_245).wrapping_add(12345);
        r = r.wrapping_add(98);
        r = r.wrapping_mul(1_103_515_245).wrapping_add(12345);
        assert_eq!(expected_index(7, 98, 4), r % 16);
    }

    #[test]
    fn single_chain_tree_commits_reversed_hash() {
        let mut hash = [0u8; 32];
        hash[0] = 0xaa;
        hash[31] = 0xbb;
        let tree = AuxTree::single(hash);
        let c = tree.commitment();
        assert_eq!(&c[..4], &MERGED_MINING_MAGIC);
        assert_eq!(c[4], 0xbb);
        assert_eq!(c[35], 0xaa);
        assert_eq!(&c[36..40], &[1, 0, 0, 0]);
        assert_eq!(&c[40..44], &[0, 0, 0, 0]);
        assert_eq!(AuxTree::build(&[(98, hash)]).unwrap(), tree);
    }

    #[test]
    fn multi_chain_tree_places_each_chain_in_its_slot() {
        let chains: Vec<(u32, Hash256)> = (1..=3u32)
            .map(|i| {
                let mut h = [0u8; 32];
                h[0] = i as u8;
                (i * 7, h)
            })
            .collect();
        let tree = AuxTree::build(&chains).unwrap();
        assert!(tree.size >= 4);
        let height = tree.size.trailing_zeros();
        for ((id, hash), proof) in chains.iter().zip(&tree.proofs) {
            assert_eq!(proof.index, expected_index(tree.nonce, *id, height));
            assert_eq!(proof.branch.len() as u32, height);
            assert_eq!(
                merkle::root_from_branch_at(hash, &proof.branch, proof.index as usize),
                tree.root
            );
        }
        assert_eq!(
            AuxTree::build(&[(1, [0; 32]), (1, [1; 32])]),
            Err(AuxTreeError::DuplicateChain(1))
        );
        assert_eq!(AuxTree::build(&[]), Err(AuxTreeError::Empty));
    }

    /// Dogecoin mainnet block 371,337, the first merge-mined block, exactly as served by
    /// the network: header, auxpow, then six transactions. Every field below is what
    /// `CAuxPow::check` verifies.
    const DOGE_371337: &str = include_str!("../testdata/doge-371337.hex");

    #[test]
    fn dogecoin_block_371337_auxpow_round_trips() {
        use crate::encode::read_varint;
        use crate::hash::from_display_hex;

        let bytes = hex::decode(DOGE_371337.trim()).unwrap();
        let header = BlockHeader::deserialize(bytes[..80].try_into().unwrap());
        assert_eq!(header.version, 0x0062_0102);
        assert_eq!(chain_id_of(header.version), 98);
        let aux_hash = header.block_hash();
        assert_eq!(
            to_display_hex(&aux_hash),
            "60323982f9c5ff1b5a954eac9dc1269352835f47c2c5222691d80f0d50dcf053"
        );

        let decoded = decode_for_test(&bytes[80..]);
        let auxpow_end = 80 + decoded.len;
        assert_eq!(
            to_display_hex(&decoded.parent_hash),
            "0000000000192392f32b46c8116f212fd698f7181b8f499c2096f5ff024ee6b3"
        );
        assert_eq!(decoded.coinbase_branch.len(), 3);
        assert_eq!(decoded.chain_branch.len(), 3);
        assert_eq!(decoded.chain_index, 0);

        // Re-serializing reproduces the network bytes exactly.
        let auxpow = AuxPow {
            coinbase: &decoded.coinbase,
            parent_hash: decoded.parent_hash,
            coinbase_branch: &decoded.coinbase_branch,
            chain_branch: &decoded.chain_branch,
            chain_index: decoded.chain_index,
            parent_header: decoded.parent_header,
        };
        assert_eq!(auxpow.serialize(), &bytes[80..auxpow_end]);
        assert_eq!(auxpow.len(), decoded.len);

        // The aux block's own transactions follow the auxpow and hash to its merkle root.
        let (n_tx, used) = read_varint(&bytes[auxpow_end..]);
        assert_eq!(n_tx, 6);
        let mut pos = auxpow_end + used;
        let mut txids = Vec::new();
        for _ in 0..n_tx {
            let len = tx_len(&bytes[pos..]);
            txids.push(sha256d(&bytes[pos..pos + len]));
            pos += len;
        }
        assert_eq!(pos, bytes.len(), "trailing bytes after transactions");
        assert_eq!(merkle::merkle_root(&txids), header.merkle_root);

        // The parent coinbase is in the parent merkle tree at index 0.
        let root = merkle::root_from_branch(&sha256d(&decoded.coinbase), &decoded.coinbase_branch);
        assert_eq!(root, decoded.parent_header.merkle_root);
        // `hashBlock` is not consensus-checked; the pool that found this block wrote an
        // unrelated Litecoin hash there, so it is only round-tripped, never derived.
        assert_eq!(
            to_display_hex(&decoded.parent_header.block_hash()),
            "45df41e40aba5b2a03d08bd1202a1c02ef3954d8aa22ea6c5ae62fd00f290ea9"
        );
        assert_ne!(decoded.parent_header.block_hash(), decoded.parent_hash);

        // The parent's scrypt hash satisfies the aux block's own target.
        let pow = Algorithm::Scrypt.pow_hash(&decoded.parent_header.serialize());
        assert!(Target::from_compact(header.bits).is_met_by(&pow));

        // The chain merkle root appears right after the magic, in display order, followed
        // by the tree size and nonce that put this chain at `chain_index`.
        let chain_root = merkle::root_from_branch_at(
            &aux_hash,
            &decoded.chain_branch,
            decoded.chain_index as usize,
        );
        assert_eq!(
            chain_root,
            from_display_hex("980ba42120410de0554d42a5b5ee58167bcd86bf7591f429005f24da45fb51cf")
                .unwrap()
        );
        let script = coinbase_script_sig(&decoded.coinbase);
        let magic_at = script
            .windows(4)
            .position(|w| w == MERGED_MINING_MAGIC)
            .expect("magic in coinbase");
        let mut expected_root = chain_root;
        expected_root.reverse();
        assert_eq!(&script[magic_at + 4..magic_at + 36], &expected_root);
        let size = u32::from_le_bytes(script[magic_at + 36..magic_at + 40].try_into().unwrap());
        let nonce = u32::from_le_bytes(script[magic_at + 40..magic_at + 44].try_into().unwrap());
        let height = decoded.chain_branch.len() as u32;
        assert_eq!(size, 8);
        assert_eq!(size, 1 << height);
        assert_eq!(decoded.chain_index, expected_index(nonce, 98, height));

        // Our own commitment for the same tree is byte-identical.
        let tree = AuxTree {
            root: chain_root,
            size,
            nonce,
            proofs: vec![],
        };
        assert_eq!(
            &script[magic_at..magic_at + COMMITMENT_LEN],
            &tree.commitment()
        );
    }

    fn coinbase_script_sig(tx: &[u8]) -> &[u8] {
        use crate::encode::read_varint;
        let (_, used) = read_varint(&tx[4..]);
        let pos = 4 + used + 36;
        let (len, used) = read_varint(&tx[pos..]);
        &tx[pos + used..pos + used + len as usize]
    }
}
