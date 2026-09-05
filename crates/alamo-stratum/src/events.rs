//! Events the stratum server reports to the rest of the pool.

use alamo_core::hash::Hash256;
use alamo_core::job::RejectReason;

/// Something happened on a stratum connection.
#[derive(Clone, Debug)]
pub enum PoolEvent {
    /// A miner connected.
    Connected {
        /// Session id.
        session: u64,
        /// Peer address.
        peer: String,
    },
    /// A worker authorized on a session.
    Authorized {
        /// Session id.
        session: u64,
        /// Full worker name as sent by the miner.
        worker: String,
        /// Address that will be paid.
        address: String,
        /// Whether the fallback address was substituted.
        fallback: bool,
    },
    /// A session ended.
    Disconnected {
        /// Session id.
        session: u64,
        /// Workers that were authorized on it.
        workers: Vec<String>,
    },
    /// A share was processed.
    Share {
        /// Session id.
        session: u64,
        /// Worker name.
        worker: String,
        /// Coin ticker of the job.
        coin: String,
        /// Difficulty the job required.
        job_difficulty: f64,
        /// Difficulty the hash actually achieved (0 for rejected shares that never hashed).
        share_difficulty: f64,
        /// Rejection reason, if rejected.
        rejected: Option<RejectReason>,
    },
    /// A session's difficulty changed.
    DifficultyChanged {
        /// Session id.
        session: u64,
        /// New difficulty.
        difficulty: f64,
    },
}

/// A share that met a network target: a block ready for submission.
#[derive(Clone, Debug)]
pub struct BlockCandidate {
    /// Coin ticker.
    pub coin: String,
    /// Block height.
    pub height: u64,
    /// Block hash (sha256d of the header) in display hex.
    pub block_hash: String,
    /// Proof-of-work hash, internal byte order.
    pub pow_hash: Hash256,
    /// Worker that found it.
    pub worker: String,
    /// Payout address in the coinbase.
    pub address: String,
    /// Full serialized block.
    pub block: Vec<u8>,
    /// Network difficulty of the block.
    pub network_difficulty: f64,
    /// Difficulty the hash achieved.
    pub share_difficulty: f64,
    /// Unix time the share arrived.
    pub found_at: u64,
}
