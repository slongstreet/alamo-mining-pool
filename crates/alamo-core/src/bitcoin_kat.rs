//! Known-answer tests against real Bitcoin mainnet blocks.
//!
//! Each block's proof of work authenticates it: every test first checks that the header
//! hashes to the block's known hash and that the hash meets the header's own `bits` at
//! mainnet difficulty, so altered test data cannot pass.
//!
//! - Block 699,412 (`0000000000000000000804d8316c8e8d975b60ed6b745b94e8788daf07ec49f3`,
//!   mined by AntPool in September 2021): 7 transactions, 5 of them segwit, 3,534 bytes,
//!   exactly as served by mempool.space and blockstream.info
//!   (`/api/block/<hash>/raw`, byte-identical from both). Header version `0x2fffe004`:
//!   BIP320 bits rolled over a template version of `0x20000004` (bit 2 was taproot's
//!   BIP9 signal, still set by Bitcoin Core while taproot was locked in).
//! - Block 759,892 (`000000000000000000041a1cd33b3dfe919aac8ba3e5c198c2f4f4722d0f521e`,
//!   October 2022), header only: version `0x20014000`, rolled over the plain
//!   `0x20000000` a template carries when nothing signals.

use crate::coinbase::CoinbaseParts;
use crate::encode::read_varint;
use crate::hash::{from_display_hex, sha256d, to_display_hex, Hash256};
use crate::header::{rolled_version, BlockHeader};
use crate::merkle;
use crate::target::Target;
use crate::Algorithm;

/// Bitcoin mainnet block 699,412, hex.
const BTC_699412: &str = include_str!("../testdata/btc-699412.hex");
const BTC_699412_HASH: &str = "0000000000000000000804d8316c8e8d975b60ed6b745b94e8788daf07ec49f3";

/// The 80-byte header of Bitcoin mainnet block 759,892.
const BTC_759892_HEADER: &str = "004001207b7d13e3a3c84a43d9fe3d1b0be288456139b2ea8589030000000000\
                                 0000000096d7b382d08e85575faf0bd7041f0365b862383097f503f804f1bf77\
                                 30073d375a9b546372e70717e2aa8b15";
const BTC_759892_HASH: &str = "000000000000000000041a1cd33b3dfe919aac8ba3e5c198c2f4f4722d0f521e";

/// BIP320's general-purpose version bits, the mask a Bitcoin lane offers under BIP310.
const BIP320_MASK: u32 = 0x1fff_e000;

/// One transaction of a block, split the ways the pool needs.
struct Tx {
    /// The serialization in the block (with witness data if any).
    raw: Vec<u8>,
    /// The non-witness serialization, whose double SHA-256 is the txid.
    stripped: Vec<u8>,
    /// Witness stacks, one per input; empty for a legacy transaction.
    witness: Vec<Vec<Vec<u8>>>,
}

/// Parse the transaction at the front of `bytes` (BIP144 serialization) and return it with
/// its length.
fn parse_tx(bytes: &[u8]) -> (Tx, usize) {
    let mut pos = 4;
    let segwit = bytes[4] == 0 && bytes[5] == 1;
    if segwit {
        pos += 2;
    }
    let body_start = pos;
    let varint = |pos: &mut usize| {
        let (v, used) = read_varint(&bytes[*pos..]);
        *pos += used;
        v as usize
    };
    let inputs = varint(&mut pos);
    for _ in 0..inputs {
        pos += 36;
        let len = varint(&mut pos);
        pos += len + 4;
    }
    let outputs = varint(&mut pos);
    for _ in 0..outputs {
        pos += 8;
        let len = varint(&mut pos);
        pos += len;
    }
    let body_end = pos;
    let mut witness = Vec::new();
    if segwit {
        for _ in 0..inputs {
            let items = varint(&mut pos);
            let mut stack = Vec::with_capacity(items);
            for _ in 0..items {
                let len = varint(&mut pos);
                stack.push(bytes[pos..pos + len].to_vec());
                pos += len;
            }
            witness.push(stack);
        }
    }
    let locktime = pos;
    pos += 4;
    let mut stripped = bytes[..4].to_vec();
    stripped.extend_from_slice(&bytes[body_start..body_end]);
    stripped.extend_from_slice(&bytes[locktime..pos]);
    (
        Tx {
            raw: bytes[..pos].to_vec(),
            stripped,
            witness,
        },
        pos,
    )
}

/// A block's header, after checking it is the block `hash` names and carries real work.
fn authenticated_header(header_bytes: &[u8], hash: &str) -> BlockHeader {
    let header = BlockHeader::deserialize(header_bytes[..80].try_into().unwrap());
    assert_eq!(header.serialize(), header_bytes[..80], "header round trip");
    let pow = Algorithm::Sha256d.pow_hash(&header.serialize());
    assert_eq!(
        pow,
        header.block_hash(),
        "SHA-256d PoW hash is the block hash"
    );
    assert_eq!(to_display_hex(&pow), hash);
    let target = Target::from_compact(header.bits);
    assert!(target.is_met_by(&pow), "the header meets its own bits");
    // Mainnet-scale work, not a regtest or made-up header.
    assert!(target.difficulty() > 1e12, "{}", target.difficulty());
    header
}

fn btc_699412() -> (BlockHeader, Vec<Tx>) {
    let bytes = hex::decode(BTC_699412.trim()).unwrap();
    let header = authenticated_header(&bytes, BTC_699412_HASH);
    let (count, used) = read_varint(&bytes[80..]);
    let mut pos = 80 + used;
    let mut txs = Vec::new();
    for _ in 0..count {
        let (tx, len) = parse_tx(&bytes[pos..]);
        pos += len;
        txs.push(tx);
    }
    assert_eq!(pos, bytes.len(), "no trailing bytes after the transactions");
    (header, txs)
}

#[test]
fn btc_699412_header_hash_and_proof_of_work() {
    let (header, txs) = btc_699412();
    assert_eq!(header.version, 0x2fff_e004);
    assert_eq!(header.bits, 0x170f_faa0);
    assert_eq!(
        to_display_hex(&header.prev_hash),
        "0000000000000000000658846305854047aae03c64791d58b26cdaddf75a37cd"
    );
    assert_eq!((header.time, header.nonce), (1_630_979_025, 1_697_221_118));
    assert_eq!(txs.len(), 7);
    assert_eq!(txs.iter().filter(|t| !t.witness.is_empty()).count(), 5);
}

#[test]
fn btc_699412_merkle_root_from_the_coinbase_branch() {
    let (header, txs) = btc_699412();
    let txids: Vec<Hash256> = txs.iter().map(|t| sha256d(&t.stripped)).collect();
    // A legacy transaction's txid is the hash of exactly what the block carries.
    assert!(txs
        .iter()
        .filter(|t| t.witness.is_empty())
        .all(|t| t.raw == t.stripped));
    // The stratum branch a template's txids give, folded over the coinbase txid, is the
    // header's merkle root: what share validation does for every share.
    let branch = merkle::coinbase_branch(&txids[1..]);
    assert_eq!(branch.len(), 3, "7 + 1 leaves need three levels");
    assert_eq!(
        merkle::root_from_branch(&txids[0], &branch),
        header.merkle_root
    );
    assert_eq!(merkle::merkle_root(&txids), header.merkle_root);
    assert_eq!(
        to_display_hex(&header.merkle_root),
        "dcb0e318bd65ed991d70205e8bd4a1902af55a02c70187bd0200cfa4d2434a36"
    );
}

#[test]
fn btc_699412_coinbase_and_witness_commitment() {
    let (header, txs) = btc_699412();
    let coinbase = &txs[0];
    // The coinbase witness is BIP141's single 32-byte reserved value of zeros.
    assert_eq!(coinbase.witness, vec![vec![vec![0u8; 32]]]);

    // Split the non-witness coinbase around 8 bytes of its scriptSig, as a stratum job
    // does around the extranonce, and rebuild both serializations from the parts.
    let script_at = 4 + 1 + 36 + 1;
    let (script_len, _) = read_varint(&coinbase.stripped[script_at - 1..]);
    let extranonce_at = script_at + script_len as usize - 8;
    let parts = CoinbaseParts {
        coinb1: coinbase.stripped[..extranonce_at].to_vec(),
        coinb2: coinbase.stripped[extranonce_at + 8..].to_vec(),
        extranonce_len: 8,
        witness: true,
    };
    let extranonce = &coinbase.stripped[extranonce_at..extranonce_at + 8];
    assert_eq!(parts.serialize(extranonce), coinbase.stripped);
    assert_eq!(parts.txid(extranonce), sha256d(&coinbase.stripped));
    assert_eq!(
        parts.serialize_for_block(extranonce),
        coinbase.raw,
        "the block form adds exactly the marker, flag, and reserved value"
    );
    assert_eq!(
        merkle::root_from_branch(
            &parts.txid(extranonce),
            &merkle::coinbase_branch(
                &txs[1..]
                    .iter()
                    .map(|t| sha256d(&t.stripped))
                    .collect::<Vec<_>>()
            )
        ),
        header.merkle_root
    );

    // The commitment output a template hands over as `default_witness_commitment` is
    // OP_RETURN aa21a9ed || sha256d(witness merkle root || reserved value), with the
    // coinbase's wtxid taken as zero.
    let wtxids: Vec<Hash256> = std::iter::once([0u8; 32])
        .chain(txs[1..].iter().map(|t| sha256d(&t.raw)))
        .collect();
    let mut preimage = merkle::merkle_root(&wtxids).to_vec();
    preimage.extend_from_slice(&[0u8; 32]);
    let mut expected = vec![0x6a, 0x24, 0xaa, 0x21, 0xa9, 0xed];
    expected.extend_from_slice(&sha256d(&preimage));
    assert!(
        coinbase
            .stripped
            .windows(expected.len())
            .any(|w| w == expected),
        "coinbase carries the witness commitment over the block's segwit transactions"
    );
}

#[test]
fn btc_699412_version_rolling_rebuilds_the_mined_header() {
    let (header, _) = btc_699412();
    // What a miner negotiating BIP310 against the BIP320 mask submits as version_bits.
    let template_version = 0x2000_0004;
    let version_bits = header.version as u32 & BIP320_MASK;
    assert_eq!(version_bits, 0x0fff_e000);
    let version = rolled_version(template_version, version_bits, BIP320_MASK);
    assert_eq!(version, header.version);
    let rebuilt = BlockHeader { version, ..header };
    assert_eq!(
        to_display_hex(&rebuilt.block_hash()),
        BTC_699412_HASH,
        "the rolled header is the mined one"
    );
    // Hashing the template's version instead gives some other, workless header.
    let unrolled = BlockHeader {
        version: template_version,
        ..header
    };
    assert!(!Target::from_compact(header.bits).is_met_by(&unrolled.block_hash()));
}

#[test]
fn btc_759892_version_rolling_over_a_plain_template_version() {
    let bytes = hex::decode(BTC_759892_HEADER).unwrap();
    let header = authenticated_header(&bytes, BTC_759892_HASH);
    assert_eq!(header.version, 0x2001_4000);
    assert_eq!(
        header.merkle_root,
        from_display_hex("373d073077bff104f803f597303862b865031f04d70baf5f57858ed082b3d796")
            .unwrap()
    );
    let version_bits = header.version as u32 & BIP320_MASK;
    assert_eq!(version_bits, 0x0001_4000);
    let rebuilt = BlockHeader {
        version: rolled_version(0x2000_0000, version_bits, BIP320_MASK),
        ..header
    };
    assert_eq!(rebuilt.serialize(), bytes[..80]);
    assert_eq!(to_display_hex(&rebuilt.block_hash()), BTC_759892_HASH);
}
