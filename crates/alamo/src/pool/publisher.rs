//! The publisher: every lane's statistics and accounting, and the status snapshot the
//! dashboard is sent.

use super::lane::LaneNotice;
use crate::config::LaneConfig;
use crate::persist::{Persistence, Retention};
use crate::stats::Stats;
use alamo_coins::{Chain, NodeHealth};
use alamo_core::odds::OddsSummary;
use alamo_core::time::now_unix;
use alamo_core::work::WorkTemplate;
use alamo_core::Algorithm;
use alamo_store::{BlockRow, CoinRounds, Store};
use alamo_stratum::{PoolEvent, WorkReceiver};
use alamo_web::{
    AppState, CoinStatus, Command, LaneCoin, LaneState, LaneStatus, NodeStatus, PoolSnapshot,
    RoundStatus,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

/// How often the snapshot is rebuilt.
const PUBLISH_INTERVAL: Duration = Duration::from_secs(2);
/// How often the block list is re-read from the database (it only changes on submit or
/// on the tracker's 30 s tick).
const BLOCKS_REFRESH_TICKS: u32 = 5;

/// What the publisher knows of a lane once its nodes have connected.
struct LaneView {
    chain: Chain,
    work: WorkReceiver,
    nodes: Vec<(&'static str, watch::Receiver<NodeHealth>)>,
    /// Last template seen per coin, so a coin whose template was withdrawn keeps its
    /// place on the dashboard, flagged stale, instead of vanishing.
    templates: HashMap<&'static str, Arc<WorkTemplate>>,
}

/// One lane's statistics and accounting, owned by the publisher.
struct LaneAccount {
    key: String,
    algorithm: Algorithm,
    stratum_port: u16,
    /// The lane's coins as configured, parent first, for the dashboard before they connect.
    configured_coins: Vec<LaneCoin>,
    stats: Stats,
    persist: Persistence,
    /// `None` until the lane's nodes have connected.
    view: Option<LaneView>,
    /// Why the lane gave up starting, once it has.
    failed: Option<String>,
}

impl LaneAccount {
    fn state(&self) -> LaneState {
        if self.failed.is_some() {
            LaneState::Failed
        } else if self.view.is_some() {
            LaneState::Running
        } else {
            LaneState::Connecting
        }
    }
}

/// Every lane's statistics, and the snapshot built from them.
pub(super) struct Publisher {
    store: Store,
    lanes: Vec<LaneAccount>,
    primary: usize,
    retention: Retention,
    blocks: Vec<BlockRow>,
    rounds: Vec<CoinRounds>,
    ticks: u32,
}

/// A lane's configured coins, parent first. Validation refuses unknown keys, so every
/// key has a built-in coin; the key stands in for the names otherwise.
fn lane_coins(lane: &LaneConfig) -> Vec<LaneCoin> {
    std::iter::once(&lane.key)
        .chain(&lane.aux)
        .map(|key| {
            let coin = alamo_coins::builtin(key);
            LaneCoin {
                key: key.clone(),
                symbol: coin
                    .as_ref()
                    .map_or_else(|| key.to_uppercase(), |c| c.symbol().to_string()),
                name: coin.map_or_else(|| key.clone(), |c| c.name().to_string()),
            }
        })
        .collect()
}

impl Publisher {
    /// Restore every configured lane's statistics. A lane's share multiplier follows
    /// from its parent coin's algorithm, so this needs no node.
    pub(super) async fn load(store: Store, lanes: &[LaneConfig], primary: usize, now: u64) -> Self {
        let mut states = Vec::with_capacity(lanes.len());
        for lane in lanes {
            let algorithm = lane.algorithm;
            let share_multiplier = algorithm.share_multiplier();
            let stats = match Stats::load(&store, &lane.key, now, share_multiplier).await {
                Ok(s) => {
                    tracing::info!(
                        lane = %lane.key,
                        workers = s.workers(now).len(),
                        shares_accepted = s.shares_accepted(),
                        "restored stats"
                    );
                    s
                }
                Err(err) => {
                    tracing::warn!(lane = %lane.key, %err, "could not restore stats");
                    Stats::new(share_multiplier)
                }
            };
            states.push(LaneAccount {
                key: lane.key.clone(),
                algorithm,
                stratum_port: lane.stratum.listen.port(),
                configured_coins: lane_coins(lane),
                stats,
                persist: Persistence::new(store.clone(), &lane.key),
                view: None,
                failed: None,
            });
        }
        Self {
            retention: Retention::new(store.clone(), now),
            store,
            lanes: states,
            primary,
            blocks: Vec::new(),
            rounds: Vec::new(),
            ticks: 0,
        }
    }

    fn notice(&mut self, notice: LaneNotice) {
        match notice {
            LaneNotice::Up(up) => {
                if let Some(lane) = self.lanes.get_mut(up.lane) {
                    lane.view = Some(LaneView {
                        chain: up.chain,
                        work: up.work,
                        nodes: up.nodes,
                        templates: HashMap::new(),
                    });
                }
            }
            LaneNotice::Failed { lane, error } => {
                if let Some(lane) = self.lanes.get_mut(lane) {
                    lane.failed = Some(error);
                }
            }
        }
    }

    /// Account one event from `lane`. Session ids are only unique within a lane's
    /// stratum server, which is why every lane keeps its own `Stats`.
    async fn observe(&mut self, lane: usize, event: &PoolEvent, ts: u64) {
        let Some(l) = self.lanes.get_mut(lane) else {
            return;
        };
        l.persist.observe(event, ts);
        l.stats.apply(event, ts);
        if l.persist.should_flush() {
            flush_persist(&l.key, &mut l.persist).await;
        }
    }

    async fn flush_all(&mut self) {
        for l in &mut self.lanes {
            flush_persist(&l.key, &mut l.persist).await;
        }
    }

    /// Carry out an operator command on the lane it names.
    async fn command(&mut self, command: Command) {
        let (lane, name) = match &command {
            Command::ResetStats { lane } => (lane, None),
            Command::RemoveWorker { lane, name } => (lane, Some(name)),
        };
        let Some(l) = self.lanes.iter_mut().find(|l| l.key == *lane) else {
            tracing::warn!(lane = %lane, "command for an unknown lane ignored");
            return;
        };
        match name {
            None => {
                // Queued shares must land before the zeroing or they would be
                // counted on top of it.
                flush_persist(&l.key, &mut l.persist).await;
                match self.store.reset_share_counters(&l.key).await {
                    Ok(()) => {
                        l.stats.reset_counters();
                        tracing::info!(lane = %l.key, "share counters and best share reset by operator");
                    }
                    Err(err) => {
                        tracing::warn!(lane = %l.key, %err, "could not reset share counters")
                    }
                }
            }
            Some(name) => {
                // The API already refused connected workers, but a session may have
                // arrived since; and queued shares must land before the row goes.
                if l.stats.is_connected(name) {
                    tracing::warn!(lane = %l.key, worker = %name, "worker reconnected; not removed");
                    return;
                }
                flush_persist(&l.key, &mut l.persist).await;
                match self.store.remove_worker(&l.key, name).await {
                    Ok(_) => {
                        l.stats.remove_worker(name);
                        tracing::info!(lane = %l.key, worker = %name, "worker removed by operator");
                    }
                    Err(err) => {
                        tracing::warn!(lane = %l.key, %err, worker = %name, "could not remove worker")
                    }
                }
            }
        }
    }

    /// Persist, sample, retain, and refresh the block list.
    async fn tick(&mut self, now: u64) {
        for l in &mut self.lanes {
            // A lane that has not connected has no sessions: nothing to sample.
            let result = if l.view.is_some() {
                l.persist.on_tick(&l.stats, now).await
            } else {
                l.persist.flush().await
            };
            if let Err(err) = result {
                tracing::warn!(lane = %l.key, %err, "could not persist accounting");
            }
        }
        if let Err(err) = self.retention.maybe_retain(now).await {
            tracing::warn!(%err, "could not apply retention");
        }
        if self.ticks % BLOCKS_REFRESH_TICKS == 0 {
            match self.store.recent_blocks(25).await {
                Ok(rows) => self.blocks = rows,
                Err(err) => tracing::warn!(%err, "could not read blocks"),
            }
            match self.store.coin_rounds().await {
                Ok(rows) => self.rounds = rows,
                Err(err) => tracing::warn!(%err, "could not read rounds"),
            }
        }
        self.ticks = self.ticks.wrapping_add(1);
    }

    /// Lane indexes, primary first, then in config order.
    fn order(&self) -> Vec<usize> {
        std::iter::once(self.primary)
            .chain((0..self.lanes.len()).filter(|&i| i != self.primary))
            .collect()
    }

    /// The status document: every lane (primary first) and every lane's blocks.
    fn snapshot(&mut self) -> PoolSnapshot {
        let now = now_unix();
        let mut lanes = Vec::with_capacity(self.lanes.len());
        for i in self.order() {
            let l = &mut self.lanes[i];
            if let Some(view) = l.view.as_mut() {
                remember_templates(&view.work, &mut view.templates);
            }
            lanes.push(lane_status(l, &self.rounds, now));
        }
        let primary = self.lanes[self.primary].stats.share_multiplier();
        let multipliers: HashMap<&str, f64> = self
            .lanes
            .iter()
            .map(|l| (l.key.as_str(), l.stats.share_multiplier()))
            .collect();
        let blocks = convert_blocks(self.blocks.clone(), |lane| {
            multipliers.get(lane).copied().unwrap_or_else(|| {
                alamo_coins::builtin(lane).map_or(primary, |c| c.algorithm().share_multiplier())
            })
        });
        PoolSnapshot::from_lanes(now, lanes, blocks)
    }
}

/// Wait for the next event from any lane, starting each wait one lane further along so
/// a busy lane cannot starve the others. `None` once every lane's channel has closed.
async fn next_event(
    lanes: &mut [Option<mpsc::Receiver<PoolEvent>>],
    start: &mut usize,
) -> Option<(usize, PoolEvent)> {
    std::future::poll_fn(|cx| {
        let n = lanes.len();
        let mut open = false;
        for k in 0..n {
            let i = (*start + k) % n;
            let Some(rx) = lanes[i].as_mut() else {
                continue;
            };
            match rx.poll_recv(cx) {
                Poll::Ready(Some(event)) => {
                    *start = (i + 1) % n;
                    return Poll::Ready(Some((i, event)));
                }
                Poll::Ready(None) => lanes[i] = None,
                Poll::Pending => open = true,
            }
        }
        if open {
            Poll::Pending
        } else {
            Poll::Ready(None)
        }
    })
    .await
}

pub(super) async fn publisher(
    mut p: Publisher,
    mut events: Vec<Option<mpsc::Receiver<PoolEvent>>>,
    mut notices: mpsc::Receiver<LaneNotice>,
    state: AppState,
    mut commands: mpsc::Receiver<Command>,
    shutdown: CancellationToken,
) {
    let mut start = 0usize;
    let mut interval = tokio::time::interval(PUBLISH_INTERVAL);
    loop {
        tokio::select! {
            Some(notice) = notices.recv() => {
                p.notice(notice);
                state.publish(p.snapshot());
            }
            next = next_event(&mut events, &mut start) => {
                let Some((lane, event)) = next else {
                    p.flush_all().await;
                    return;
                };
                p.observe(lane, &event, now_unix()).await;
            }
            Some(command) = commands.recv() => {
                p.command(command).await;
                state.publish(p.snapshot());
            }
            _ = interval.tick() => {
                p.tick(now_unix()).await;
                state.publish(p.snapshot());
            }
            _ = shutdown.cancelled() => {
                p.flush_all().await;
                return;
            }
        }
    }
}

async fn flush_persist(lane: &str, persist: &mut Persistence) {
    if let Err(err) = persist.flush().await {
        tracing::warn!(lane, %err, "could not persist accounting");
    }
}

/// Lane-wide figures every chain's status in the lane is derived from.
#[derive(Clone, Copy)]
struct PoolFigures {
    hashrate: f64,
    total_work: f64,
    share_multiplier: f64,
}

fn coin_status(
    w: &WorkTemplate,
    chain: Chain,
    pool: PoolFigures,
    rounds: Option<&CoinRounds>,
    health: &NodeHealth,
    now: u64,
) -> CoinStatus {
    let network_difficulty = w.network_difficulty();
    CoinStatus {
        symbol: w.coin.to_string(),
        chain: chain.to_string(),
        height: w.height,
        network_difficulty,
        template_age_seconds: now.saturating_sub(w.created_at),
        coinbase_value: w.coinbase_value,
        odds: OddsSummary::compute(pool.hashrate, network_difficulty),
        round: round_status(
            network_difficulty,
            pool.total_work,
            rounds,
            pool.share_multiplier,
        ),
        node: node_status(health, now),
    }
}

fn node_status(health: &NodeHealth, now: u64) -> NodeStatus {
    NodeStatus {
        connected: health.connected,
        stale: health.withdrawn,
        failures: health.failures,
        last_error: health.last_error.clone(),
        last_ok_seconds: health.last_ok.map(|t| now.saturating_sub(t)),
        zmq: health.zmq,
    }
}

/// Note the templates in the current merged work.
fn remember_templates(
    work: &WorkReceiver,
    templates: &mut HashMap<&'static str, Arc<WorkTemplate>>,
) {
    let current = work.borrow().clone();
    for w in current
        .iter()
        .flat_map(|m| std::iter::once(&m.parent).chain(m.aux.iter()))
    {
        templates.insert(w.coin, w.clone());
    }
}

/// The current round on one chain. Before the first block the round spans all work.
/// The store banks work at each block in stratum share units; `share_multiplier`
/// converts it to the network units everything else here uses.
fn round_status(
    network_difficulty: f64,
    total_work: f64,
    rounds: Option<&CoinRounds>,
    share_multiplier: f64,
) -> RoundStatus {
    let banked = rounds
        .and_then(|r| r.last_work_at_found)
        .map_or(0.0, |w| w / share_multiplier);
    let work = (total_work - banked).max(0.0);
    let expected_work = network_difficulty;
    let luck_percent = rounds
        .filter(|r| r.blocks_found > 0 && total_work > 0.0)
        .map(|r| alamo_core::odds::luck_percent(r.expected_work, total_work));
    let expected_blocks = if expected_work > 0.0 {
        total_work / expected_work
    } else {
        0.0
    };
    RoundStatus {
        blocks_found: rounds.map_or(0, |r| r.blocks_found.max(0) as u64),
        started_at: rounds.and_then(|r| r.last_found_at),
        work,
        expected_work,
        progress: if expected_work > 0.0 {
            work / expected_work
        } else {
            0.0
        },
        luck_percent,
        expected_blocks,
    }
}

/// Convert each block's winning share from stratum units to network units with its
/// own lane's share multiplier, so it compares to the block's difficulty next to it.
fn convert_blocks(mut blocks: Vec<BlockRow>, multiplier: impl Fn(&str) -> f64) -> Vec<BlockRow> {
    for b in &mut blocks {
        let m = multiplier(&b.lane);
        if m > 0.0 {
            b.share_diff /= m;
        }
    }
    blocks
}

/// The round of `coin` in `lane`: the same coin mined in another lane has its own.
fn lane_round<'a>(rounds: &'a [CoinRounds], lane: &str, coin: &str) -> Option<&'a CoinRounds> {
    rounds.iter().find(|r| r.lane == lane && r.coin == coin)
}

/// One lane's status. Its coins appear once they have had a template; node health rides
/// along; rounds are matched by lane and coin.
fn lane_status(l: &LaneAccount, rounds: &[CoinRounds], now: u64) -> LaneStatus {
    let stats = &l.stats;
    let workers = stats.workers(now);
    let hashrate = workers.iter().map(|w| w.hashrate).fold(0.0, |a, b| a + b);
    let total_work = stats.total_work();
    let share_multiplier = stats.share_multiplier();
    let figures = PoolFigures {
        hashrate,
        total_work,
        share_multiplier,
    };
    let coins = l.view.as_ref().map_or_else(Vec::new, |view| {
        view.nodes
            .iter()
            .filter_map(|(coin, health)| {
                let w = view.templates.get(coin)?;
                let r = lane_round(rounds, &l.key, coin);
                let health = health.borrow().clone();
                Some(coin_status(w, view.chain, figures, r, &health, now))
            })
            .collect()
    });
    let best = stats.best_share();
    LaneStatus {
        key: l.key.clone(),
        algorithm: l.algorithm.name().to_string(),
        stratum_port: l.stratum_port,
        state: l.state(),
        error: l.failed.clone(),
        chain: l.view.as_ref().map(|v| v.chain.to_string()),
        share_multiplier,
        hashrate,
        shares_accepted: stats.shares_accepted(),
        shares_rejected: stats.shares_rejected(),
        total_work,
        best_share_difficulty: best.as_ref().map_or(0.0, |b| b.difficulty),
        best_share_worker: best.as_ref().map(|b| b.worker.clone()),
        best_share_at: best.and_then(|b| b.at),
        scoring_since: stats.scoring_since(),
        configured_coins: l.configured_coins.clone(),
        coins,
        workers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::pool::lane::LaneUp;
    use alamo_store::BlockStatus;
    #[test]
    fn round_before_any_block_counts_all_work_and_has_no_luck() {
        let r = round_status(1_000.0, 250.0, None, 1.0);
        assert_eq!(r.blocks_found, 0);
        assert_eq!(r.started_at, None);
        assert_eq!(r.work, 250.0);
        assert_eq!(r.expected_work, 1_000.0);
        assert_eq!(r.progress, 0.25);
        assert_eq!(r.luck_percent, None);
        assert_eq!(r.expected_blocks, 0.25);
    }

    #[test]
    fn round_after_blocks_starts_at_the_banked_work() {
        let rounds = CoinRounds {
            lane: "ltc".into(),
            coin: "LTC".into(),
            blocks_found: 2,
            expected_work: 2_000.0,
            last_found_at: Some(500),
            last_work_at_found: Some(1_600.0),
        };
        // 2 blocks expected to take 2_000 work; the pool did 2_400: luck 83%.
        let r = round_status(1_000.0, 2_400.0, Some(&rounds), 1.0);
        let banked_in_share_units = CoinRounds {
            last_work_at_found: Some(1_600.0 * 65_536.0),
            ..rounds.clone()
        };
        assert_eq!(
            round_status(1_000.0, 2_400.0, Some(&banked_in_share_units), 65_536.0),
            r,
            "work banked by the store is in stratum share units"
        );
        assert_eq!(r.blocks_found, 2);
        assert_eq!(r.started_at, Some(500));
        assert_eq!(r.work, 800.0);
        assert_eq!(r.progress, 0.8);
        assert!((r.luck_percent.unwrap() - 83.333).abs() < 0.01);
        assert_eq!(r.expected_blocks, 2.4);
    }

    fn two_lanes() -> Vec<LaneConfig> {
        let coin = |key: &str, extra: &str| {
            format!(
                "[coins.{key}]\nrpc_url = \"http://127.0.0.1:1\"\nrpc_user = \"u\"\n\
                 rpc_password = \"p\"\nfallback_address = \"a\"\n{extra}\n"
            )
        };
        let text = format!(
            "[pool]\nname = \"x\"\ndata_dir = \"./data\"\n[stratum]\nlisten = \"127.0.0.1:0\"\n\
             [web]\nlisten = \"127.0.0.1:0\"\n{}{}",
            coin("ltc", ""),
            coin("doge", "[coins.doge.stratum]\nlisten = \"127.0.0.1:0\""),
        );
        Config::parse(&text).unwrap().lanes()
    }

    #[test]
    fn lane_coins_name_the_parent_first_then_its_aux_coins() {
        let text = "[pool]\nname = \"x\"\ndata_dir = \"./data\"\n[stratum]\nlisten = \"127.0.0.1:0\"\n\
             [web]\nlisten = \"127.0.0.1:0\"\n\
             [coins.ltc]\nrpc_url = \"http://127.0.0.1:1\"\nrpc_user = \"u\"\nrpc_password = \"p\"\nfallback_address = \"a\"\n\
             [coins.doge]\nmerge_mined_with = \"ltc\"\nrpc_url = \"http://127.0.0.1:1\"\nrpc_user = \"u\"\nrpc_password = \"p\"\nfallback_address = \"a\"\n";
        let lanes = Config::parse(text).unwrap().lanes();
        assert_eq!(lanes.len(), 1);
        let coins = lane_coins(&lanes[0]);
        let named: Vec<_> = coins
            .iter()
            .map(|c| (c.key.as_str(), c.symbol.as_str(), c.name.as_str()))
            .collect();
        assert_eq!(
            named,
            vec![("ltc", "LTC", "Litecoin"), ("doge", "DOGE", "Dogecoin")]
        );
    }

    #[tokio::test]
    async fn lanes_with_a_same_named_worker_and_session_id_do_not_mix() {
        let path = std::env::temp_dir()
            .join(format!("alamo-pool-lanes-{}", std::process::id()))
            .join("pool.db");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let store = Store::open(&path).await.unwrap();
        let lanes = two_lanes();
        assert_eq!(
            lanes.iter().map(|l| l.key.as_str()).collect::<Vec<_>>(),
            vec!["doge", "ltc"]
        );
        let primary_index = Config::primary_lane_index(&lanes);
        assert_eq!(lanes[primary_index].key, "ltc");
        let (doge, ltc) = (0, 1);
        let t0 = 1_700_000_000u64;
        let mut p = Publisher::load(store.clone(), &lanes, primary_index, t0).await;

        // Both stratum servers number their first session 1.
        let auth = |address: &str| PoolEvent::Authorized {
            session: 1,
            worker: "rig".into(),
            address: address.into(),
            fallback: false,
            aux: vec![],
        };
        let share = |job_difficulty: f64| PoolEvent::Share {
            session: 1,
            worker: "rig".into(),
            coin: "LTC",
            job_difficulty,
            share_difficulty: job_difficulty * 2.0,
            rejected: None,
        };
        p.observe(ltc, &auth("ltc1q"), t0).await;
        p.observe(doge, &auth("D1"), t0).await;
        for i in 0..3 {
            p.observe(ltc, &share(4.0), t0 + i).await;
        }
        p.observe(doge, &share(100.0), t0 + 1).await;
        // Session 1 ending on DOGE says nothing about session 1 on LTC.
        p.observe(
            doge,
            &PoolEvent::Disconnected {
                session: 1,
                workers: vec!["rig".into()],
            },
            t0 + 2,
        )
        .await;

        let l = &p.lanes[ltc].stats;
        let d = &p.lanes[doge].stats;
        assert_eq!((l.shares_accepted(), d.shares_accepted()), (3, 1));
        assert!(l.is_connected("rig"));
        assert!(!d.is_connected("rig"));
        assert_eq!(l.workers(t0 + 2)[0].address, "ltc1q");
        assert_eq!(d.workers(t0 + 2)[0].address, "D1");

        p.flush_all().await;
        let rows = |lane: &'static str| {
            let store = store.clone();
            async move { store.load_workers(lane).await.unwrap() }
        };
        assert_eq!(rows("ltc").await[0].shares_accepted, 3);
        assert_eq!(rows("ltc").await[0].work_accepted, 12.0);
        assert_eq!(rows("doge").await[0].shares_accepted, 1);
        assert_eq!(rows("doge").await[0].payout_address, "D1");

        // A snapshot from the start: every lane, primary first, still connecting.
        let snapshot = p.snapshot();
        let keys = |s: &PoolSnapshot| s.lanes.iter().map(|l| l.key.clone()).collect::<Vec<_>>();
        assert_eq!(keys(&snapshot), vec!["ltc", "doge"]);
        assert!(snapshot
            .lanes
            .iter()
            .all(|l| l.state == LaneState::Connecting && l.chain.is_none()));
        assert_eq!(
            snapshot.lanes[0].shares_accepted, 3,
            "restored figures per lane"
        );
        assert_eq!(snapshot.lanes[1].shares_accepted, 1);
        assert_eq!(snapshot.lanes[0].algorithm, "scrypt");
        assert_eq!(snapshot.lanes[0].share_multiplier, 65_536.0);

        // DOGE fails to start; LTC comes up.
        let (_work_tx, work) = watch::channel(None);
        p.notice(LaneNotice::Failed {
            lane: doge,
            error: "lane 'doge': wrong network".into(),
        });
        let snapshot = p.snapshot();
        assert_eq!(snapshot.lanes[1].state, LaneState::Failed);
        assert_eq!(
            snapshot.lanes[1].error.as_deref(),
            Some("lane 'doge': wrong network")
        );
        p.notice(LaneNotice::Up(LaneUp {
            lane: ltc,
            chain: Chain::Regtest,
            work,
            nodes: vec![],
        }));
        let snapshot = p.snapshot();
        let primary = &snapshot.lanes[0];
        assert_eq!(primary.state, LaneState::Running);
        assert_eq!(primary.chain.as_deref(), Some("regtest"));
        assert!(primary.error.is_none());
        assert_eq!(primary.shares_accepted, 3);
        assert_eq!(primary.share_multiplier, 65_536.0);
        assert_eq!(primary.best_share_worker.as_deref(), Some("rig"));
        assert_eq!(primary.workers.len(), 1);
        assert_eq!(primary.workers[0].address, "ltc1q");
        assert_eq!(primary.workers[0].connections, 1);
        assert_eq!(snapshot.lanes[1].workers[0].address, "D1");
        assert_eq!(snapshot.lanes[1].workers[0].connections, 0);

        // Operator commands act on the lane they name.
        p.command(Command::ResetStats { lane: "ltc".into() }).await;
        assert_eq!(p.lanes[ltc].stats.shares_accepted(), 0);
        assert_eq!(p.lanes[doge].stats.shares_accepted(), 1);
        assert_eq!(rows("doge").await[0].shares_accepted, 1);
        p.command(Command::RemoveWorker {
            lane: "ltc".into(),
            name: "rig".into(),
        })
        .await;
        assert_eq!(
            rows("ltc").await.len(),
            1,
            "a connected worker is not removed"
        );
        p.command(Command::RemoveWorker {
            lane: "doge".into(),
            name: "rig".into(),
        })
        .await;
        assert!(
            rows("doge").await.is_empty(),
            "idle in doge, so removed there"
        );
        assert_eq!(rows("ltc").await.len(), 1);
        p.command(Command::ResetStats {
            lane: "nope".into(),
        })
        .await;

        // Restored per lane on restart.
        let p = Publisher::load(store.clone(), &lanes, primary_index, t0 + 3).await;
        assert!(p.lanes[doge].stats.workers(t0 + 3).is_empty());
        assert_eq!(p.lanes[ltc].stats.shares_accepted(), 0);
        assert_eq!(p.lanes[ltc].stats.total_work(), 12.0 / 65_536.0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn blocks_convert_with_their_own_lane_and_rounds_match_lane_and_coin() {
        let block = |lane: &str, share_diff: f64| BlockRow {
            id: 1,
            lane: lane.into(),
            coin: "X".into(),
            height: 1,
            hash: "00".into(),
            worker: "w".into(),
            difficulty: 1.0,
            share_diff,
            reward_sats: None,
            found_at: 0,
            status: BlockStatus::Accepted,
            confirmations: 0,
            work_at_found: None,
        };
        let blocks = convert_blocks(vec![block("ltc", 65_536.0), block("btc", 3.0)], |lane| {
            if lane == "ltc" {
                65_536.0
            } else {
                1.0
            }
        });
        assert_eq!(blocks[0].share_diff, 1.0);
        assert_eq!(blocks[1].share_diff, 3.0);

        let round = |lane: &str, last: f64| CoinRounds {
            lane: lane.into(),
            coin: "DOGE".into(),
            blocks_found: 1,
            expected_work: 1.0,
            last_found_at: Some(1),
            last_work_at_found: Some(last),
        };
        let rounds = vec![round("doge", 10.0), round("ltc", 20.0)];
        assert_eq!(
            lane_round(&rounds, "ltc", "DOGE")
                .unwrap()
                .last_work_at_found,
            Some(20.0)
        );
        assert_eq!(
            lane_round(&rounds, "doge", "DOGE")
                .unwrap()
                .last_work_at_found,
            Some(10.0)
        );
        assert!(lane_round(&rounds, "ltc", "LTC").is_none());
    }
    #[tokio::test]
    async fn next_event_tags_each_event_with_its_lane_and_rotates() {
        let (a_tx, a_rx) = mpsc::channel(8);
        let (b_tx, b_rx) = mpsc::channel(8);
        let mut lanes = vec![Some(a_rx), Some(b_rx)];
        let ev = |session| PoolEvent::DifficultyChanged {
            session,
            difficulty: 1.0,
        };
        for i in 0..2 {
            a_tx.send(ev(i)).await.unwrap();
            b_tx.send(ev(10 + i)).await.unwrap();
        }
        let mut start = 0;
        let mut seen = Vec::new();
        for _ in 0..4 {
            let (lane, event) = next_event(&mut lanes, &mut start).await.unwrap();
            let PoolEvent::DifficultyChanged { session, .. } = event else {
                unreachable!()
            };
            seen.push((lane, session));
        }
        assert_eq!(
            seen,
            vec![(0, 0), (1, 10), (0, 1), (1, 11)],
            "no lane starves"
        );
        drop(a_tx);
        b_tx.send(ev(12)).await.unwrap();
        assert_eq!(next_event(&mut lanes, &mut start).await.unwrap().0, 1);
        drop(b_tx);
        assert!(next_event(&mut lanes, &mut start).await.is_none());
        assert!(lanes.iter().all(Option::is_none));
    }
}
