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
    /// TCP port miners connect to. The host is wherever the dashboard was reached.
    pub stratum_port: u16,
    /// Unix time the snapshot was built.
    pub now: u64,
    /// Chains being mined, parent first.
    pub coins: Vec<CoinStatus>,
    /// Pool hashrate estimate over the recent window, hashes per second.
    pub hashrate: f64,
    /// Accepted shares recorded (lifetime).
    pub shares_accepted: u64,
    /// Rejected shares recorded (lifetime).
    pub shares_rejected: u64,
    /// Lifetime accepted work in difficulty units.
    pub total_work: f64,
    /// Best share difficulty any worker has found.
    pub best_share_difficulty: f64,
    /// Workers seen, connected first.
    pub workers: Vec<WorkerStatus>,
    /// Recent blocks, newest first.
    pub blocks: Vec<BlockRow>,
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
    /// Block odds on this chain at the pool's current hashrate.
    pub odds: OddsSummary,
    /// The round in progress and lifetime luck on this chain.
    pub round: RoundStatus,
    /// Whether the node behind this chain is answering.
    pub node: NodeStatus,
}

/// Reachability of one chain's node.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct NodeStatus {
    /// Whether the most recent RPC call succeeded.
    pub connected: bool,
    /// Whether the template was withdrawn because the node stayed unreachable too long.
    /// The figures shown for this chain come from the last template it did serve.
    pub stale: bool,
    /// Consecutive failed polls.
    pub failures: u32,
    /// The most recent RPC failure while unreachable.
    pub last_error: Option<String>,
    /// Seconds since the node last answered, if it ever has.
    pub last_ok_seconds: Option<u64>,
    /// ZMQ block notifications: `null` when not configured, else whether subscribed.
    pub zmq: Option<bool>,
}

/// Work since the last block on a chain, compared with what a block is expected to take.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RoundStatus {
    /// Blocks the node has accepted on this chain.
    pub blocks_found: u64,
    /// Unix time the last block was found, which started this round.
    pub started_at: Option<i64>,
    /// Accepted work this round, in difficulty units.
    pub work: f64,
    /// Work one block is expected to take: the network difficulty.
    pub expected_work: f64,
    /// `work / expected_work`. Above 1 means the round is running long.
    pub progress: f64,
    /// Lifetime luck in percent: expected work over actual work for the blocks found.
    /// 100 is average, above 100 is lucky. `None` until the first block.
    pub luck_percent: Option<f64>,
    /// Blocks the pool's lifetime work would find on average, at the current network
    /// difficulty. Meaningful before the first block, when luck is not.
    pub expected_blocks: f64,
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
    /// Lifetime accepted work in difficulty units.
    pub work_accepted: f64,
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
