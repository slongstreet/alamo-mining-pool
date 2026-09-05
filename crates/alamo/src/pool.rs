//! The pool runtime: node connections, template source, stratum server, block submission,
//! confirmation tracking, and the status snapshot for the dashboard.

use crate::config::Config;
use crate::stats::Stats;
use alamo_coins::{Chain, Coin, CoinPayouts, RpcClient, TemplateSource};
use alamo_core::odds::OddsSummary;
use alamo_store::{NewBlock, Store};
use alamo_stratum::{BlockCandidate, PoolEvent, StratumServer, WorkReceiver};
use alamo_web::{AppState, BlockStatus, CoinStatus, PoolSnapshot};
use anyhow::{bail, Context};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// Confirmations after which a block is considered final (coinbase maturity).
const CONFIRMATIONS_FINAL: i64 = 100;

/// Result of handing a block to the node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmitOutcome {
    /// The node accepted the block.
    Accepted,
    /// The node rejected it with the given reason.
    Rejected(String),
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
    pub payouts: Arc<CoinPayouts>,
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
    tracing::info!(coin = coin.symbol(), chain = %info.chain, height = info.blocks, "node connected");
    let payouts = CoinPayouts::new(coin.address_params(chain), &cfg.fallback_address)
        .with_context(|| {
            format!(
                "coins.{key}.fallback_address is not a valid {} address for {:?}",
                coin.symbol(),
                chain
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
            coin = %candidate.coin, height = candidate.height, hash = %candidate.block_hash,
            worker = %candidate.worker, address = %candidate.address, "block accepted by node"
        ),
        SubmitOutcome::Rejected(reason) => tracing::error!(
            coin = %candidate.coin, height = candidate.height, hash = %candidate.block_hash,
            %reason, "block REJECTED by node"
        ),
    }
    store
        .insert_block(&NewBlock {
            coin: candidate.coin.clone(),
            height: candidate.height,
            hash: candidate.block_hash.clone(),
            worker: candidate.worker.clone(),
            difficulty: candidate.network_difficulty,
            share_diff: candidate.share_difficulty,
            reward_sats: None,
            found_at: candidate.found_at,
            status: match &outcome {
                SubmitOutcome::Accepted => "accepted".into(),
                SubmitOutcome::Rejected(_) => "rejected".into(),
            },
        })
        .await
        .context("recording block")?;
    Ok(outcome)
}

/// Re-check accepted blocks against the chain and update their status.
pub async fn track_confirmations(rpc: &RpcClient, store: &Store) -> anyhow::Result<()> {
    for block in store.unsettled_blocks().await? {
        match rpc.get_block_info(&block.hash).await {
            Ok(info) if info.confirmations < 0 => {
                tracing::warn!(height = block.height, hash = %block.hash, "block orphaned");
                store
                    .set_block_status(block.id, "orphaned", info.confirmations)
                    .await?;
            }
            Ok(info) if info.confirmations >= CONFIRMATIONS_FINAL => {
                store
                    .set_block_status(block.id, "confirmed", info.confirmations)
                    .await?;
            }
            Ok(info) => {
                store
                    .set_block_status(block.id, "accepted", info.confirmations)
                    .await?
            }
            Err(err) => tracing::warn!(hash = %block.hash, %err, "could not check block"),
        }
    }
    Ok(())
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Run the pool until `shutdown` is cancelled or a component fails.
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

    let mut tasks = JoinSet::new();

    let source = TemplateSource {
        rpc: parent.rpc.clone(),
        coin: parent.coin.clone(),
        coinbase_tag: tag,
        poll_interval: Duration::from_millis(500),
        refresh_interval: Duration::from_secs(30),
    };
    tasks.spawn(
        source
            .run(work_tx, shutdown.child_token())
            .then_ok("template source"),
    );

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
    tasks.spawn(bound.run(shutdown.child_token()).then_ok("stratum"));

    tasks.spawn(submitter(
        parent.rpc.clone(),
        store.clone(),
        blocks_rx,
        shutdown.child_token(),
    ));
    tasks.spawn(tracker(
        parent.rpc.clone(),
        store.clone(),
        shutdown.child_token(),
    ));
    tasks.spawn(publisher(
        events_rx,
        work_rx,
        store.clone(),
        state,
        parent.coin.symbol().to_string(),
        parent.chain,
        shutdown.child_token(),
    ));

    let result = tokio::select! {
        _ = shutdown.cancelled() => Ok(()),
        finished = tasks.join_next() => match finished {
            Some(Ok(Ok(name))) => bail!("{name} stopped unexpectedly"),
            Some(Ok(Err(err))) => Err(err),
            Some(Err(join)) => Err(anyhow::Error::new(join).context("pool task panicked")),
            None => Ok(()),
        },
    };
    shutdown.cancel();
    while tasks.join_next().await.is_some() {}
    result
}

trait ThenOk: Sized {
    fn then_ok(
        self,
        name: &'static str,
    ) -> impl std::future::Future<Output = anyhow::Result<&'static str>>;
}

impl<F: std::future::Future<Output = ()>> ThenOk for F {
    async fn then_ok(self, name: &'static str) -> anyhow::Result<&'static str> {
        self.await;
        Ok(name)
    }
}

async fn submitter(
    rpc: RpcClient,
    store: Store,
    mut blocks: mpsc::Receiver<BlockCandidate>,
    shutdown: CancellationToken,
) -> anyhow::Result<&'static str> {
    loop {
        tokio::select! {
            next = blocks.recv() => {
                let Some(candidate) = next else { return Ok("submitter") };
                if let Err(err) = submit_candidate(&rpc, &store, &candidate).await {
                    tracing::error!(%err, "block submission failed");
                }
            }
            _ = shutdown.cancelled() => return Ok("submitter"),
        }
    }
}

async fn tracker(
    rpc: RpcClient,
    store: Store,
    shutdown: CancellationToken,
) -> anyhow::Result<&'static str> {
    let mut interval = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(err) = track_confirmations(&rpc, &store).await {
                    tracing::warn!(%err, "confirmation tracking failed");
                }
            }
            _ = shutdown.cancelled() => return Ok("tracker"),
        }
    }
}

async fn publisher(
    mut events: mpsc::Receiver<PoolEvent>,
    work: WorkReceiver,
    store: Store,
    state: AppState,
    symbol: String,
    chain: Chain,
    shutdown: CancellationToken,
) -> anyhow::Result<&'static str> {
    let mut stats = Stats::default();
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    loop {
        tokio::select! {
            next = events.recv() => {
                let Some(event) = next else { return Ok("publisher") };
                stats.apply(event, Instant::now());
            }
            _ = interval.tick() => {
                let snapshot = build_snapshot(&stats, &work, &store, &symbol, chain).await;
                state.publish(snapshot);
            }
            _ = shutdown.cancelled() => return Ok("publisher"),
        }
    }
}

async fn build_snapshot(
    stats: &Stats,
    work: &WorkReceiver,
    store: &Store,
    symbol: &str,
    chain: Chain,
) -> PoolSnapshot {
    let now = Instant::now();
    let hashrate = stats.pool_hashrate(now);
    let current = work.borrow().clone();
    let coins = current
        .as_ref()
        .map(|w| {
            vec![CoinStatus {
                symbol: symbol.to_string(),
                chain: format!("{chain:?}").to_lowercase(),
                height: w.height,
                network_difficulty: w.network_difficulty(),
                template_age_seconds: now_unix().saturating_sub(w.created_at),
                coinbase_value: w.coinbase_value,
            }]
        })
        .unwrap_or_default();
    let odds = current
        .as_ref()
        .map(|w| OddsSummary::compute(hashrate, w.network_difficulty()));
    let blocks = match store.recent_blocks(25).await {
        Ok(rows) => rows
            .into_iter()
            .map(|b| BlockStatus {
                coin: b.coin,
                height: b.height,
                hash: b.hash,
                worker: b.worker,
                found_at: b.found_at,
                status: b.status,
                confirmations: b.confirmations,
                reward_sats: b.reward_sats,
            })
            .collect(),
        Err(err) => {
            tracing::warn!(%err, "could not load recent blocks");
            Vec::new()
        }
    };
    PoolSnapshot {
        coins,
        hashrate,
        shares_accepted: stats.shares_accepted(),
        shares_rejected: stats.shares_rejected(),
        workers: stats.workers(now),
        blocks,
        odds,
        ..Default::default()
    }
}
