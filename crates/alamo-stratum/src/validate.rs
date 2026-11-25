//! Share validation: rebuild the header a miner hashed and check it against targets.

use crate::events::BlockCandidate;
use crate::job::{SessionJob, EXTRANONCE1_LEN, EXTRANONCE2_LEN};
use alamo_core::auxpow::AuxPow;
use alamo_core::hash::to_display_hex;
use alamo_core::header::{rolled_version, BlockHeader};
use alamo_core::job::{RejectReason, ShareOutcome};

/// The fields of a `mining.submit`, already parsed from hex.
#[derive(Clone, Debug)]
pub struct Submit<'a> {
    /// Worker name.
    pub worker: &'a str,
    /// extranonce2 bytes; `None` when the miner sent valid hex of the wrong length.
    pub extranonce2: Option<[u8; EXTRANONCE2_LEN]>,
    /// Header time.
    pub ntime: u32,
    /// Header nonce.
    pub nonce: u32,
    /// BIP310 version bits (the optional sixth parameter); `None` hashes the job's own
    /// version.
    pub version_bits: Option<u32>,
}

/// Maximum seconds a share's ntime may be ahead of the current time.
const MAX_FUTURE_NTIME: u64 = 7_200;

/// Validate a submit against a session job and its extranonce1. `version_mask` is the
/// version-rolling mask negotiated for the session (zero when none was).
///
/// Returns the outcome and a block candidate for every chain whose target the share met.
pub fn validate(
    job: &mut SessionJob,
    extranonce1: &[u8; EXTRANONCE1_LEN],
    submit: &Submit<'_>,
    version_mask: u32,
    payout_address: &str,
    now_unix: u64,
) -> (ShareOutcome, Vec<BlockCandidate>) {
    if job.stale {
        return (ShareOutcome::Rejected(RejectReason::StaleJob), Vec::new());
    }
    let extranonce2 = match submit.extranonce2 {
        Some(en2) if EXTRANONCE1_LEN + EXTRANONCE2_LEN == job.coinbase.extranonce_len => en2,
        _ => {
            return (
                ShareOutcome::Rejected(RejectReason::InvalidExtranonce2),
                Vec::new(),
            )
        }
    };
    if u64::from(submit.ntime) < u64::from(job.work.min_time)
        || u64::from(submit.ntime) > now_unix + MAX_FUTURE_NTIME
    {
        return (
            ShareOutcome::Rejected(RejectReason::InvalidNtime),
            Vec::new(),
        );
    }
    let version = match submit.version_bits {
        Some(bits) if bits & !version_mask != 0 => {
            return (
                ShareOutcome::Rejected(RejectReason::InvalidVersionBits),
                Vec::new(),
            );
        }
        Some(bits) => rolled_version(job.work.version, bits, version_mask),
        None => job.work.version,
    };
    let en2 = u32::from_be_bytes(extranonce2);
    if !job
        .seen
        .insert((en2, submit.ntime, submit.nonce, version as u32))
    {
        return (ShareOutcome::Rejected(RejectReason::Duplicate), Vec::new());
    }

    let mut extranonce = [0u8; EXTRANONCE1_LEN + EXTRANONCE2_LEN];
    extranonce[..EXTRANONCE1_LEN].copy_from_slice(extranonce1);
    extranonce[EXTRANONCE1_LEN..].copy_from_slice(&extranonce2);

    let coinbase_txid = job.coinbase.txid(&extranonce);
    let header = BlockHeader {
        version,
        prev_hash: job.work.prev_hash,
        merkle_root: job.work.merkle_root(&coinbase_txid),
        time: submit.ntime,
        bits: job.work.bits,
        nonce: submit.nonce,
    };
    let pow_hash = job.work.algorithm.pow_hash(&header.serialize());
    let difficulty = job.work.algorithm.share_difficulty(&pow_hash);

    if !job.target.is_met_by(&pow_hash) {
        return (
            ShareOutcome::Rejected(RejectReason::LowDifficulty),
            Vec::new(),
        );
    }

    let mut candidates = Vec::new();
    if job.work.target().is_met_by(&pow_hash) {
        let coinbase = job.coinbase.serialize_for_block(&extranonce);
        candidates.push(BlockCandidate {
            coin: job.work.coin,
            height: job.work.height,
            block_hash: to_display_hex(&header.block_hash()),
            pow_hash,
            worker: submit.worker.to_string(),
            address: payout_address.to_string(),
            block: job.work.assemble_block(&header, &[], &coinbase),
            network_difficulty: job.work.network_difficulty(),
            share_difficulty: difficulty,
            coinbase_value: job.work.coinbase_value,
            found_at: now_unix,
        });
    }
    for aux in job.aux.iter().filter(|a| !a.stale) {
        if !aux.work.target().is_met_by(&pow_hash) {
            continue;
        }
        let parent_coinbase = job.coinbase.serialize(&extranonce);
        let auxpow = AuxPow {
            coinbase: &parent_coinbase,
            parent_hash: header.block_hash(),
            coinbase_branch: &job.work.merkle_branch,
            chain_branch: &aux.chain_branch,
            chain_index: aux.chain_index,
            parent_header: header,
        };
        candidates.push(BlockCandidate {
            coin: aux.work.coin,
            height: aux.work.height,
            block_hash: to_display_hex(&aux.hash),
            pow_hash,
            worker: submit.worker.to_string(),
            address: aux.address.clone(),
            block: aux
                .work
                .assemble_block(&aux.header, &auxpow.serialize(), &aux.coinbase),
            network_difficulty: aux.work.network_difficulty(),
            share_difficulty: difficulty,
            coinbase_value: aux.work.coinbase_value,
            found_at: now_unix,
        });
    }

    if candidates.is_empty() {
        (ShareOutcome::Accepted { difficulty }, candidates)
    } else {
        (ShareOutcome::Block { difficulty }, candidates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::SessionJob;
    use alamo_core::coinbase::CoinbaseParts;
    use alamo_core::job::JobId;
    use alamo_core::work::WorkTemplate;
    use std::collections::HashSet;
    use std::sync::Arc;

    /// A stratum difficulty far below the regtest network target (network difficulty
    /// 1e-7 expressed in scrypt share units), so every accepted share is a block.
    const TINY: f64 = 1e-7 * 65536.0;

    fn job(difficulty: f64) -> SessionJob {
        let work = Arc::new(WorkTemplate::regtest_sample(1, None));
        let coinbase = CoinbaseParts::build(&work, &[0x51], &[], 8).unwrap();
        let target = work.algorithm.share_target(difficulty);
        SessionJob {
            id: JobId(7),
            work,
            coinbase,
            aux: Vec::new(),
            difficulty,
            target,
            stale: false,
            seen: HashSet::new(),
        }
    }

    /// A job whose parent coinbase commits to one aux block.
    fn merged_job(difficulty: f64) -> SessionJob {
        use crate::job::AuxJob;
        use alamo_core::auxpow::AuxTree;
        use alamo_core::hash::sha256d;
        let aux_work = Arc::new(WorkTemplate::regtest_aux_sample(31));
        let aux_coinbase = CoinbaseParts::build(&aux_work, &[0x52], &[], 0)
            .unwrap()
            .serialize(&[]);
        let header = BlockHeader {
            version: aux_work.version,
            prev_hash: aux_work.prev_hash,
            merkle_root: aux_work.merkle_root(&sha256d(&aux_coinbase)),
            time: aux_work.cur_time,
            bits: aux_work.bits,
            nonce: 0,
        };
        let hash = header.block_hash();
        let tree = AuxTree::single(hash);
        let work = Arc::new(WorkTemplate::regtest_sample(1, None));
        let coinbase = CoinbaseParts::build(&work, &[0x51], &tree.commitment(), 8).unwrap();
        let target = work.algorithm.share_target(difficulty);
        SessionJob {
            id: JobId(8),
            work,
            coinbase,
            aux: vec![AuxJob {
                work: aux_work,
                address: "D".into(),
                coinbase: aux_coinbase,
                header,
                hash,
                chain_index: 0,
                chain_branch: Vec::new(),
                stale: false,
            }],
            difficulty,
            target,
            stale: false,
            seen: HashSet::new(),
        }
    }

    fn submit(nonce: u32) -> Submit<'static> {
        Submit {
            worker: "w",
            extranonce2: Some([0, 0, 0, 1]),
            ntime: 1_700_000_000,
            nonce,
            version_bits: None,
        }
    }

    /// Find a nonce whose scrypt hash satisfies the (tiny) regtest share target.
    fn mine(job: &mut SessionJob) -> u32 {
        let nonce = (0u32..)
            .find(|&nonce| {
                let (outcome, _) =
                    validate(job, &[1, 2, 3, 4], &submit(nonce), 0, "addr", 1_700_000_100);
                !matches!(outcome, ShareOutcome::Rejected(RejectReason::LowDifficulty))
            })
            .unwrap();
        job.seen.clear();
        nonce
    }

    #[test]
    fn regtest_share_is_a_block_and_duplicates_are_rejected() {
        let mut j = job(TINY);
        let nonce = mine(&mut j);
        let (outcome, block) = validate(
            &mut j,
            &[1, 2, 3, 4],
            &submit(nonce),
            0,
            "addr",
            1_700_000_100,
        );
        assert!(matches!(outcome, ShareOutcome::Block { .. }), "{outcome:?}");
        assert_eq!(block.len(), 1);
        let block = &block[0];
        assert_eq!(block.height, 1);
        assert_eq!(&block.block[..4], &0x2000_0000i32.to_le_bytes());
        assert_eq!(block.block[80], 1); // one transaction
                                        // Header prev hash and the coinbase's extranonce are embedded.
        assert_eq!(&block.block[4..36], &[0x11; 32]);
        assert!(block
            .block
            .windows(8)
            .any(|w| w == [1, 2, 3, 4, 0, 0, 0, 1]));

        let (dup, _) = validate(
            &mut j,
            &[1, 2, 3, 4],
            &submit(nonce),
            0,
            "addr",
            1_700_000_100,
        );
        assert_eq!(dup, ShareOutcome::Rejected(RejectReason::Duplicate));
    }

    #[test]
    fn rejects_stale_bad_ntime_and_bad_extranonce() {
        let mut j = job(TINY);
        j.stale = true;
        assert_eq!(
            validate(&mut j, &[0; 4], &submit(1), 0, "a", 1_700_000_100).0,
            ShareOutcome::Rejected(RejectReason::StaleJob)
        );
        let mut j = job(TINY);
        let mut s = submit(1);
        s.ntime = 1;
        assert_eq!(
            validate(&mut j, &[0; 4], &s, 0, "a", 1_700_000_100).0,
            ShareOutcome::Rejected(RejectReason::InvalidNtime)
        );
        let mut s = submit(1);
        s.extranonce2 = None;
        assert_eq!(
            validate(&mut j, &[0; 4], &s, 0, "a", 1_700_000_100).0,
            ShareOutcome::Rejected(RejectReason::InvalidExtranonce2)
        );
    }

    #[test]
    fn merged_share_yields_parent_and_aux_blocks() {
        use alamo_core::auxpow::{decode_for_test, MERGED_MINING_MAGIC};
        let mut j = merged_job(TINY);
        let nonce = mine(&mut j);
        let (outcome, blocks) = validate(
            &mut j,
            &[1, 2, 3, 4],
            &submit(nonce),
            0,
            "addr",
            1_700_000_100,
        );
        assert!(matches!(outcome, ShareOutcome::Block { .. }));
        assert_eq!(blocks.len(), 2);
        let (ltc, doge) = (&blocks[0], &blocks[1]);
        assert_eq!(ltc.coin, "LTC");
        assert_eq!(ltc.coinbase_value, j.work.coinbase_value);
        assert_eq!(doge.coin, "DOGE");
        assert_eq!(doge.height, 31);
        assert_eq!(doge.coinbase_value, j.aux[0].work.coinbase_value);
        assert_eq!(doge.address, "D");
        assert_eq!(&doge.block[..4], &0x0062_0104i32.to_le_bytes());

        // The aux block is: aux header, auxpow, then its single coinbase.
        let aux_header = BlockHeader::deserialize(doge.block[..80].try_into().unwrap());
        assert_eq!(to_display_hex(&aux_header.block_hash()), doge.block_hash);
        let auxpow = decode_for_test(&doge.block[80..]);
        let parent_header = BlockHeader::deserialize(ltc.block[..80].try_into().unwrap());
        assert_eq!(auxpow.parent_header, parent_header);
        assert_eq!(auxpow.parent_hash, parent_header.block_hash());
        assert!(auxpow.coinbase_branch.is_empty());
        assert!(auxpow.chain_branch.is_empty());
        assert_eq!(auxpow.chain_index, 0);
        // The parent coinbase in the auxpow is the non-witness form, hashing to the
        // parent merkle root, and carries the aux block hash after the magic.
        assert_eq!(
            alamo_core::hash::sha256d(&auxpow.coinbase),
            parent_header.merkle_root
        );
        let magic_at = auxpow
            .coinbase
            .windows(4)
            .position(|w| w == MERGED_MINING_MAGIC)
            .unwrap();
        let mut committed = [0u8; 32];
        committed.copy_from_slice(&auxpow.coinbase[magic_at + 4..magic_at + 36]);
        committed.reverse();
        assert_eq!(committed, aux_header.block_hash());
        let rest = &doge.block[80 + auxpow.len..];
        assert_eq!(rest[0], 1);
        assert_eq!(&rest[1..], &j.aux[0].coinbase[..]);

        // A stale aux job still yields the parent block but no aux block.
        j.seen.clear();
        j.aux[0].stale = true;
        let (_, blocks) = validate(
            &mut j,
            &[1, 2, 3, 4],
            &submit(nonce),
            0,
            "addr",
            1_700_000_100,
        );
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].coin, "LTC");
    }

    /// A Bitcoin-shaped job at a SHA-256d share difficulty that every regtest block meets.
    fn sha_job() -> SessionJob {
        let mut work = WorkTemplate::regtest_sample(1, None);
        work.coin = "BTC";
        work.algorithm = alamo_core::Algorithm::Sha256d;
        let work = Arc::new(work);
        let coinbase = CoinbaseParts::build(&work, &[0x51], &[], 8).unwrap();
        let difficulty = 1e-9;
        SessionJob {
            id: JobId(9),
            target: work.algorithm.share_target(difficulty),
            work,
            coinbase,
            aux: Vec::new(),
            difficulty,
            stale: false,
            seen: HashSet::new(),
        }
    }

    #[test]
    fn rolled_version_bits_go_into_the_header_and_the_duplicate_key() {
        const MASK: u32 = 0x1fff_e000;
        let mut j = sha_job();
        let rolled = |nonce, bits| Submit {
            version_bits: Some(bits),
            ..submit(nonce)
        };
        let bits = 0x0aaa_e000;
        let nonce = (0u32..)
            .find(|&n| {
                let (outcome, _) = validate(
                    &mut j,
                    &[1, 2, 3, 4],
                    &rolled(n, bits),
                    MASK,
                    "a",
                    1_700_000_100,
                );
                !matches!(outcome, ShareOutcome::Rejected(RejectReason::LowDifficulty))
            })
            .unwrap();
        j.seen.clear();
        let (outcome, blocks) = validate(
            &mut j,
            &[1, 2, 3, 4],
            &rolled(nonce, bits),
            MASK,
            "a",
            1_700_000_100,
        );
        assert!(matches!(outcome, ShareOutcome::Block { .. }), "{outcome:?}");
        // The block carries the rolled header, and its hash is that header's.
        let header = BlockHeader::deserialize(blocks[0].block[..80].try_into().unwrap());
        assert_eq!(header.version, 0x2aaa_e000);
        assert_eq!(header.nonce, nonce);
        assert_eq!(blocks[0].block_hash, to_display_hex(&header.block_hash()));
        assert!(j
            .target
            .is_met_by(&alamo_core::hash::sha256d(&header.serialize())));

        // The same nonce and bits again is a duplicate; other bits are another share.
        assert_eq!(
            validate(
                &mut j,
                &[1, 2, 3, 4],
                &rolled(nonce, bits),
                MASK,
                "a",
                1_700_000_100
            )
            .0,
            ShareOutcome::Rejected(RejectReason::Duplicate)
        );
        assert_ne!(
            validate(
                &mut j,
                &[1, 2, 3, 4],
                &rolled(nonce, 0x0000_2000),
                MASK,
                "a",
                1_700_000_100
            )
            .0,
            ShareOutcome::Rejected(RejectReason::Duplicate)
        );
        // Without the sixth parameter the job's own version is hashed.
        j.seen.clear();
        let (_, blocks) = validate(
            &mut j,
            &[1, 2, 3, 4],
            &submit(nonce),
            MASK,
            "a",
            1_700_000_100,
        );
        if let Some(block) = blocks.first() {
            assert_eq!(&block.block[..4], &0x2000_0000i32.to_le_bytes());
        }
    }

    #[test]
    fn version_bits_outside_the_negotiated_mask_are_rejected() {
        let mut j = sha_job();
        let bits = |b| Submit {
            version_bits: Some(b),
            ..submit(1)
        };
        for (mask, b) in [
            (0x1fff_e000, 0x0000_1000),
            (0x1fff_e000, 0x2000_0000),
            (0x0000_6000, 0x0000_8000),
            (0, 0x0000_2000),
        ] {
            assert_eq!(
                validate(&mut j, &[0; 4], &bits(b), mask, "a", 1_700_000_100).0,
                ShareOutcome::Rejected(RejectReason::InvalidVersionBits),
                "mask {mask:08x} bits {b:08x}"
            );
        }
        // Zero bits are always allowed, negotiated or not.
        assert_ne!(
            validate(&mut j, &[0; 4], &bits(0), 0, "a", 1_700_000_100).0,
            ShareOutcome::Rejected(RejectReason::InvalidVersionBits)
        );
    }

    #[test]
    fn high_difficulty_rejects_low_share() {
        let mut j = job(1e12);
        let (outcome, _) = validate(&mut j, &[0; 4], &submit(0), 0, "a", 1_700_000_100);
        assert_eq!(outcome, ShareOutcome::Rejected(RejectReason::LowDifficulty));
    }
}
