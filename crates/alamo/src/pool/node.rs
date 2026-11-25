//! Node connections: one lane's parent chain and the aux chains merge-mined with it.

use crate::config::{Config, LaneConfig};
use alamo_coins::{Chain, Coin, CoinConfig, RpcClient};
use alamo_core::payout::{AuxPayoutTable, PayoutSet, PayoutTable};
use anyhow::{bail, Context};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

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
