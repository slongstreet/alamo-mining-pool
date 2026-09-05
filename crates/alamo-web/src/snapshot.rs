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
    /// Chains being mined.
    pub coins: Vec<CoinStatus>,
    /// Pool hashrate estimate over the recent window, hashes per second.
    pub hashrate: f64,
    /// Accepted shares since start.
    pub shares_accepted: u64,
    /// Rejected shares since start.
    pub shares_rejected: u64,
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
    /// Accepted shares.
    pub shares_accepted: u64,
    /// Rejected shares.
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
