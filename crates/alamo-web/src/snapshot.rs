//! The pool status document served to the dashboard.

use alamo_core::odds::OddsSummary;
use alamo_store::BlockRow;
use serde::Serialize;

/// Everything the dashboard shows, refreshed by the daemon every couple of seconds.
///
/// Per-lane figures live in `lanes`. The top-level `coins`, `hashrate`, share counts,
/// work, share multiplier, best share, `scoring_since`, and `workers` are deprecated: for
/// one release they mirror the primary lane (`lanes[0]`) while it is running and hold
/// their defaults otherwise.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PoolSnapshot {
    /// Configured pool name.
    pub pool_name: String,
    /// Daemon version.
    pub version: String,
    /// Seconds since start.
    pub uptime_seconds: u64,
    /// TCP port miners of the primary lane connect to. The host is wherever the dashboard
    /// was reached. Each lane's port is in `lanes`.
    pub stratum_port: u16,
    /// Unix time the snapshot was built.
    pub now: u64,
    /// Every configured lane, primary first, then by key.
    pub lanes: Vec<LaneStatus>,
    /// Deprecated: `lanes[0].coins`.
    pub coins: Vec<CoinStatus>,
    /// Deprecated: `lanes[0].hashrate`.
    pub hashrate: f64,
    /// Deprecated: `lanes[0].shares_accepted`.
    pub shares_accepted: u64,
    /// Deprecated: `lanes[0].shares_rejected`.
    pub shares_rejected: u64,
    /// Deprecated: `lanes[0].total_work`.
    pub total_work: f64,
    /// Deprecated: `lanes[0].share_multiplier`.
    pub share_multiplier: f64,
    /// Deprecated: `lanes[0].best_share_difficulty`.
    pub best_share_difficulty: f64,
    /// Deprecated: `lanes[0].best_share_worker`.
    pub best_share_worker: Option<String>,
    /// Deprecated: `lanes[0].best_share_at`.
    pub best_share_at: Option<u64>,
    /// Deprecated: `lanes[0].scoring_since`.
    pub scoring_since: Option<u64>,
    /// Deprecated: `lanes[0].workers`.
    pub workers: Vec<WorkerStatus>,
    /// Recent blocks of every lane, newest first. Each row's `share_diff` is in its own
    /// lane's network units.
    pub blocks: Vec<BlockRow>,
}

impl PoolSnapshot {
    /// A snapshot of `lanes` (primary first) and `blocks`, with the deprecated top-level
    /// fields mirroring the primary lane while it is running.
    pub fn from_lanes(now: u64, lanes: Vec<LaneStatus>, blocks: Vec<BlockRow>) -> Self {
        let mut s = Self {
            now,
            blocks,
            ..Default::default()
        };
        if let Some(p) = lanes.first().filter(|l| l.state == LaneState::Running) {
            s.coins = p.coins.clone();
            s.hashrate = p.hashrate;
            s.shares_accepted = p.shares_accepted;
            s.shares_rejected = p.shares_rejected;
            s.total_work = p.total_work;
            s.share_multiplier = p.share_multiplier;
            s.best_share_difficulty = p.best_share_difficulty;
            s.best_share_worker = p.best_share_worker.clone();
            s.best_share_at = p.best_share_at;
            s.scoring_since = p.scoring_since;
            s.workers = p.workers.clone();
        }
        s.lanes = lanes;
        s
    }

    /// The lane with config key `key`.
    pub fn lane(&self, key: &str) -> Option<&LaneStatus> {
        self.lanes.iter().find(|l| l.key == key)
    }
}

/// Where a lane is in its life.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LaneState {
    /// Waiting for its nodes; its listener is not open yet.
    #[default]
    Connecting,
    /// Nodes connected and listener open.
    Running,
    /// Gave up starting because of a configuration problem; see `error`.
    Failed,
}

impl LaneState {
    /// The JSON spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            LaneState::Connecting => "connecting",
            LaneState::Running => "running",
            LaneState::Failed => "failed",
        }
    }
}

/// One lane: a parent coin, its aux coins, its listener, and its own statistics.
#[derive(Clone, Debug, Default, Serialize)]
pub struct LaneStatus {
    /// The parent coin's config key, which is the lane id.
    pub key: String,
    /// Proof-of-work algorithm: `scrypt` or `sha256d`.
    pub algorithm: String,
    /// TCP port this lane's miners connect to.
    pub stratum_port: u16,
    /// `connecting`, `running`, or `failed`.
    pub state: LaneState,
    /// Why the lane failed to start; `null` unless `state` is `failed`.
    pub error: Option<String>,
    /// `main`, `test`, or `regtest`; `null` until the parent's node has answered.
    pub chain: Option<String>,
    /// Stratum share difficulty per unit of network difficulty on this lane's parent
    /// chain (65536 for scrypt, 1 for sha256d). Job difficulties (`WorkerStatus::
    /// difficulty`, the share log's `difficulty`) are in stratum units, as the miner sees
    /// them; every other difficulty the API reports, including `share_diff`, is in
    /// network units.
    pub share_multiplier: f64,
    /// Lane hashrate estimate over the recent window, hashes per second.
    pub hashrate: f64,
    /// Accepted shares recorded (lifetime).
    pub shares_accepted: u64,
    /// Rejected shares recorded (lifetime).
    pub shares_rejected: u64,
    /// Lifetime accepted work in difficulty units.
    pub total_work: f64,
    /// Best share difficulty any worker in the lane has found.
    pub best_share_difficulty: f64,
    /// Worker that found the best share.
    pub best_share_worker: Option<String>,
    /// Unix time the best share arrived, when known.
    pub best_share_at: Option<u64>,
    /// Unix time the lane first saw a worker: when it started keeping score.
    pub scoring_since: Option<u64>,
    /// Every coin configured in this lane, parent first, known from startup: what the
    /// lane mines once its nodes answer, whatever its state.
    pub configured_coins: Vec<LaneCoin>,
    /// Chains mined in this lane, parent first, once they have had a template.
    pub coins: Vec<CoinStatus>,
    /// Workers seen in this lane, connected first.
    pub workers: Vec<WorkerStatus>,
}

/// A coin configured in a lane, from the config file rather than its node.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LaneCoin {
    /// Config key (`ltc`).
    pub key: String,
    /// Ticker (`LTC`), as `CoinStatus::symbol` spells it.
    pub symbol: String,
    /// Human-readable name (`Litecoin`).
    pub name: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(key: &str, state: LaneState, shares: u64) -> LaneStatus {
        LaneStatus {
            key: key.into(),
            algorithm: "scrypt".into(),
            state,
            share_multiplier: 65536.0,
            shares_accepted: shares,
            hashrate: shares as f64,
            best_share_worker: Some(format!("{key}-rig")),
            configured_coins: vec![LaneCoin {
                key: key.into(),
                symbol: key.to_uppercase(),
                name: format!("{key} coin"),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn legacy_fields_mirror_a_running_primary_lane_only() {
        let s = PoolSnapshot::from_lanes(
            7,
            vec![
                lane("ltc", LaneState::Running, 3),
                lane("doge", LaneState::Failed, 9),
            ],
            vec![],
        );
        assert_eq!(s.now, 7);
        assert_eq!(s.shares_accepted, 3);
        assert_eq!(s.hashrate, 3.0);
        assert_eq!(s.share_multiplier, 65536.0);
        assert_eq!(s.best_share_worker.as_deref(), Some("ltc-rig"));
        assert_eq!(s.lane("doge").unwrap().shares_accepted, 9);

        let s = PoolSnapshot::from_lanes(
            7,
            vec![
                lane("ltc", LaneState::Connecting, 3),
                lane("doge", LaneState::Running, 9),
            ],
            vec![],
        );
        assert_eq!(s.shares_accepted, 0, "defaults until the primary lane runs");
        assert_eq!(s.share_multiplier, 0.0);
        assert!(s.best_share_worker.is_none());
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["lanes"][0]["state"], "connecting");
        assert_eq!(json["lanes"][0]["error"], serde_json::Value::Null);
        assert_eq!(json["lanes"][1]["state"], "running");
        assert_eq!(
            json["lanes"][0]["configured_coins"],
            serde_json::json!([{ "key": "ltc", "symbol": "LTC", "name": "ltc coin" }]),
            "a lane's configured coins are known before it connects"
        );
    }
}
