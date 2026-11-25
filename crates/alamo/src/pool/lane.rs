//! One lane's task tree: its template feeds, work merger, stratum server, submitter, and
//! trackers, and what it tells the publisher about itself.

use super::confirm::tracker;
use super::node::{connect_lane, ChainNode, Chains};
use super::submit::submitter;
use super::TaskEnd;
use crate::config::{Config, LaneConfig};
use crate::notify::Notifier;
use crate::settings::LiveSettings;
use alamo_coins::{merge, Chain, NodeHealth, RpcClient, TemplateSource, ZmqStatus, ZmqSubscriber};
use alamo_core::work::MergedWork;
use alamo_core::work::WorkTemplate;
use alamo_store::Store;
use alamo_stratum::server::Bound;
use alamo_stratum::{BlockCandidate, PoolEvent, StratumServer, VardiffConfig, WorkReceiver};
use anyhow::Context;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

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

/// Everything one lane's task tree needs.
pub(super) struct LaneTask {
    /// Position in [`Config::lanes`], which is also the publisher's index for it.
    pub(super) index: usize,
    pub(super) lane: LaneConfig,
    pub(super) config: Arc<Config>,
    pub(super) settings: Arc<LiveSettings>,
    pub(super) store: Store,
    pub(super) spool: PathBuf,
    pub(super) events: mpsc::Sender<PoolEvent>,
    pub(super) notices: mpsc::Sender<LaneNotice>,
    /// Where `block_found` notifications go.
    pub(super) notifier: Notifier,
    pub(super) shutdown: CancellationToken,
}

/// What a lane tells the publisher about itself.
pub(super) enum LaneNotice {
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
pub(super) struct LaneUp {
    pub(super) lane: usize,
    pub(super) chain: Chain,
    pub(super) work: WorkReceiver,
    /// Node health, parent first, in the order the snapshot lists coins.
    pub(super) nodes: Vec<(&'static str, watch::Receiver<NodeHealth>)>,
    /// The lane's vardiff as currently in force, so the snapshot follows Settings changes.
    pub(super) vardiff: watch::Receiver<VardiffConfig>,
}

/// A lane that has connected and bound its listener, before any of its tasks run.
struct StartedLane {
    chains: Chains,
    bound: Bound,
    work_tx: watch::Sender<Option<Arc<MergedWork>>>,
    work_rx: WorkReceiver,
    blocks_rx: mpsc::Receiver<BlockCandidate>,
    vardiff: watch::Receiver<VardiffConfig>,
}

/// Connect one lane, then run its template sources, merger, stratum server, submitter,
/// and trackers until shutdown or until one of them stops. A failure while starting is
/// this lane's alone (`TaskEnd::LaneFailed`); a failure once running is the daemon's.
pub(super) async fn run_lane(task: LaneTask) -> anyhow::Result<TaskEnd> {
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
        let vardiff_rx = vardiff.clone();
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
        anyhow::Ok((bound, work_tx, work_rx, blocks_rx, vardiff_rx))
    }
    .await;
    let (bound, work_tx, work_rx, blocks_rx, vardiff) = attached?;
    Ok(Some(StartedLane {
        chains,
        bound,
        work_tx,
        work_rx,
        blocks_rx,
        vardiff,
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
        notifier,
        shutdown,
        ..
    } = task;
    let StartedLane {
        chains,
        bound,
        work_tx,
        work_rx,
        blocks_rx,
        vardiff,
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
        notifier,
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
            vardiff,
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
