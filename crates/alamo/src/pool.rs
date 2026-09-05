//! The pool runtime: node connections, template source, stratum server, block submission,
//! confirmation tracking, and the status snapshot for the dashboard.

use crate::config::Config;
use crate::stats::Stats;
use alamo_coins::{Chain, Coin, RpcClient, TemplateSource};
use alamo_core::odds::OddsSummary;
use alamo_core::payout::PayoutTable;
use alamo_core::time::now_unix;
use alamo_store::{BlockRow, BlockStatus, NewBlock, Store};
use alamo_stratum::{BlockCandidate, PoolEvent, StratumServer, WorkReceiver};
use alamo_web::{AppState, CoinStatus, PoolSnapshot};
use anyhow::{bail, Context};
use std::sync::Arc;
use std::time::{Duration, Instant};
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

/// A parent chain the pool mines.
pub struct ParentChain {
    /// Config key (`ltc`).
    pub key: String,
    /// Coin definition.
    pub coin: Arc<dyn Coin>,
    /// Node client.
    pub rpc: RpcClient,
    /// Which network the node is on.
    pub chain: Chain,
    /// Username resolver.
    pub payouts: Arc<PayoutTable>,
}

/// Connect to the configured parent chain and verify it.
pub async fn connect_parent(config: &Config) -> anyhow::Result<ParentChain> {
    let parents: Vec<(&String, &alamo_coins::CoinConfig)> = config
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
    for (aux, c) in config
        .coins
        .iter()
        .filter(|(_, c)| c.enabled && c.merge_mined_with.is_some())
    {
        tracing::warn!(coin = %aux, parent = ?c.merge_mined_with, "merge mining arrives in Wave 2; skipping");
    }
    let coin = alamo_coins::builtin(key).context("unknown coin")?;
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
    Ok(ParentChain {
        key: key.clone(),
        coin,
        rpc,
        chain,
        payouts: Arc::new(payouts),
    })
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

/// Re-check accepted blocks against the chain and update their status.
pub async fn track_confirmations(
    rpc: &RpcClient,
    coin: &dyn Coin,
    store: &Store,
) -> anyhow::Result<()> {
    for block in store.unsettled_blocks().await? {
        let info = match rpc.get_block_info(&block.hash).await {
            Ok(info) => info,
            Err(err) => {
                tracing::warn!(hash = %block.hash, %err, "could not check block");
                continue;
            }
        };
        let status = if info.confirmations < 0 {
            tracing::warn!(height = block.height, hash = %block.hash, "block orphaned");
            BlockStatus::Orphaned
        } else if info.confirmations >= coin.coinbase_maturity() {
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

/// Run the pool until `shutdown` is cancelled or a component stops.
pub async fn run(
    config: Config,
    store: Store,
    state: AppState,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let parent = connect_parent(&config).await?;
    let cfg = &config.coins[&parent.key];
    let tag = cfg
        .coinbase_tag
        .clone()
        .unwrap_or_else(|| "/alamo/".into())
        .into_bytes();

    let (work_tx, work_rx) = watch::channel(None);
    let (events_tx, events_rx) = mpsc::channel::<PoolEvent>(4096);
    let (blocks_tx, blocks_rx) = mpsc::channel::<BlockCandidate>(64);

    // Every task returns its name when it stops; none of them stops on its own.
    let mut tasks: JoinSet<&'static str> = JoinSet::new();

    let source = TemplateSource {
        rpc: parent.rpc.clone(),
        coin: parent.coin.clone(),
        coinbase_tag: tag,
        poll_interval: Duration::from_millis(cfg.poll_interval_ms),
        refresh_interval: Duration::from_secs(cfg.template_refresh_secs),
    };
    let token = shutdown.child_token();
    tasks.spawn(async move {
        source.run(work_tx, token).await;
        "template source"
    });

    let bound = StratumServer {
        config: config.stratum.clone(),
        work: work_rx.clone(),
        payouts: parent.payouts.clone(),
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

    tasks.spawn(submitter(
        parent.rpc.clone(),
        store.clone(),
        blocks_rx,
        shutdown.child_token(),
    ));
    tasks.spawn(tracker(
        parent.rpc.clone(),
        parent.coin.clone(),
        store.clone(),
        shutdown.child_token(),
    ));
    tasks.spawn(publisher(
        events_rx,
        work_rx,
        store.clone(),
        state,
        parent.chain,
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
    rpc: RpcClient,
    store: Store,
    mut blocks: mpsc::Receiver<BlockCandidate>,
    shutdown: CancellationToken,
) -> &'static str {
    loop {
        tokio::select! {
            next = blocks.recv() => {
                let Some(candidate) = next else { return "submitter" };
                if let Err(err) = submit_candidate(&rpc, &store, &candidate).await {
                    tracing::error!(%err, "block submission failed");
                }
            }
            _ = shutdown.cancelled() => return "submitter",
        }
    }
}

async fn tracker(
    rpc: RpcClient,
    coin: Arc<dyn Coin>,
    store: Store,
    shutdown: CancellationToken,
) -> &'static str {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(err) = track_confirmations(&rpc, coin.as_ref(), &store).await {
                    tracing::warn!(%err, "confirmation tracking failed");
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
    let mut stats = Stats::default();
    let mut blocks: Vec<BlockRow> = Vec::new();
    let mut ticks: u32 = 0;
    let mut interval = tokio::time::interval(PUBLISH_INTERVAL);
    loop {
        tokio::select! {
            next = events.recv() => {
                let Some(event) = next else { return "publisher" };
                stats.apply(event, Instant::now());
            }
            _ = interval.tick() => {
                if ticks % BLOCKS_REFRESH_TICKS == 0 {
                    match store.recent_blocks(25).await {
                        Ok(rows) => blocks = rows,
                        Err(err) => tracing::warn!(%err, "could not load recent blocks"),
                    }
                }
                ticks = ticks.wrapping_add(1);
                state.publish(build_snapshot(&stats, &work, blocks.clone(), chain));
            }
            _ = shutdown.cancelled() => return "publisher",
        }
    }
}

fn build_snapshot(
    stats: &Stats,
    work: &WorkReceiver,
    blocks: Vec<BlockRow>,
    chain: Chain,
) -> PoolSnapshot {
    let now = Instant::now();
    let workers = stats.workers(now);
    let hashrate = workers.iter().map(|w| w.hashrate).fold(0.0, |a, b| a + b);
    let current = work.borrow().clone();
    let coins = current
        .iter()
        .map(|w| CoinStatus {
            symbol: w.coin.to_string(),
            chain: chain.to_string(),
            height: w.height,
            network_difficulty: w.network_difficulty(),
            template_age_seconds: now_unix().saturating_sub(w.created_at),
            coinbase_value: w.coinbase_value,
        })
        .collect();
    let odds = current
        .as_ref()
        .map(|w| OddsSummary::compute(hashrate, w.network_difficulty()));
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
