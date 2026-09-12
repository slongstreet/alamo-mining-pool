//! The pool status document served to the dashboard.

use alamo_core::odds::OddsSummary;
use alamo_store::BlockRow;
use serde::Serialize;

/// Everything the dashboard shows, refreshed by the daemon every couple of seconds.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PoolSnapshot {
    /// Configured pool name.
    pub pool_name: String,
    /// Daemon version.
    pub version: String,
    /// Seconds since start.
    pub uptime_seconds: u64,
    /// Unix time the snapshot was built, so clients can render ages without clock skew.
    pub now: u64,
    /// Chains being mined, parent first.
    pub coins: Vec<CoinStatus>,
    /// Pool hashrate estimate over the recent window, hashes per second.
    pub hashrate: f64,
    /// Accepted shares recorded (lifetime).
    pub shares_accepted: u64,
    /// Rejected shares recorded (lifetime).
    pub shares_rejected: u64,
    /// Best share difficulty any worker has ever submitted.
    pub best_share_difficulty: f64,
    /// Workers seen, connected first.
    pub workers: Vec<WorkerStatus>,
    /// Recent blocks, newest first.
    pub blocks: Vec<BlockRow>,
    /// Block odds for the parent chain at the current hashrate.
    pub odds: Option<OddsSummary>,
}

/// Status of one chain.
#[derive(Clone, Debug, Serialize)]
pub struct CoinStatus {
    /// Ticker.
    pub symbol: String,
    /// Full coin name.
    pub name: String,
    /// `main`, `test`, or `regtest`.
    pub chain: String,
    /// Height of the block being mined.
    pub height: u64,
    /// Network difficulty relative to pool difficulty 1.
    pub network_difficulty: f64,
    /// Seconds since the current template was fetched.
    pub template_age_seconds: u64,
    /// Coinbase value of the current template, in base units.
    pub coinbase_value: u64,
    /// Confirmations before a found block's reward can be spent.
    pub coinbase_maturity: i64,
    /// Block odds for this chain at the current pool hashrate.
    pub odds: Option<OddsSummary>,
    /// Work submitted since the last block found on this chain.
    pub round: Option<RoundStatus>,
}

/// Progress of the current round on one chain.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct RoundStatus {
    /// Unix time the round started.
    pub started_at: u64,
    /// Accepted work this round, in difficulty-1 shares.
    pub work: f64,
    /// Accepted shares this round.
    pub shares: u64,
    /// Best share difficulty this round.
    pub best_share: f64,
    /// Work expected per block at the current network difficulty, in difficulty-1 shares.
    pub expected_work: f64,
    /// Luck: expected work over actual work, as a percentage. 100 is average.
    pub luck_percent: f64,
}

/// Status of one worker.
#[derive(Clone, Debug, Serialize)]
pub struct WorkerStatus {
    /// Worker name as authorized.
    pub name: String,
    /// Payout address.
    pub address: String,
    /// Whether the fallback address is being paid.
    pub fallback: bool,
    /// Aux chain payouts (from the stratum password).
    pub aux_payouts: Vec<AuxPayoutStatus>,
    /// Number of live sessions.
    pub connections: usize,
    /// Current share difficulty.
    pub difficulty: f64,
    /// Hashrate estimate, hashes per second.
    pub hashrate: f64,
    /// Accepted shares (lifetime).
    pub shares_accepted: u64,
    /// Rejected shares (lifetime).
    pub shares_rejected: u64,
    /// Best share difficulty seen.
    pub best_difficulty: f64,
    /// Seconds since the last accepted share, if any.
    pub last_share_seconds: Option<u64>,
}

/// One aux chain payout of a worker.
#[derive(Clone, Debug, Serialize)]
pub struct AuxPayoutStatus {
    /// Ticker.
    pub coin: String,
    /// Payout address.
    pub address: String,
    /// Whether the fallback address is being paid.
    pub fallback: bool,
}

/// A share as pushed to the dashboard's live log.
#[derive(Clone, Debug, Serialize)]
pub struct ShareEvent {
    /// Unix time received.
    pub ts: u64,
    /// Worker name.
    pub worker: String,
    /// Coin ticker of the job.
    pub coin: String,
    /// Difficulty the job required.
    pub difficulty: f64,
    /// Difficulty the hash achieved.
    pub share_diff: f64,
    /// Whether it was accepted.
    pub accepted: bool,
    /// Rejection slug, if rejected.
    pub reject_reason: Option<String>,
}
