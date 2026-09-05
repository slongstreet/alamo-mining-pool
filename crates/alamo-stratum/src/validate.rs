//! Share validation: rebuild the header a miner hashed and check it against targets.

use crate::events::BlockCandidate;
use crate::job::{SessionJob, EXTRANONCE1_LEN, EXTRANONCE2_LEN};
use alamo_core::hash::to_display_hex;
use alamo_core::header::BlockHeader;
use alamo_core::job::{RejectReason, ShareOutcome};
use alamo_core::target::hash_difficulty;

/// The fields of a `mining.submit`, already parsed from hex.
#[derive(Clone, Debug)]
pub struct Submit {
    /// Worker name.
    pub worker: String,
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
/// Returns the outcome and, when a network target was met, the block to submit.
pub fn validate(
    job: &mut SessionJob,
    extranonce1: &[u8; EXTRANONCE1_LEN],
    submit: &Submit,
    payout_address: &str,
    now_unix: u64,
) -> (ShareOutcome, Option<BlockCandidate>) {
    if job.stale {
        return (ShareOutcome::Rejected(RejectReason::StaleJob), None);
    }
    if submit.extranonce2.len() != EXTRANONCE2_LEN {
        return (
            ShareOutcome::Rejected(RejectReason::InvalidExtranonce2),
            None,
        );
    }
    if u64::from(submit.ntime) < u64::from(job.work.min_time)
        || u64::from(submit.ntime) > now_unix + MAX_FUTURE_NTIME
    {
        return (ShareOutcome::Rejected(RejectReason::InvalidNtime), None);
    }
    let en2 = u32::from_be_bytes(submit.extranonce2[..].try_into().unwrap());
    if !job.seen.insert((en2, submit.ntime, submit.nonce)) {
        return (ShareOutcome::Rejected(RejectReason::Duplicate), None);
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
        return (ShareOutcome::Rejected(RejectReason::LowDifficulty), None);
    }

    if job.work.target.is_met_by(&pow_hash) {
        let coinbase = job.coinbase.serialize_for_block(&extranonce);
        let block = job.work.assemble_block(&header, &coinbase);
        let candidate = BlockCandidate {
            coin: job.work.coin.clone(),
            height: job.work.height,
            block_hash: to_display_hex(&header.block_hash()),
            pow_hash,
            worker: submit.worker.clone(),
            address: payout_address.to_string(),
            block,
            network_difficulty: job.work.network_difficulty(),
            share_difficulty: difficulty,
            found_at: now_unix,
        };
        let outcome = ShareOutcome::Block {
            difficulty,
            pow_hash,
            chains: vec![],
        };
        return (outcome, Some(candidate));
    }

    (ShareOutcome::Accepted { difficulty }, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::SessionJob;
    use alamo_core::coinbase::CoinbaseParts;
    use alamo_core::job::JobId;
    use alamo_core::target::Target;
    use alamo_core::work::WorkTemplate;
    use alamo_core::Algorithm;
    use std::collections::HashSet;
    use std::sync::Arc;

    fn regtest_work() -> Arc<WorkTemplate> {
        let mut prefix = Vec::new();
        alamo_core::encode::push_script_num(&mut prefix, 1);
        let mut w = WorkTemplate {
            id: JobId(1),
            coin: "LTC".into(),
            algorithm: Algorithm::Scrypt,
            height: 1,
            version: 0x2000_0000,
            prev_hash: [0x11; 32],
            bits: 0x207f_ffff,
            target: Target::from_compact(0x207f_ffff),
            cur_time: 1_700_000_000,
            min_time: 1_699_990_000,
            coinbase_value: 5_000_000_000,
            coinbase_script_prefix: prefix,
            witness_commitment: None,
            transactions: vec![],
            merkle_branch: vec![],
            extra_payload: vec![],
            clean_jobs: true,
            created_at: 0,
        };
        w.compute_merkle_branch();
        Arc::new(w)
    }

    fn job(difficulty: f64) -> SessionJob {
        let work = regtest_work();
        let coinbase = CoinbaseParts::build(&work, &[0x51], 8).unwrap();
        SessionJob {
            id: JobId(7),
            work,
            coinbase,
            difficulty,
            target: Target::from_difficulty(difficulty),
            stale: false,
            seen: HashSet::new(),
        }
    }

    fn submit(nonce: u32) -> Submit {
        Submit {
            worker: "w".into(),
            extranonce2: vec![0, 0, 0, 1],
            ntime: 1_700_000_000,
            nonce,
        }
    }

    /// Find a nonce whose scrypt hash satisfies the (tiny) regtest share target.
    fn mine(job: &SessionJob) -> u32 {
        (0u32..)
            .find(|&nonce| {
                let mut probe = job.seen.clone();
                probe.clear();
                let mut j = SessionJob {
                    seen: probe,
                    ..clone_job(job)
                };
                matches!(
                    validate(&mut j, &[1, 2, 3, 4], &submit(nonce), "addr", 1_700_000_100).0,
                    ShareOutcome::Accepted { .. } | ShareOutcome::Block { .. }
                )
            })
            .unwrap()
    }

    fn clone_job(j: &SessionJob) -> SessionJob {
        SessionJob {
            id: j.id,
            work: j.work.clone(),
            coinbase: j.coinbase.clone(),
            difficulty: j.difficulty,
            target: j.target,
            stale: j.stale,
            seen: HashSet::new(),
        }
    }

    #[test]
    fn regtest_share_is_a_block_and_duplicates_are_rejected() {
        // Share difficulty far below the regtest network target: any accepted share is a block.
        let mut j = job(1e-7);
        let nonce = mine(&j);
        let (outcome, block) =
            validate(&mut j, &[1, 2, 3, 4], &submit(nonce), "addr", 1_700_000_100);
        assert!(matches!(outcome, ShareOutcome::Block { .. }), "{outcome:?}");
        let block = block.unwrap();
        assert_eq!(block.height, 1);
        assert_eq!(&block.block[..4], &0x2000_0000i32.to_le_bytes());
        assert_eq!(block.block[80], 1); // one transaction
                                        // Header prev hash and the coinbase's extranonce are embedded.
        assert_eq!(&block.block[4..36], &[0x11; 32]);
        let en_pos = block
            .block
            .windows(8)
            .position(|w| w == [1, 2, 3, 4, 0, 0, 0, 1]);
        assert!(en_pos.is_some());

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
    fn high_difficulty_rejects_low_share() {
        let mut j = job(1e12);
        let (outcome, _) = validate(&mut j, &[0; 4], &submit(0), "a", 1_700_000_100);
        assert_eq!(outcome, ShareOutcome::Rejected(RejectReason::LowDifficulty));
    }
}
