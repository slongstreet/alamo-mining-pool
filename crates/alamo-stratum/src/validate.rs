//! Share validation: rebuild the header a miner hashed and check it against targets.

use crate::events::BlockCandidate;
use crate::job::{SessionJob, EXTRANONCE1_LEN, EXTRANONCE2_LEN};
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
/// Returns the outcome and, when the network target was met, the block to submit.
pub fn validate(
    job: &mut SessionJob,
    extranonce1: &[u8; EXTRANONCE1_LEN],
    submit: &Submit<'_>,
    payout_address: &str,
    now_unix: u64,
) -> (ShareOutcome, Option<BlockCandidate>) {
    if job.stale {
        return (ShareOutcome::Rejected(RejectReason::StaleJob), None);
    }
    if submit.extranonce2.len() + EXTRANONCE1_LEN != job.coinbase.extranonce_len {
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

    if job.work.target().is_met_by(&pow_hash) {
        let coinbase = job.coinbase.serialize_for_block(&extranonce);
        let block = job.work.assemble_block(&header, &coinbase);
        let candidate = BlockCandidate {
            coin: job.work.coin,
            height: job.work.height,
            block_hash: to_display_hex(&header.block_hash()),
            pow_hash,
            worker: submit.worker.to_string(),
            address: payout_address.to_string(),
            block,
            network_difficulty: job.work.network_difficulty(),
            share_difficulty: difficulty,
            found_at: now_unix,
        };
        return (ShareOutcome::Block { difficulty }, Some(candidate));
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
    use std::collections::HashSet;
    use std::sync::Arc;

    fn job(difficulty: f64) -> SessionJob {
        let work = Arc::new(WorkTemplate::regtest_sample(1, None));
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
        let block = block.unwrap();
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
    fn high_difficulty_rejects_low_share() {
        let mut j = job(1e12);
        let (outcome, _) = validate(&mut j, &[0; 4], &submit(0), "a", 1_700_000_100);
        assert_eq!(outcome, ShareOutcome::Rejected(RejectReason::LowDifficulty));
    }
}
