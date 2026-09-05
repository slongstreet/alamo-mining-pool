//! Share validation: rebuild the header a miner hashed and check it against targets.

use crate::events::BlockCandidate;
use crate::job::{SessionJob, EXTRANONCE1_LEN, EXTRANONCE2_LEN};
use alamo_core::auxpow::AuxPow;
use alamo_core::hash::to_display_hex;
use alamo_core::header::BlockHeader;
use alamo_core::job::{RejectReason, ShareOutcome};
use alamo_core::target::hash_difficulty;

/// The fields of a `mining.submit`, already parsed from hex.
#[derive(Clone, Debug)]
pub struct Submit<'a> {
    /// Worker name.
    pub worker: &'a str,
    /// extranonce2 bytes.
    pub extranonce2: Vec<u8>,
    /// Header time.
    pub ntime: u32,
    /// Header nonce.
    pub nonce: u32,
}

/// Maximum seconds a share's ntime may be ahead of the current time.
const MAX_FUTURE_NTIME: u64 = 7_200;

/// Validate a submit against a session job and its extranonce1.
///
/// Returns the outcome and a block candidate for every chain whose target the share met.
pub fn validate(
    job: &mut SessionJob,
    extranonce1: &[u8; EXTRANONCE1_LEN],
    submit: &Submit<'_>,
    payout_address: &str,
    now_unix: u64,
) -> (ShareOutcome, Vec<BlockCandidate>) {
    if job.stale {
        return (ShareOutcome::Rejected(RejectReason::StaleJob), Vec::new());
    }
    if submit.extranonce2.len() + EXTRANONCE1_LEN != job.coinbase.extranonce_len {
        return (
            ShareOutcome::Rejected(RejectReason::InvalidExtranonce2),
            Vec::new(),
        );
    }
    if u64::from(submit.ntime) < u64::from(job.work.min_time)
        || u64::from(submit.ntime) > now_unix + MAX_FUTURE_NTIME
    {
        return (
            ShareOutcome::Rejected(RejectReason::InvalidNtime),
            Vec::new(),
        );
    }
    let en2 = u32::from_be_bytes(submit.extranonce2[..].try_into().unwrap());
    if !job.seen.insert((en2, submit.ntime, submit.nonce)) {
        return (ShareOutcome::Rejected(RejectReason::Duplicate), Vec::new());
    }

    let mut extranonce = [0u8; EXTRANONCE1_LEN + EXTRANONCE2_LEN];
    extranonce[..EXTRANONCE1_LEN].copy_from_slice(extranonce1);
    extranonce[EXTRANONCE1_LEN..].copy_from_slice(&submit.extranonce2);

    let coinbase_txid = job.coinbase.txid(&extranonce);
    let header = BlockHeader {
        version: job.work.version,
        prev_hash: job.work.prev_hash,
        merkle_root: job.work.merkle_root(&coinbase_txid),
        time: submit.ntime,
        bits: job.work.bits,
        nonce: submit.nonce,
    };
    let pow_hash = job.work.algorithm.pow_hash(&header.serialize());
    let difficulty = hash_difficulty(&pow_hash);

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
    use alamo_core::target::Target;
    use alamo_core::work::WorkTemplate;
    use std::collections::HashSet;
    use std::sync::Arc;

    fn job(difficulty: f64) -> SessionJob {
        let work = Arc::new(WorkTemplate::regtest_sample(1, None));
        let coinbase = CoinbaseParts::build(&work, &[0x51], &[], 8).unwrap();
        SessionJob {
            id: JobId(7),
            work,
            coinbase,
            aux: Vec::new(),
            difficulty,
            target: Target::from_difficulty(difficulty),
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
            target: Target::from_difficulty(difficulty),
            stale: false,
            seen: HashSet::new(),
        }
    }

    fn submit(nonce: u32) -> Submit<'static> {
        Submit {
            worker: "w",
            extranonce2: vec![0, 0, 0, 1],
            ntime: 1_700_000_000,
            nonce,
        }
    }

    /// Find a nonce whose scrypt hash satisfies the (tiny) regtest share target.
    fn mine(job: &mut SessionJob) -> u32 {
        let nonce = (0u32..)
            .find(|&nonce| {
                let (outcome, _) =
                    validate(job, &[1, 2, 3, 4], &submit(nonce), "addr", 1_700_000_100);
                !matches!(outcome, ShareOutcome::Rejected(RejectReason::LowDifficulty))
            })
            .unwrap();
        job.seen.clear();
        nonce
    }

    #[test]
    fn regtest_share_is_a_block_and_duplicates_are_rejected() {
        // Share difficulty far below the regtest network target: any accepted share is a block.
        let mut j = job(1e-7);
        let nonce = mine(&mut j);
        let (outcome, block) =
            validate(&mut j, &[1, 2, 3, 4], &submit(nonce), "addr", 1_700_000_100);
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

        let (dup, _) = validate(&mut j, &[1, 2, 3, 4], &submit(nonce), "addr", 1_700_000_100);
        assert_eq!(dup, ShareOutcome::Rejected(RejectReason::Duplicate));
    }

    #[test]
    fn rejects_stale_bad_ntime_and_bad_extranonce() {
        let mut j = job(1e-7);
        j.stale = true;
        assert_eq!(
            validate(&mut j, &[0; 4], &submit(1), "a", 1_700_000_100).0,
            ShareOutcome::Rejected(RejectReason::StaleJob)
        );
        let mut j = job(1e-7);
        let mut s = submit(1);
        s.ntime = 1;
        assert_eq!(
            validate(&mut j, &[0; 4], &s, "a", 1_700_000_100).0,
            ShareOutcome::Rejected(RejectReason::InvalidNtime)
        );
        let mut s = submit(1);
        s.extranonce2 = vec![0; 3];
        assert_eq!(
            validate(&mut j, &[0; 4], &s, "a", 1_700_000_100).0,
            ShareOutcome::Rejected(RejectReason::InvalidExtranonce2)
        );
    }

    #[test]
    fn merged_share_yields_parent_and_aux_blocks() {
        use alamo_core::auxpow::{decode_for_test, MERGED_MINING_MAGIC};
        let mut j = merged_job(1e-7);
        let nonce = mine(&mut j);
        let (outcome, blocks) =
            validate(&mut j, &[1, 2, 3, 4], &submit(nonce), "addr", 1_700_000_100);
        assert!(matches!(outcome, ShareOutcome::Block { .. }));
        assert_eq!(blocks.len(), 2);
        let (ltc, doge) = (&blocks[0], &blocks[1]);
        assert_eq!(ltc.coin, "LTC");
        assert_eq!(doge.coin, "DOGE");
        assert_eq!(doge.height, 31);
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
        let (_, blocks) = validate(&mut j, &[1, 2, 3, 4], &submit(nonce), "addr", 1_700_000_100);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].coin, "LTC");
    }

    #[test]
    fn high_difficulty_rejects_low_share() {
        let mut j = job(1e12);
        let (outcome, _) = validate(&mut j, &[0; 4], &submit(0), "a", 1_700_000_100);
        assert_eq!(outcome, ShareOutcome::Rejected(RejectReason::LowDifficulty));
    }
}
