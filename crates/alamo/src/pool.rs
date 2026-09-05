//! The pool runtime: node connections, template sources, stratum server, block submission,
//! confirmation tracking, and the status snapshot for the dashboard.

use crate::config::Config;
use crate::persist::Persistence;
use crate::stats::Stats;
use alamo_coins::{
    merge, Chain, Coin, CoinConfig, NodeHealth, RpcClient, TemplateSource, ZmqStatus, ZmqSubscriber,
};
use alamo_core::odds::OddsSummary;
use alamo_core::payout::{AuxPayoutTable, PayoutSet, PayoutTable};
use alamo_core::time::now_unix;
use alamo_core::work::WorkTemplate;
use alamo_store::{BlockRow, BlockStatus, CoinRounds, NewBlock, Store};
use alamo_stratum::{BlockCandidate, PoolEvent, StratumServer, WorkReceiver};
use alamo_web::{AppState, CoinStatus, NodeStatus, PoolSnapshot, RoundStatus};
use anyhow::{bail, Context};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

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

    fn nodes(&self) -> impl Iterator<Item = &ChainNode> {
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

/// Connect to the configured parent chain and every aux chain merge-mined with it.
/// `Ok(None)` means shutdown was requested while waiting for a node.
pub async fn connect_chains(
    config: &Config,
    shutdown: &CancellationToken,
) -> anyhow::Result<Option<Chains>> {
    let parents: Vec<(&String, &CoinConfig)> = config
        .coins
        .iter()
        .filter(|(_, c)| c.enabled && c.merge_mined_with.is_none())
        .collect();
    let (key, cfg) = match parents.as_slice() {
        [one] => *one,
        [] => bail!("no enabled parent coin configured"),
        many => bail!(
            "only one parent chain is supported for now; found {}",
            many.len()
        ),
    };
    let Some(parent) = connect_node(key, cfg, shutdown).await? else {
        return Ok(None);
    };

    let mut aux = Vec::new();
    for (aux_key, aux_cfg) in config
        .coins
        .iter()
        .filter(|(_, c)| c.enabled && c.merge_mined_with.as_deref() == Some(key.as_str()))
    {
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

/// Submit a block candidate to the node and record the result.
pub async fn submit_candidate(
    rpc: &RpcClient,
    store: &Store,
    candidate: &BlockCandidate,
) -> anyhow::Result<SubmitOutcome> {
    let hex_block = hex::encode(&candidate.block);
    let outcome = match rpc.submit_block(&hex_block).await {
        Ok(None) => SubmitOutcome::Accepted,
        Ok(Some(reason)) => SubmitOutcome::Rejected(reason),
        Err(err) => SubmitOutcome::Rejected(format!("rpc: {err}")),
    };
    match &outcome {
        SubmitOutcome::Accepted => tracing::info!(
            coin = candidate.coin, height = candidate.height, hash = %candidate.block_hash,
            worker = %candidate.worker, address = %candidate.address, "block accepted by node"
        ),
        SubmitOutcome::Rejected(reason) => tracing::error!(
            coin = candidate.coin, height = candidate.height, hash = %candidate.block_hash,
            %reason, "block REJECTED by node"
        ),
    }
    store
        .insert_block(&NewBlock {
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

fn node_feed(node: &ChainNode) -> NodeFeed {
    let tag = node
        .config
        .coinbase_tag
        .clone()
        .unwrap_or_else(|| "/alamo/".into())
        .into_bytes();
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
        ..TemplateSource::new(node.rpc.clone(), node.coin.clone(), tag)
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
pub async fn run(
    config: Config,
    store: Store,
    state: AppState,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let Some(chains) = connect_chains(&config, &shutdown).await? else {
        return Ok(());
    };

    let (work_tx, work_rx) = watch::channel(None);
    let (events_tx, events_rx) = mpsc::channel::<PoolEvent>(4096);
    let (blocks_tx, blocks_rx) = mpsc::channel::<BlockCandidate>(64);

    // Every task returns its name when it stops; none of them stops on its own.
    let mut tasks: JoinSet<&'static str> = JoinSet::new();

    // Node health, parent first, in the order the snapshot lists coins.
    let mut nodes: Vec<(&'static str, watch::Receiver<NodeHealth>)> = Vec::new();
    let (parent_tx, parent_rx) = watch::channel::<Option<Arc<WorkTemplate>>>(None);
    let health = spawn_feed(
        &mut tasks,
        node_feed(&chains.parent),
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
            node_feed(node),
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

    let bound = StratumServer {
        config: config.stratum.clone(),
        work: work_rx.clone(),
        payouts: Arc::new(chains.payout_set()),
        events: events_tx,
        blocks: blocks_tx,
    }
    .bind()
    .await
    .context("binding stratum")?;
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
    tasks.spawn(publisher(
        events_rx,
        work_rx,
        nodes,
        store.clone(),
        state,
        chains.parent.chain,
        shutdown.child_token(),
    ));

    let result = tokio::select! {
        _ = shutdown.cancelled() => Ok(()),
        finished = tasks.join_next() => match finished {
            Some(Ok(name)) => bail!("{name} stopped unexpectedly"),
            Some(Err(join)) => Err(anyhow::Error::new(join).context("pool task panicked")),
            None => Ok(()),
        },
    };
    shutdown.cancel();
    while tasks.join_next().await.is_some() {}
    result
}

async fn submitter(
    rpcs: HashMap<&'static str, RpcClient>,
    store: Store,
    mut blocks: mpsc::Receiver<BlockCandidate>,
    shutdown: CancellationToken,
) -> &'static str {
    loop {
        tokio::select! {
            next = blocks.recv() => {
                let Some(candidate) = next else { return "submitter" };
                let Some(rpc) = rpcs.get(candidate.coin) else {
                    tracing::error!(coin = candidate.coin, "no node for block candidate");
                    continue;
                };
                if let Err(err) = submit_candidate(rpc, &store, &candidate).await {
                    tracing::error!(coin = candidate.coin, %err, "block submission failed");
                }
            }
            _ = shutdown.cancelled() => return "submitter",
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

async fn publisher(
    mut events: mpsc::Receiver<PoolEvent>,
    work: WorkReceiver,
    nodes: Vec<(&'static str, watch::Receiver<NodeHealth>)>,
    store: Store,
    state: AppState,
    chain: Chain,
    shutdown: CancellationToken,
) -> &'static str {
    let now = now_unix();
    let mut stats = match Stats::load(&store, now).await {
        Ok(s) => {
            tracing::info!(
                workers = s.workers(now).len(),
                shares_accepted = s.shares_accepted(),
                "restored stats"
            );
            s
        }
        Err(err) => {
            tracing::warn!(%err, "could not restore stats");
            Stats::default()
        }
    };
    let mut persist = Persistence::new(store.clone(), now);
    let mut blocks: Vec<BlockRow> = Vec::new();
    let mut rounds: Vec<CoinRounds> = Vec::new();
    // Last template seen per coin, so a coin whose template was withdrawn keeps its
    // place on the dashboard, flagged stale, instead of vanishing.
    let mut templates: HashMap<&'static str, Arc<WorkTemplate>> = HashMap::new();
    let mut ticks: u32 = 0;
    let mut interval = tokio::time::interval(PUBLISH_INTERVAL);
    loop {
        tokio::select! {
            next = events.recv() => {
                let Some(event) = next else {
                    flush_persist(&mut persist).await;
                    return "publisher";
                };
                let ts = now_unix();
                persist.observe(&event, ts);
                stats.apply(&event, ts);
                if persist.should_flush() {
                    flush_persist(&mut persist).await;
                }
            }
            _ = interval.tick() => {
                if let Err(err) = persist.on_tick(&stats, now_unix()).await {
                    tracing::warn!(%err, "could not persist accounting");
                }
                if ticks % BLOCKS_REFRESH_TICKS == 0 {
                    match store.recent_blocks(25).await {
                        Ok(rows) => blocks = rows,
                        Err(err) => tracing::warn!(%err, "could not read blocks"),
                    }
                    match store.coin_rounds().await {
                        Ok(rows) => rounds = rows,
                        Err(err) => tracing::warn!(%err, "could not read rounds"),
                    }
                }
                ticks = ticks.wrapping_add(1);
                remember_templates(&work, &mut templates);
                state.publish(build_snapshot(
                    &stats,
                    &templates,
                    &nodes,
                    blocks.clone(),
                    &rounds,
                    chain,
                ));
            }
            _ = shutdown.cancelled() => {
                flush_persist(&mut persist).await;
                return "publisher";
            }
        }
    }
}

async fn flush_persist(persist: &mut Persistence) {
    if let Err(err) = persist.flush().await {
        tracing::warn!(%err, "could not persist accounting");
    }
}

fn coin_status(
    w: &WorkTemplate,
    chain: Chain,
    hashrate: f64,
    total_work: f64,
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
        odds: OddsSummary::compute(hashrate, network_difficulty),
        round: round_status(network_difficulty, total_work, rounds),
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
fn round_status(
    network_difficulty: f64,
    total_work: f64,
    rounds: Option<&CoinRounds>,
) -> RoundStatus {
    let banked = rounds.and_then(|r| r.last_work_at_found).unwrap_or(0.0);
    let work = (total_work - banked).max(0.0);
    let expected_work = network_difficulty;
    let luck_percent = rounds
        .filter(|r| r.blocks_found > 0 && total_work > 0.0)
        .map(|r| alamo_core::odds::luck_percent(r.expected_work, total_work));
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
    }
}

fn build_snapshot(
    stats: &Stats,
    templates: &HashMap<&'static str, Arc<WorkTemplate>>,
    nodes: &[(&'static str, watch::Receiver<NodeHealth>)],
    blocks: Vec<BlockRow>,
    rounds: &[CoinRounds],
    chain: Chain,
) -> PoolSnapshot {
    let now = now_unix();
    let workers = stats.workers(now);
    let hashrate = workers.iter().map(|w| w.hashrate).fold(0.0, |a, b| a + b);
    let total_work = stats.total_work();
    // Coins appear once they have had a template; node health rides along.
    let coins = nodes
        .iter()
        .filter_map(|(coin, health)| {
            let w = templates.get(coin)?;
            let r = rounds.iter().find(|r| r.coin == *coin);
            let health = health.borrow().clone();
            Some(coin_status(w, chain, hashrate, total_work, r, &health, now))
        })
        .collect();
    PoolSnapshot {
        now,
        coins,
        hashrate,
        shares_accepted: stats.shares_accepted(),
        shares_rejected: stats.shares_rejected(),
        total_work,
        best_share_difficulty: stats.best_difficulty(),
        workers,
        blocks,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_before_any_block_counts_all_work_and_has_no_luck() {
        let r = round_status(1_000.0, 250.0, None);
        assert_eq!(r.blocks_found, 0);
        assert_eq!(r.started_at, None);
        assert_eq!(r.work, 250.0);
        assert_eq!(r.expected_work, 1_000.0);
        assert_eq!(r.progress, 0.25);
        assert_eq!(r.luck_percent, None);
    }

    #[test]
    fn round_after_blocks_starts_at_the_banked_work() {
        let rounds = CoinRounds {
            coin: "LTC".into(),
            blocks_found: 2,
            expected_work: 2_000.0,
            last_found_at: Some(500),
            last_work_at_found: Some(1_600.0),
        };
        // 2 blocks expected to take 2_000 work; the pool did 2_400: luck 83%.
        let r = round_status(1_000.0, 2_400.0, Some(&rounds));
        assert_eq!(r.blocks_found, 2);
        assert_eq!(r.started_at, Some(500));
        assert_eq!(r.work, 800.0);
        assert_eq!(r.progress, 0.8);
        assert!((r.luck_percent.unwrap() - 83.333).abs() < 0.01);
    }
}
