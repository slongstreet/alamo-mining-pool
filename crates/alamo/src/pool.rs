//! The pool runtime: node connections, template sources, stratum server, block submission,
//! confirmation tracking, and the status snapshot for the dashboard.

use crate::config::{Config, LaneConfig};
use crate::persist::{Persistence, Retention};
use crate::settings::{LiveSettings, LogSetup};
use crate::stats::Stats;
use alamo_coins::{
    merge, Chain, Coin, CoinConfig, NodeHealth, RpcClient, RpcError, TemplateSource, ZmqStatus,
    ZmqSubscriber,
};
use alamo_core::odds::OddsSummary;
use alamo_core::payout::{AuxPayoutTable, PayoutSet, PayoutTable};
use alamo_core::time::now_unix;
use alamo_core::work::MergedWork;
use alamo_core::work::WorkTemplate;
use alamo_core::Algorithm;
use alamo_store::{BlockRow, BlockStatus, CoinRounds, NewBlock, Store};
use alamo_stratum::server::Bound;
use alamo_stratum::{BlockCandidate, PoolEvent, StratumServer, WorkReceiver};
use alamo_web::{
    AppState, CoinStatus, Command, LaneCoin, LaneState, LaneStatus, NodeStatus, PoolSnapshot,
    RoundStatus,
};
use anyhow::{bail, Context};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// Directory under `data_dir` holding copies of blocks whose submission has not been
/// accepted: still retrying, interrupted by a shutdown, or rejected.
pub const BLOCK_SPOOL_DIR: &str = "blocks";

/// Result of handing a block to the node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// The node accepted the block.
    Accepted,
    /// The node rejected it with the given reason.
    Rejected(String),
}

impl SubmitOutcome {
    fn status(&self) -> BlockStatus {
        match self {
            SubmitOutcome::Accepted => BlockStatus::Accepted,
            SubmitOutcome::Rejected(_) => BlockStatus::Rejected,
        }
    }
}

/// A connected node for one coin.
pub struct ChainNode {
    /// Config key (`ltc`, `doge`).
    pub key: String,
    /// Coin definition.
    pub coin: Arc<dyn Coin>,
    /// Node client.
    pub rpc: RpcClient,
    /// Which network the node is on.
    pub chain: Chain,
    /// Username resolver for this coin.
    pub payouts: PayoutTable,
    /// The coin's configuration.
    pub config: CoinConfig,
}

/// The parent chain and the aux chains merge-mined with it.
pub struct Chains {
    /// The chain whose header is hashed.
    pub parent: ChainNode,
    /// Chains committed to in the parent coinbase.
    pub aux: Vec<ChainNode>,
}

impl Chains {
    /// Payout tables for every chain, for the stratum server.
    pub fn payout_set(&self) -> PayoutSet {
        PayoutSet {
            parent: self.parent.payouts.clone(),
            aux: self
                .aux
                .iter()
                .map(|node| AuxPayoutTable {
                    coin: node.coin.symbol(),
                    table: node.payouts.clone(),
                })
                .collect(),
        }
    }

    /// The parent, then every aux chain.
    pub fn nodes(&self) -> impl Iterator<Item = &ChainNode> {
        std::iter::once(&self.parent).chain(self.aux.iter())
    }
}

/// Delays between attempts to reach a node at startup.
const CONNECT_BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
];

/// Connect to one node, retrying until it answers or `shutdown` is cancelled (`Ok(None)`).
/// Configuration problems fail immediately; an unreachable node only logs and waits, so a
/// pool started before its nodes finish booting comes up on its own.
async fn connect_node(
    key: &str,
    cfg: &CoinConfig,
    shutdown: &CancellationToken,
) -> anyhow::Result<Option<ChainNode>> {
    let coin = alamo_coins::builtin(key).with_context(|| format!("unknown coin '{key}'"))?;
    if let Some(endpoint) = &cfg.zmq_hashblock {
        alamo_coins::zmq::parse_endpoint(endpoint)
            .with_context(|| format!("coins.{key}.zmq_hashblock"))?;
    }
    let rpc = RpcClient::new(&cfg.rpc_url, &cfg.rpc_user, &cfg.rpc_password);
    let mut attempt: usize = 0;
    let info = loop {
        match rpc.get_blockchain_info().await {
            Ok(info) => break info,
            Err(err) => {
                let delay = CONNECT_BACKOFF[attempt.min(CONNECT_BACKOFF.len() - 1)];
                attempt += 1;
                if attempt == 1 {
                    // A lane's listener opens only after every one of its nodes answers,
                    // so from a miner's side this looks like a dead pool rather than a
                    // node problem. Say so. Other lanes are not held up.
                    tracing::warn!(
                        coin = coin.symbol(),
                        "stratum is not listening until this node answers; miners will be \
                         refused or reset until then (check the node's RPC URL, credentials, \
                         and rpcallowip)"
                    );
                }
                tracing::warn!(
                    coin = coin.symbol(),
                    url = %cfg.rpc_url,
                    %err,
                    attempt,
                    retry_in_secs = delay.as_secs(),
                    "node unreachable"
                );
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = shutdown.cancelled() => return Ok(None),
                }
            }
        }
    };
    if let Some(why) = coin.unsupported_network(&info.chain) {
        bail!("coins.{key} node is on {}: {why}", info.chain);
    }
    let chain =
        Chain::parse(&info.chain).with_context(|| format!("unknown chain '{}'", info.chain))?;
    tracing::info!(coin = coin.symbol(), %chain, height = info.blocks, "node connected");
    let payouts = PayoutTable::new(coin.address_params(chain), &cfg.fallback_address)
        .with_context(|| {
            format!(
                "coins.{key}.fallback_address is not a valid {} address for {chain}",
                coin.symbol()
            )
        })?;
    Ok(Some(ChainNode {
        key: key.to_string(),
        coin,
        rpc,
        chain,
        payouts,
        config: cfg.clone(),
    }))
}

/// Connect to one lane's parent chain and every aux chain merge-mined with it, retrying
/// each node until it answers. `Ok(None)` means shutdown was requested while waiting.
pub async fn connect_lane(
    config: &Config,
    lane: &LaneConfig,
    shutdown: &CancellationToken,
) -> anyhow::Result<Option<Chains>> {
    let key = &lane.key;
    let cfg = config
        .coins
        .get(key)
        .with_context(|| format!("coin '{key}' is not configured"))?;
    let Some(parent) = connect_node(key, cfg, shutdown).await? else {
        return Ok(None);
    };

    let mut aux = Vec::new();
    for aux_key in &lane.aux {
        let aux_cfg = config
            .coins
            .get(aux_key)
            .with_context(|| format!("coin '{aux_key}' is not configured"))?;
        let Some(node) = connect_node(aux_key, aux_cfg, shutdown).await? else {
            return Ok(None);
        };
        if node.coin.aux_chain_id().is_none() {
            bail!("coin '{aux_key}' cannot be merge-mined");
        }
        if node.chain != parent.chain {
            bail!(
                "coin '{aux_key}' node is on {} but parent '{key}' is on {}",
                node.chain,
                parent.chain
            );
        }
        tracing::info!(
            coin = node.coin.symbol(),
            parent = parent.coin.symbol(),
            "merge mining"
        );
        aux.push(node);
    }
    Ok(Some(Chains { parent, aux }))
}

/// Delays between `submitblock` attempts when the node cannot be reached. A found block
/// is worth far more than the wait: keep trying for several minutes, quickly at first so
/// a node that is merely restarting its RPC server costs almost nothing.
const SUBMIT_RETRY_DELAYS: [Duration; 12] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(60),
    Duration::from_secs(60),
];

/// Bitcoin-derived nodes answer -28 while they are still starting up.
const RPC_IN_WARMUP: i64 = -28;

/// Whether a failed `submitblock` call says nothing about the block itself, so the same
/// block is worth sending again.
fn retryable(err: &RpcError) -> bool {
    match err {
        RpcError::Transport(_) => true,
        RpcError::Node { code, .. } => *code == RPC_IN_WARMUP,
        RpcError::Decode(_) | RpcError::Url(_) => false,
    }
}

/// Call `attempt` until the node gives a verdict or `delays` runs out.
///
/// A failed call may still have delivered the block (the reply was lost, or the node
/// went down right after connecting it), so once an attempt has failed, `duplicate` from
/// a later one means the node already has the block: that is an acceptance.
async fn submit_with_retry<F, Fut>(
    mut attempt: F,
    delays: &[Duration],
    coin: &'static str,
    hash: &str,
) -> SubmitOutcome
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Option<String>, RpcError>>,
{
    let mut failed = 0usize;
    loop {
        match attempt().await {
            Ok(None) => return SubmitOutcome::Accepted,
            Ok(Some(reason)) if failed > 0 && reason == "duplicate" => {
                tracing::info!(coin, %hash, "node already has the block from an earlier attempt");
                return SubmitOutcome::Accepted;
            }
            Ok(Some(reason)) => return SubmitOutcome::Rejected(reason),
            Err(err) => {
                let Some(delay) = delays.get(failed).filter(|_| retryable(&err)) else {
                    return SubmitOutcome::Rejected(format!("rpc: {err}"));
                };
                failed += 1;
                tracing::warn!(
                    coin, %hash, %err,
                    attempt = failed,
                    retry_in_ms = delay.as_millis() as u64,
                    "block submission failed; retrying"
                );
                tokio::time::sleep(*delay).await;
            }
        }
    }
}

/// Where a candidate's block is kept on disk while it is being submitted.
fn spool_path(dir: &Path, candidate: &BlockCandidate) -> PathBuf {
    dir.join(format!(
        "{}-{}-{}.hex",
        candidate.coin, candidate.height, candidate.block_hash
    ))
}

/// Write `block_hex` to `path` through a temporary file, so a partial write is never
/// mistaken for a block.
fn write_spool(path: &Path, block_hex: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("hex.tmp");
    std::fs::write(&tmp, block_hex)?;
    std::fs::rename(&tmp, path)
}

/// Submit a block candidate to the node and record the result. An unreachable node is
/// retried for several minutes.
///
/// `lane` is the parent coin's config key, recorded with the block (also for an aux block).
pub async fn submit_candidate(
    rpc: &RpcClient,
    store: &Store,
    lane: &str,
    candidate: &BlockCandidate,
) -> anyhow::Result<SubmitOutcome> {
    submit_candidate_spooled(rpc, store, lane, candidate, None).await
}

/// [`submit_candidate`], keeping a copy of the block under `spool` until the node accepts
/// it. The copy is written alongside the first attempt, never ahead of it, and is what an
/// operator hands to `submitblock` if the daemon dies or every attempt fails.
pub async fn submit_candidate_spooled(
    rpc: &RpcClient,
    store: &Store,
    lane: &str,
    candidate: &BlockCandidate,
    spool: Option<&Path>,
) -> anyhow::Result<SubmitOutcome> {
    let hex_block = Arc::new(hex::encode(&candidate.block));
    let spooled = spool.map(|dir| {
        let path = spool_path(dir, candidate);
        let (hex_block, target) = (hex_block.clone(), path.clone());
        let write = tokio::task::spawn_blocking(move || write_spool(&target, &hex_block));
        (path, write)
    });
    let outcome = submit_with_retry(
        || rpc.submit_block(&hex_block),
        &SUBMIT_RETRY_DELAYS,
        candidate.coin,
        &candidate.block_hash,
    )
    .await;
    let copy = match spooled {
        Some((path, write)) => match write.await {
            Ok(Ok(())) => Some(path),
            Ok(Err(err)) => {
                tracing::warn!(coin = candidate.coin, path = %path.display(), %err, "could not keep a copy of the block");
                None
            }
            Err(_) => None,
        },
        None => None,
    };
    match &outcome {
        SubmitOutcome::Accepted => {
            tracing::info!(
                coin = candidate.coin, height = candidate.height, hash = %candidate.block_hash,
                worker = %candidate.worker, address = %candidate.address, "block accepted by node"
            );
            if let Some(path) = &copy {
                let _ = std::fs::remove_file(path);
            }
        }
        SubmitOutcome::Rejected(reason) => tracing::error!(
            coin = candidate.coin, height = candidate.height, hash = %candidate.block_hash,
            %reason,
            copy = copy.as_ref().map(|p| p.display().to_string()),
            "block REJECTED by node"
        ),
    }
    store
        .insert_block(&NewBlock {
            lane: lane.to_string(),
            coin: candidate.coin.to_string(),
            height: candidate.height,
            hash: candidate.block_hash.clone(),
            worker: candidate.worker.clone(),
            difficulty: candidate.network_difficulty,
            share_diff: candidate.share_difficulty,
            reward_sats: Some(candidate.coinbase_value as i64),
            found_at: candidate.found_at,
            status: outcome.status(),
        })
        .await
        .context("recording block")?;
    Ok(outcome)
}

/// Re-check one coin's accepted blocks against its chain and update their status.
pub async fn track_confirmations(
    rpc: &RpcClient,
    coin: &dyn Coin,
    chain: Chain,
    store: &Store,
) -> anyhow::Result<()> {
    for block in store.unsettled_blocks(coin.symbol()).await? {
        let info = match rpc.get_block_info(&block.hash).await {
            Ok(info) => info,
            Err(err) => {
                tracing::warn!(coin = coin.symbol(), hash = %block.hash, %err, "could not check block");
                continue;
            }
        };
        let status = if info.confirmations < 0 {
            tracing::warn!(coin = coin.symbol(), height = block.height, hash = %block.hash, "block orphaned");
            BlockStatus::Orphaned
        } else if info.confirmations >= coin.coinbase_maturity(chain) {
            BlockStatus::Confirmed
        } else {
            BlockStatus::Accepted
        };
        store
            .set_block_status(block.id, status, info.confirmations)
            .await?;
    }
    Ok(())
}

/// A node's template source plus the tasks and channels that feed it.
struct NodeFeed {
    source: TemplateSource,
    health: watch::Receiver<NodeHealth>,
    zmq: Option<ZmqFeed>,
}

fn node_feed(node: &ChainNode, tag: watch::Receiver<Vec<u8>>) -> NodeFeed {
    let (health_tx, health) = watch::channel(NodeHealth::default());
    let (zmq, zmq_rx) = match &node.config.zmq_hashblock {
        Some(endpoint) => {
            let (status_tx, status_rx) = watch::channel(ZmqStatus::default());
            // The subscriber owns the sender; the source watches the receiver.
            let subscriber = ZmqSubscriber {
                endpoint: endpoint.clone(),
                coin: node.coin.symbol(),
            };
            (
                Some(ZmqFeed {
                    subscriber,
                    status: status_tx,
                }),
                Some(status_rx),
            )
        }
        None => (None, None),
    };
    let source = TemplateSource {
        poll_interval: Duration::from_millis(node.config.poll_interval_ms),
        refresh_interval: Duration::from_secs(node.config.template_refresh_secs),
        stale_after: Duration::from_secs(node.config.template_stale_secs),
        zmq: zmq_rx,
        health: Some(health_tx),
        coinbase_tag: tag,
        ..TemplateSource::new(node.rpc.clone(), node.coin.clone(), Vec::new())
    };
    NodeFeed {
        source,
        health,
        zmq,
    }
}

/// A ZMQ subscriber and the channel it reports on.
struct ZmqFeed {
    subscriber: ZmqSubscriber,
    status: watch::Sender<ZmqStatus>,
}

/// Start a node's template source and, if configured, its ZMQ subscriber.
fn spawn_feed(
    tasks: &mut JoinSet<&'static str>,
    feed: NodeFeed,
    tx: watch::Sender<Option<Arc<WorkTemplate>>>,
    shutdown: &CancellationToken,
    name: &'static str,
) -> watch::Receiver<NodeHealth> {
    let token = shutdown.child_token();
    tasks.spawn(async move {
        feed.source.run(tx, token).await;
        name
    });
    if let Some(zmq) = feed.zmq {
        let token = shutdown.child_token();
        tasks.spawn(async move {
            zmq.subscriber.run(zmq.status, token).await;
            "zmq subscriber"
        });
    }
    feed.health
}

/// Run the pool until `shutdown` is cancelled or a component stops.
///
/// Each enabled parent coin is a lane with its own nodes, stratum listener, vardiff,
/// payout tables, submitter, and trackers. Lanes start independently: one whose node is
/// unreachable keeps retrying without holding any other lane's listener closed.
///
/// A lane that cannot start (its node is on another network than the config implies, a
/// fallback address does not fit that network, its listener cannot bind) fails on its
/// own: it is logged, shown as failed, and the other lanes keep running. The daemon exits
/// with that error only when no lane is left running or connecting, so a single-lane
/// pool with a bad config stops as it always did. A component of a running lane that
/// stops unexpectedly still stops the daemon, so its supervisor restarts it.
pub async fn run(
    config: Config,
    store: Store,
    state: AppState,
    log: LogSetup,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let lanes = config.lanes();
    if lanes.is_empty() {
        bail!("no enabled parent coin configured");
    }
    let primary = Config::primary_lane_index(&lanes);
    state.set_primary_lane(&lanes[primary].key);
    // Settings stored in the database override the file; the pool tasks below watch
    // the channels this owns, so a change from the dashboard reaches them live.
    let settings = LiveSettings::start(&config, store.clone(), state.clone(), log)
        .await
        .context("loading settings")?;
    state.install_operator(settings.clone());

    let config = Arc::new(config);
    let spool = config.pool.data_dir.join(BLOCK_SPOOL_DIR);
    let mut tasks: JoinSet<anyhow::Result<TaskEnd>> = JoinSet::new();
    let (notice_tx, notice_rx) = mpsc::channel::<LaneNotice>(lanes.len());
    // One event channel per lane: which channel an event arrives on is its lane, so
    // nothing is added to the per-share path.
    let mut events = Vec::with_capacity(lanes.len());
    for (index, lane) in lanes.iter().enumerate() {
        let (events_tx, events_rx) = mpsc::channel::<PoolEvent>(4096);
        events.push(Some(events_rx));
        tasks.spawn(run_lane(LaneTask {
            index,
            lane: lane.clone(),
            config: config.clone(),
            settings: settings.clone(),
            store: store.clone(),
            spool: spool.clone(),
            events: events_tx,
            notices: notice_tx.clone(),
            shutdown: shutdown.child_token(),
        }));
    }
    drop(notice_tx);

    let commands = state
        .take_commands()
        .context("web command receiver already taken")?;
    let publisher_state = Publisher::load(store.clone(), &lanes, primary, now_unix()).await;
    let token = shutdown.child_token();
    tasks.spawn(async move {
        publisher(
            publisher_state,
            events,
            notice_rx,
            state,
            commands,
            token.clone(),
        )
        .await;
        if token.is_cancelled() {
            Ok(TaskEnd::Done)
        } else {
            bail!("publisher stopped unexpectedly")
        }
    });

    // Lanes still running or connecting.
    let mut alive = lanes.len();
    let result = loop {
        tokio::select! {
            _ = shutdown.cancelled() => break Ok(()),
            finished = tasks.join_next() => match finished {
                // A task that saw the shutdown.
                Some(Ok(Ok(TaskEnd::Done))) => continue,
                Some(Ok(Ok(TaskEnd::LaneFailed(err)))) => {
                    alive -= 1;
                    if alive == 0 {
                        break Err(err.context("no lane is left running"));
                    }
                }
                Some(Ok(Err(err))) => break Err(err),
                Some(Err(join)) => break Err(anyhow::Error::new(join).context("pool task panicked")),
                None => break Ok(()),
            },
        }
    };
    shutdown.cancel();
    while tasks.join_next().await.is_some() {}
    result
}

/// How a pool task ended without stopping the daemon.
enum TaskEnd {
    /// It saw the shutdown.
    Done,
    /// A lane could not start; the others carry on.
    LaneFailed(anyhow::Error),
}

/// Everything one lane's task tree needs.
struct LaneTask {
    /// Position in [`Config::lanes`], which is also the publisher's index for it.
    index: usize,
    lane: LaneConfig,
    config: Arc<Config>,
    settings: Arc<LiveSettings>,
    store: Store,
    spool: PathBuf,
    events: mpsc::Sender<PoolEvent>,
    notices: mpsc::Sender<LaneNotice>,
    shutdown: CancellationToken,
}

/// What a lane tells the publisher about itself.
enum LaneNotice {
    /// Nodes connected and listener open.
    Up(LaneUp),
    /// The lane gave up starting.
    Failed {
        /// Lane index.
        lane: usize,
        /// The reason, with its causes.
        error: String,
    },
}

/// A lane's nodes have connected and its listener is bound: what the publisher needs to
/// show it.
struct LaneUp {
    lane: usize,
    chain: Chain,
    work: WorkReceiver,
    /// Node health, parent first, in the order the snapshot lists coins.
    nodes: Vec<(&'static str, watch::Receiver<NodeHealth>)>,
}

/// A lane that has connected and bound its listener, before any of its tasks run.
struct StartedLane {
    chains: Chains,
    bound: Bound,
    work_tx: watch::Sender<Option<Arc<MergedWork>>>,
    work_rx: WorkReceiver,
    blocks_rx: mpsc::Receiver<BlockCandidate>,
}

/// Connect one lane, then run its template sources, merger, stratum server, submitter,
/// and trackers until shutdown or until one of them stops. A failure while starting is
/// this lane's alone (`TaskEnd::LaneFailed`); a failure once running is the daemon's.
async fn run_lane(task: LaneTask) -> anyhow::Result<TaskEnd> {
    let key = task.lane.key.clone();
    let started = match start_lane(&task).await {
        Ok(Some(started)) => started,
        Ok(None) => return Ok(TaskEnd::Done),
        Err(err) => {
            let err = err.context(format!("lane '{key}'"));
            let error = format!("{err:#}");
            tracing::error!(lane = %key, %error, "lane failed to start; other lanes keep running");
            task.settings.lane_detached(&key).await;
            let _ = task
                .notices
                .send(LaneNotice::Failed {
                    lane: task.index,
                    error,
                })
                .await;
            return Ok(TaskEnd::LaneFailed(err));
        }
    };
    run_started_lane(task, started)
        .await
        .map(|()| TaskEnd::Done)
        .with_context(|| format!("lane '{key}'"))
}

/// Everything that can fail because of the configuration: connecting (and checking) the
/// nodes, attaching the settings, binding the listener. Nothing is spawned yet, so a
/// failure leaves nothing behind. `Ok(None)` is a shutdown while waiting for a node.
async fn start_lane(task: &LaneTask) -> anyhow::Result<Option<StartedLane>> {
    let Some(chains) = connect_lane(&task.config, &task.lane, &task.shutdown).await? else {
        return Ok(None);
    };
    let payouts = task.settings.lane_connected(&chains).await?;
    let attached = async {
        let vardiff = task
            .settings
            .vardiff(&task.lane.key)
            .context("no vardiff settings for this lane")?;
        let (work_tx, work_rx) = watch::channel(None);
        let (blocks_tx, blocks_rx) = mpsc::channel::<BlockCandidate>(64);
        let bound = StratumServer {
            listen: task.lane.stratum.listen,
            vardiff,
            payouts,
            work: work_rx.clone(),
            events: task.events.clone(),
            blocks: blocks_tx,
            version_mask: chains.parent.coin.version_rolling_mask(),
        }
        .bind()
        .await
        .context("binding stratum")?;
        anyhow::Ok((bound, work_tx, work_rx, blocks_rx))
    }
    .await;
    let (bound, work_tx, work_rx, blocks_rx) = attached?;
    Ok(Some(StartedLane {
        chains,
        bound,
        work_tx,
        work_rx,
        blocks_rx,
    }))
}

async fn run_started_lane(task: LaneTask, started: StartedLane) -> anyhow::Result<()> {
    let LaneTask {
        index,
        lane,
        settings,
        store,
        spool,
        notices,
        shutdown,
        ..
    } = task;
    let StartedLane {
        chains,
        bound,
        work_tx,
        work_rx,
        blocks_rx,
    } = started;

    // Every task returns its name when it stops; none of them stops on its own.
    let mut tasks: JoinSet<&'static str> = JoinSet::new();

    // Node health, parent first, in the order the snapshot lists coins.
    let mut nodes: Vec<(&'static str, watch::Receiver<NodeHealth>)> = Vec::new();
    let (parent_tx, parent_rx) = watch::channel::<Option<Arc<WorkTemplate>>>(None);
    let health = spawn_feed(
        &mut tasks,
        node_feed(&chains.parent, settings.coinbase_tag()),
        parent_tx,
        &shutdown,
        "template source",
    );
    nodes.push((chains.parent.coin.symbol(), health));
    let mut aux_rxs = Vec::with_capacity(chains.aux.len());
    for node in &chains.aux {
        let (aux_tx, aux_rx) = watch::channel::<Option<Arc<WorkTemplate>>>(None);
        let health = spawn_feed(
            &mut tasks,
            node_feed(node, settings.coinbase_tag()),
            aux_tx,
            &shutdown,
            "aux template source",
        );
        nodes.push((node.coin.symbol(), health));
        aux_rxs.push(aux_rx);
    }
    let token = shutdown.child_token();
    tasks.spawn(async move {
        merge(parent_rx, aux_rxs, work_tx, token).await;
        "work merger"
    });

    let token = shutdown.child_token();
    tasks.spawn(async move {
        bound.run(token).await;
        "stratum"
    });

    let rpcs: HashMap<&'static str, RpcClient> = chains
        .nodes()
        .map(|node| (node.coin.symbol(), node.rpc.clone()))
        .collect();
    tasks.spawn(submitter(
        rpcs,
        store.clone(),
        lane.key.clone(),
        spool,
        blocks_rx,
        shutdown.child_token(),
    ));
    for node in chains.nodes() {
        tasks.spawn(tracker(
            node.rpc.clone(),
            node.coin.clone(),
            node.chain,
            store.clone(),
            shutdown.child_token(),
        ));
    }
    let announced = notices
        .send(LaneNotice::Up(LaneUp {
            lane: index,
            chain: chains.parent.chain,
            work: work_rx,
            nodes,
        }))
        .await;

    let result = if announced.is_err() {
        Err(anyhow::anyhow!("publisher is gone"))
    } else {
        tokio::select! {
            _ = shutdown.cancelled() => Ok(()),
            finished = tasks.join_next() => match finished {
                Some(Ok(name)) => Err(anyhow::anyhow!("{name} stopped unexpectedly")),
                Some(Err(join)) => Err(anyhow::Error::new(join).context("pool task panicked")),
                None => Ok(()),
            },
        }
    };
    shutdown.cancel();
    while tasks.join_next().await.is_some() {}
    result
}

/// Hand every block candidate to its node. Each submission runs on its own, so a node
/// that is slow or down never holds up a block for another chain; on shutdown the
/// submissions still retrying are dropped and their copies stay in `spool`.
async fn submitter(
    rpcs: HashMap<&'static str, RpcClient>,
    store: Store,
    lane: String,
    spool: PathBuf,
    mut blocks: mpsc::Receiver<BlockCandidate>,
    shutdown: CancellationToken,
) -> &'static str {
    let mut pending: JoinSet<()> = JoinSet::new();
    loop {
        tokio::select! {
            next = blocks.recv() => {
                let Some(candidate) = next else { return "submitter" };
                let Some(rpc) = rpcs.get(candidate.coin).cloned() else {
                    tracing::error!(coin = candidate.coin, "no node for block candidate");
                    continue;
                };
                let (store, lane, spool) = (store.clone(), lane.clone(), spool.clone());
                pending.spawn(async move {
                    let submitted =
                        submit_candidate_spooled(&rpc, &store, &lane, &candidate, Some(&spool))
                            .await;
                    if let Err(err) = submitted {
                        tracing::error!(coin = candidate.coin, %err, "block submission failed");
                    }
                });
            }
            Some(done) = pending.join_next() => {
                if let Err(join) = done {
                    tracing::error!(%join, "block submission task failed");
                }
            }
            _ = shutdown.cancelled() => {
                if !pending.is_empty() {
                    tracing::warn!(
                        pending = pending.len(),
                        spool = %spool.display(),
                        "shutting down with block submissions still retrying"
                    );
                }
                return "submitter";
            }
        }
    }
}

async fn tracker(
    rpc: RpcClient,
    coin: Arc<dyn Coin>,
    chain: Chain,
    store: Store,
    shutdown: CancellationToken,
) -> &'static str {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(err) = track_confirmations(&rpc, coin.as_ref(), chain, &store).await {
                    tracing::warn!(coin = coin.symbol(), %err, "confirmation tracking failed");
                }
            }
            _ = shutdown.cancelled() => return "tracker",
        }
    }
}

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
struct Publisher {
    store: Store,
    lanes: Vec<LaneAccount>,
    primary: usize,
    retention: Retention,
    blocks: Vec<BlockRow>,
    rounds: Vec<CoinRounds>,
    ticks: u32,
}

/// The algorithm of a lane's parent coin.
fn lane_algorithm(key: &str) -> Algorithm {
    alamo_coins::builtin(key).map_or(Algorithm::Scrypt, |c| c.algorithm())
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
    async fn load(store: Store, lanes: &[LaneConfig], primary: usize, now: u64) -> Self {
        let mut states = Vec::with_capacity(lanes.len());
        for lane in lanes {
            let algorithm = lane_algorithm(&lane.key);
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

    /// The status document: every lane (primary first), with the deprecated top-level
    /// fields mirroring the primary lane while it runs, and every lane's blocks.
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

async fn publisher(
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
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// A real transport error: nothing listens on port 1.
    async fn transport_error() -> RpcError {
        RpcClient::new("http://127.0.0.1:1", "u", "p")
            .submit_block("00")
            .await
            .expect_err("port 1 refuses connections")
    }

    /// Run `submit_with_retry` against scripted node answers; returns the outcome and how
    /// many attempts were made.
    async fn scripted(
        answers: Vec<Result<Option<String>, RpcError>>,
        retries: usize,
    ) -> (SubmitOutcome, usize) {
        let total = answers.len();
        let answers = RefCell::new(VecDeque::from(answers));
        let delays = vec![Duration::ZERO; retries];
        let outcome = submit_with_retry(
            || {
                let next = answers
                    .borrow_mut()
                    .pop_front()
                    .expect("unexpected attempt");
                async move { next }
            },
            &delays,
            "BTC",
            "00ff",
        )
        .await;
        let used = total - answers.borrow().len();
        (outcome, used)
    }

    #[tokio::test]
    async fn unreachable_node_is_retried_until_it_accepts() {
        let answers = vec![
            Err(transport_error().await),
            Err(RpcError::Node {
                code: RPC_IN_WARMUP,
                message: "Loading block index".into(),
            }),
            Ok(None),
        ];
        assert_eq!(scripted(answers, 5).await, (SubmitOutcome::Accepted, 3));
    }

    #[tokio::test]
    async fn duplicate_after_a_failed_attempt_is_an_acceptance() {
        let answers = vec![Err(transport_error().await), Ok(Some("duplicate".into()))];
        assert_eq!(scripted(answers, 5).await, (SubmitOutcome::Accepted, 2));
        // With no failed attempt before it, the node knew the block from someone else.
        let answers = vec![Ok(Some("duplicate".into()))];
        assert_eq!(
            scripted(answers, 5).await,
            (SubmitOutcome::Rejected("duplicate".into()), 1)
        );
    }

    #[tokio::test]
    async fn a_verdict_or_a_permanent_error_is_not_retried() {
        let answers = vec![Ok(Some("high-hash".into())), Ok(None)];
        assert_eq!(
            scripted(answers, 5).await,
            (SubmitOutcome::Rejected("high-hash".into()), 1)
        );
        let answers = vec![
            Err(RpcError::Node {
                code: -22,
                message: "Block decode failed".into(),
            }),
            Ok(None),
        ];
        let (outcome, used) = scripted(answers, 5).await;
        assert_eq!(used, 1);
        assert!(matches!(outcome, SubmitOutcome::Rejected(r) if r.contains("Block decode failed")));
    }

    #[tokio::test]
    async fn retries_stop_when_the_schedule_runs_out() {
        let answers = vec![
            Err(transport_error().await),
            Err(transport_error().await),
            Err(transport_error().await),
        ];
        let (outcome, used) = scripted(answers, 2).await;
        assert_eq!(used, 3, "one attempt plus two retries");
        assert!(matches!(outcome, SubmitOutcome::Rejected(r) if r.starts_with("rpc: ")));
    }

    #[test]
    fn spool_copy_is_written_whole_under_the_block_name() {
        let dir = std::env::temp_dir().join(format!("alamo-spool-{}", std::process::id()));
        let path = dir.join("nested").join("BTC-1-00ff.hex");
        write_spool(&path, "deadbeef").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "deadbeef");
        assert!(!path.with_extension("hex.tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

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

        // A snapshot from the start: every lane, primary first, still connecting, with
        // the deprecated top-level fields at their defaults.
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
        assert_eq!(snapshot.shares_accepted, 0);
        assert!(snapshot.workers.is_empty());
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
        assert_eq!(snapshot.shares_accepted, 0, "primary still connecting");
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
        // The deprecated fields mirror the primary lane exactly.
        assert_eq!(snapshot.shares_accepted, primary.shares_accepted);
        assert_eq!(snapshot.shares_rejected, primary.shares_rejected);
        assert_eq!(snapshot.hashrate, primary.hashrate);
        assert_eq!(snapshot.total_work, primary.total_work);
        assert_eq!(snapshot.share_multiplier, primary.share_multiplier);
        assert_eq!(
            snapshot.best_share_difficulty,
            primary.best_share_difficulty
        );
        assert_eq!(snapshot.best_share_worker, primary.best_share_worker);
        assert_eq!(snapshot.scoring_since, primary.scoring_since);
        assert_eq!(snapshot.workers.len(), 1);
        assert_eq!(snapshot.workers[0].address, "ltc1q");
        assert_eq!(snapshot.workers[0].connections, 1);
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
