//! The pool runtime: node connections, template sources, stratum server, block submission,
//! confirmation tracking, and the status snapshot for the dashboard.

use crate::config::Config;
use crate::persist::Persistence;
use crate::stats::Stats;
use alamo_coins::{merge, Chain, Coin, CoinConfig, RpcClient, TemplateSource};
use alamo_core::odds::OddsSummary;
use alamo_core::payout::{AuxPayoutTable, PayoutSet, PayoutTable};
use alamo_core::time::now_unix;
use alamo_core::work::WorkTemplate;
use alamo_store::{BlockRow, BlockStatus, NewBlock, Store};
use alamo_stratum::{BlockCandidate, PoolEvent, StratumServer, WorkReceiver};
use alamo_web::{AppState, CoinStatus, PoolSnapshot};
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

async fn connect_node(key: &str, cfg: &CoinConfig) -> anyhow::Result<ChainNode> {
    let coin = alamo_coins::builtin(key).with_context(|| format!("unknown coin '{key}'"))?;
    let rpc = RpcClient::new(&cfg.rpc_url, &cfg.rpc_user, &cfg.rpc_password);
    let info = rpc
        .get_blockchain_info()
        .await
        .with_context(|| format!("connecting to {} node at {}", coin.name(), cfg.rpc_url))?;
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
    Ok(ChainNode {
        key: key.to_string(),
        coin,
        rpc,
        chain,
        payouts,
        config: cfg.clone(),
    })
}

/// Connect to the configured parent chain and every aux chain merge-mined with it.
pub async fn connect_chains(config: &Config) -> anyhow::Result<Chains> {
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
    let parent = connect_node(key, cfg).await?;

    let mut aux = Vec::new();
    for (aux_key, aux_cfg) in config
        .coins
        .iter()
        .filter(|(_, c)| c.enabled && c.merge_mined_with.as_deref() == Some(key.as_str()))
    {
        let node = connect_node(aux_key, aux_cfg).await?;
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
    Ok(Chains { parent, aux })
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
            reward_sats: None,
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

fn template_source(node: &ChainNode) -> TemplateSource {
    let tag = node
        .config
        .coinbase_tag
        .clone()
        .unwrap_or_else(|| "/alamo/".into())
        .into_bytes();
    TemplateSource {
        rpc: node.rpc.clone(),
        coin: node.coin.clone(),
        coinbase_tag: tag,
        poll_interval: Duration::from_millis(node.config.poll_interval_ms),
        refresh_interval: Duration::from_secs(node.config.template_refresh_secs),
    }
}

/// Run the pool until `shutdown` is cancelled or a component stops.
pub async fn run(
    config: Config,
    store: Store,
    state: AppState,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let chains = connect_chains(&config).await?;

    let (work_tx, work_rx) = watch::channel(None);
    let (events_tx, events_rx) = mpsc::channel::<PoolEvent>(4096);
    let (blocks_tx, blocks_rx) = mpsc::channel::<BlockCandidate>(64);

    // Every task returns its name when it stops; none of them stops on its own.
    let mut tasks: JoinSet<&'static str> = JoinSet::new();

    let (parent_tx, parent_rx) = watch::channel::<Option<Arc<WorkTemplate>>>(None);
    let source = template_source(&chains.parent);
    let token = shutdown.child_token();
    tasks.spawn(async move {
        source.run(parent_tx, token).await;
        "template source"
    });
    let mut aux_rxs = Vec::with_capacity(chains.aux.len());
    for node in &chains.aux {
        let (aux_tx, aux_rx) = watch::channel::<Option<Arc<WorkTemplate>>>(None);
        let source = template_source(node);
        let token = shutdown.child_token();
        tasks.spawn(async move {
            source.run(aux_tx, token).await;
            "aux template source"
        });
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
                        Err(err) => tracing::warn!(%err, "could not load recent blocks"),
                    }
                }
                ticks = ticks.wrapping_add(1);
                state.publish(build_snapshot(&stats, &work, blocks.clone(), chain));
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

fn coin_status(w: &WorkTemplate, chain: Chain) -> CoinStatus {
    CoinStatus {
        symbol: w.coin.to_string(),
        chain: chain.to_string(),
        height: w.height,
        network_difficulty: w.network_difficulty(),
        template_age_seconds: now_unix().saturating_sub(w.created_at),
        coinbase_value: w.coinbase_value,
    }
}

fn build_snapshot(
    stats: &Stats,
    work: &WorkReceiver,
    blocks: Vec<BlockRow>,
    chain: Chain,
) -> PoolSnapshot {
    let now = now_unix();
    let workers = stats.workers(now);
    let hashrate = workers.iter().map(|w| w.hashrate).fold(0.0, |a, b| a + b);
    let current = work.borrow().clone();
    let coins = current
        .iter()
        .flat_map(|m| std::iter::once(&m.parent).chain(m.aux.iter()))
        .map(|w| coin_status(w, chain))
        .collect();
    let odds = current
        .as_ref()
        .map(|m| OddsSummary::compute(hashrate, m.parent.network_difficulty()));
    PoolSnapshot {
        coins,
        hashrate,
        shares_accepted: stats.shares_accepted(),
        shares_rejected: stats.shares_rejected(),
        workers,
        blocks,
        odds,
        ..Default::default()
    }
}
