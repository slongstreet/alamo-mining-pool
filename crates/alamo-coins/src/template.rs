//! Fetch block templates from a node and turn them into [`WorkTemplate`]s.

use crate::rpc::{RpcClient, RpcError};
use crate::zmq::ZmqStatus;
use crate::Coin;
use alamo_core::encode::{push_data, push_script_num};
use alamo_core::hash::from_display_hex;
use alamo_core::job::JobId;
use alamo_core::time::now_unix;
use alamo_core::work::{TemplateTx, WorkTemplate};
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// The fields of `getblocktemplate` the pool uses.
#[derive(Debug, Deserialize)]
pub struct RawTemplate {
    /// Block version including version bits.
    pub version: i32,
    /// Previous block hash, display order.
    pub previousblockhash: String,
    /// Transactions to include, in order.
    #[serde(default)]
    pub transactions: Vec<RawTx>,
    /// Coinbase reward including fees.
    pub coinbasevalue: u64,
    /// Compact target as hex.
    pub bits: String,
    /// Node's current time.
    pub curtime: u32,
    /// Earliest allowed header time.
    pub mintime: u32,
    /// Height of the new block.
    pub height: u64,
    /// Witness commitment scriptPubKey, present when segwit is active.
    #[serde(default)]
    pub default_witness_commitment: Option<String>,
    /// Litecoin MWEB extension block, appended to the serialized block.
    #[serde(default)]
    pub mweb: Option<String>,
}

/// A transaction entry from the template.
#[derive(Debug, Deserialize)]
pub struct RawTx {
    /// Raw transaction hex.
    pub data: String,
    /// Transaction id, display order.
    pub txid: String,
}

/// Why a template could not be converted.
#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    /// A hex field was malformed.
    #[error("bad hex in template field {0}")]
    Hex(&'static str),
    /// The node call failed.
    #[error(transparent)]
    Rpc(#[from] RpcError),
}

/// Convert a raw template to work. `id` and `clean_jobs` are supplied by the source.
pub fn convert(
    raw: RawTemplate,
    coin: &dyn Coin,
    coinbase_tag: &[u8],
    id: JobId,
    clean_jobs: bool,
) -> Result<WorkTemplate, TemplateError> {
    let prev_hash = from_display_hex(&raw.previousblockhash)
        .map_err(|_| TemplateError::Hex("previousblockhash"))?;
    let bits = u32::from_str_radix(&raw.bits, 16).map_err(|_| TemplateError::Hex("bits"))?;
    let extra_payload = coin.extra_block_payload(&raw)?;
    let mut transactions = Vec::with_capacity(raw.transactions.len());
    for tx in raw.transactions {
        transactions.push(TemplateTx {
            data: hex::decode(&tx.data).map_err(|_| TemplateError::Hex("transactions.data"))?,
            txid: from_display_hex(&tx.txid)
                .map_err(|_| TemplateError::Hex("transactions.txid"))?,
        });
    }
    let witness_commitment = match raw.default_witness_commitment {
        Some(hex_str) => Some(
            hex::decode(hex_str).map_err(|_| TemplateError::Hex("default_witness_commitment"))?,
        ),
        None => None,
    };
    let mut prefix = Vec::with_capacity(32);
    push_script_num(&mut prefix, raw.height as i64);
    if !coinbase_tag.is_empty() {
        push_data(&mut prefix, coinbase_tag);
    }

    let mut work = WorkTemplate {
        id,
        coin: coin.symbol(),
        algorithm: coin.algorithm(),
        height: raw.height,
        version: coin.block_version(raw.version),
        prev_hash,
        bits,
        cur_time: raw.curtime,
        min_time: raw.mintime,
        coinbase_value: raw.coinbasevalue,
        coinbase_script_prefix: prefix,
        witness_commitment,
        transactions,
        merkle_branch: Vec::new(),
        extra_payload,
        clean_jobs,
        created_at: now_unix(),
    };
    work.compute_merkle_branch();
    Ok(work)
}

/// What the template source knows about its node, for the dashboard and metrics.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeHealth {
    /// Whether the most recent RPC call succeeded.
    pub connected: bool,
    /// Consecutive failed polls.
    pub failures: u32,
    /// The most recent RPC failure, if the node is currently unreachable.
    pub last_error: Option<String>,
    /// Unix time of the last successful poll.
    pub last_ok: Option<u64>,
    /// Whether the published template was withdrawn because the node stayed unreachable
    /// past `template_stale_secs`.
    pub withdrawn: bool,
    /// ZMQ subscription state: `None` when not configured.
    pub zmq: Option<bool>,
}

/// Polls a node for templates and publishes them on a watch channel.
pub struct TemplateSource {
    /// Node client.
    pub rpc: RpcClient,
    /// The coin being mined.
    pub coin: Arc<dyn Coin>,
    /// Bytes for the coinbase scriptSig tag.
    pub coinbase_tag: Vec<u8>,
    /// How often to check the chain tip.
    pub poll_interval: Duration,
    /// How often to refresh the template even without a new tip (picks up new fees).
    pub refresh_interval: Duration,
    /// How long the node may stay unreachable before the template is withdrawn.
    pub stale_after: Duration,
    /// Block notifications from a [`ZmqSubscriber`](crate::ZmqSubscriber), if configured.
    /// Each notification triggers an immediate poll; polling continues as the fallback,
    /// just less often while the subscription is up.
    pub zmq: Option<watch::Receiver<ZmqStatus>>,
    /// Where node health is reported, if anyone is listening.
    pub health: Option<watch::Sender<NodeHealth>>,
}

/// Tip polling cadence while ZMQ notifications are flowing.
const ZMQ_FALLBACK_POLL: Duration = Duration::from_secs(10);
/// Minimum delay between polls while the node is unreachable.
const FAILURE_POLL: Duration = Duration::from_secs(2);

impl TemplateSource {
    /// A source with default timings, no notifications, and no health reporting.
    pub fn new(rpc: RpcClient, coin: Arc<dyn Coin>, coinbase_tag: Vec<u8>) -> Self {
        Self {
            rpc,
            coin,
            coinbase_tag,
            poll_interval: Duration::from_millis(500),
            refresh_interval: Duration::from_secs(30),
            stale_after: Duration::from_secs(120),
            zmq: None,
            health: None,
        }
    }

    /// Run until cancelled, publishing each new template to `tx`.
    pub async fn run(
        self,
        tx: watch::Sender<Option<Arc<WorkTemplate>>>,
        shutdown: CancellationToken,
    ) {
        shutdown.run_until_cancelled(self.poll_loop(tx)).await;
    }

    async fn poll_loop(mut self, tx: watch::Sender<Option<Arc<WorkTemplate>>>) {
        let mut next_id: u64 = 1;
        let mut last_tip: Option<String> = None;
        let mut last_refresh = Instant::now() - self.refresh_interval;
        let mut last_ok: Option<Instant> = None;
        let mut health = NodeHealth::default();
        let mut zmq_blocks = self
            .zmq
            .as_mut()
            .map_or(0, |rx| rx.borrow_and_update().blocks);
        loop {
            match self.fetch(&last_tip, last_refresh, JobId(next_id)).await {
                Ok(fetched) => {
                    if health.failures > 0 {
                        tracing::info!(coin = self.coin.symbol(), "node reachable again");
                    }
                    last_ok = Some(Instant::now());
                    health.connected = true;
                    health.failures = 0;
                    health.last_error = None;
                    health.last_ok = Some(now_unix());
                    if let Some((tip, work)) = fetched {
                        next_id += 1;
                        last_refresh = Instant::now();
                        tracing::info!(
                            coin = work.coin,
                            height = work.height,
                            txs = work.transactions.len(),
                            clean = work.clean_jobs,
                            difficulty = work.network_difficulty(),
                            "new work"
                        );
                        last_tip = Some(tip);
                        health.withdrawn = false;
                        tx.send_replace(Some(Arc::new(work)));
                    }
                }
                Err(err) => {
                    health.connected = false;
                    health.failures += 1;
                    if health.failures == 1 || health.failures % 30 == 0 {
                        tracing::warn!(coin = self.coin.symbol(), %err, failures = health.failures, "template fetch failed");
                    }
                    health.last_error = Some(err.to_string());
                    let unreachable_for = last_ok.map_or(Duration::MAX, |t| t.elapsed());
                    if !health.withdrawn
                        && last_tip.is_some()
                        && unreachable_for >= self.stale_after
                    {
                        tracing::warn!(
                            coin = self.coin.symbol(),
                            unreachable_secs = unreachable_for.as_secs(),
                            "node unreachable too long; withdrawing template"
                        );
                        health.withdrawn = true;
                        // Force a full refetch once the node is back, whatever the tip.
                        last_refresh = Instant::now() - self.refresh_interval;
                        tx.send_replace(None);
                    }
                }
            }
            let zmq_live = self.zmq.as_ref().is_some_and(|rx| rx.borrow().connected);
            health.zmq = self.zmq.as_ref().map(|_| zmq_live);
            if let Some(report) = &self.health {
                report.send_if_modified(|current| {
                    if *current == health {
                        false
                    } else {
                        *current = health.clone();
                        true
                    }
                });
            }

            let delay = if health.failures > 0 {
                self.poll_interval.max(FAILURE_POLL)
            } else if zmq_live {
                self.poll_interval.max(ZMQ_FALLBACK_POLL)
            } else {
                self.poll_interval
            };
            self.wait(delay, &mut zmq_blocks).await;
        }
    }

    /// Sleep for `delay`, or return early when ZMQ reports a new block.
    async fn wait(&mut self, delay: Duration, zmq_blocks: &mut u64) {
        let sleep = tokio::time::sleep(delay);
        tokio::pin!(sleep);
        loop {
            let Some(rx) = self.zmq.as_mut() else {
                sleep.await;
                return;
            };
            tokio::select! {
                _ = &mut sleep => return,
                changed = rx.changed() => {
                    if changed.is_err() {
                        self.zmq = None;
                        continue;
                    }
                    let blocks = rx.borrow_and_update().blocks;
                    if blocks != *zmq_blocks {
                        *zmq_blocks = blocks;
                        tracing::debug!(coin = self.coin.symbol(), "block notification; polling now");
                        return;
                    }
                    // Connection state changed but no new block: keep waiting.
                }
            }
        }
    }

    /// Fetch a template if the tip moved or the refresh interval elapsed.
    async fn fetch(
        &self,
        last_tip: &Option<String>,
        last_refresh: Instant,
        id: JobId,
    ) -> Result<Option<(String, WorkTemplate)>, TemplateError> {
        let tip = self.rpc.get_best_block_hash().await?;
        let tip_changed = last_tip.as_deref() != Some(tip.as_str());
        if !tip_changed && last_refresh.elapsed() < self.refresh_interval {
            return Ok(None);
        }
        let raw = self
            .rpc
            .get_block_template(self.coin.template_rules())
            .await?;
        let work = convert(raw, self.coin.as_ref(), &self.coinbase_tag, id, tip_changed)?;
        Ok(Some((tip, work)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Dogecoin, Litecoin};
    use serde_json::json;

    #[test]
    fn converts_regtest_template() {
        let raw: RawTemplate = serde_json::from_value(json!({
            "version": 536870912,
            "previousblockhash": "530827f38f93b43ed12af0b3ad25a288dc02ed74d6d7857862df51fc56c416f9",
            "transactions": [],
            "coinbasevalue": 5000000000u64,
            "target": "7fffff0000000000000000000000000000000000000000000000000000000000",
            "mintime": 1296688603,
            "curtime": 1788619193,
            "bits": "207fffff",
            "height": 1,
            "default_witness_commitment": "6a24aa21a9ede2f61c3f71d1defd3fa999dfa36953755c690689799962b48bebd836974e8cf9"
        }))
        .unwrap();
        let work = convert(raw, &Litecoin, b"/alamo/", JobId(3), true).unwrap();
        assert_eq!(work.height, 1);
        assert_eq!(work.bits, 0x207f_ffff);
        assert_eq!(work.prev_hash[31], 0x53);
        assert_eq!(
            work.coinbase_script_prefix,
            [0x51, 0x07, b'/', b'a', b'l', b'a', b'm', b'o', b'/']
        );
        assert_eq!(work.witness_commitment.as_ref().unwrap().len(), 38);
        assert!(work.merkle_branch.is_empty());
        assert!(work.clean_jobs);
        assert!(work.network_difficulty() < 1e-9);
        assert!(work.extra_payload.is_empty());
    }

    #[test]
    fn dogecoin_template_gets_the_auxpow_version() {
        // From a Dogecoin 1.14.9 regtest node at height 31.
        let raw: RawTemplate = serde_json::from_value(json!({
            "version": 6422532,
            "previousblockhash": "dce5787e8f1146cdfb2335a116ac70451a5fe21b5f8a93dd849feb64c9777ab1",
            "transactions": [],
            "coinbasevalue": 50000000000000u64,
            "mintime": 1788621996,
            "curtime": 1788621996,
            "bits": "207fffff",
            "height": 31
        }))
        .unwrap();
        let work = convert(raw, &Dogecoin, b"/alamo/", JobId(1), true).unwrap();
        assert_eq!(work.version, 0x0062_0104);
        assert_eq!(work.coin, "DOGE");
        assert!(work.witness_commitment.is_none());
        assert_eq!(work.coinbase_script_prefix[..2], [0x01, 31]);
    }

    #[test]
    fn mweb_payload_gets_presence_byte_and_hogex_is_last() {
        // Trimmed from a real regtest template at height 433 (MWEB active).
        let raw: RawTemplate = serde_json::from_value(json!({
            "version": 536870912,
            "previousblockhash": "03585da87c1466f2d908af1540d98b9cea819bc4981b93b4b72d42ce4e28070b",
            "transactions": [{
                "txid": "364869fb48886345d7325b81eadeb59400ec74f5eda7f4759d0d96b6d73fef36",
                "data": "0200000000080132acafd2229828b6658b62e18773d6de67928ebf3d660928280cb6685081e0520000000000ffffffff01e666814a00000000225820e14c995207dce8ee7d1834d922c933b4888f4f83a896db0b485bfc936e73bb140000000000"
            }],
            "coinbasevalue": 1250000000u64,
            "mintime": 1788620172,
            "curtime": 1788620172,
            "bits": "207fffff",
            "height": 433,
            "default_witness_commitment": "6a24aa21a9ed00f63fed7466940d4a9d7c492aa8b05f09257f752e91b8eb8350cd66e097a253",
            "mweb": "8231b9e092c9e87dec44e73c19fa09c7a03e250e1354a885229aea60ab8de976c9ff"
        }))
        .unwrap();
        let work = convert(raw, &Litecoin, b"", JobId(1), false).unwrap();
        assert_eq!(work.extra_payload[0], 0x01);
        assert_eq!(&work.extra_payload[1..3], &[0x82, 0x31]);
        assert_eq!(work.transactions.len(), 1);
        assert_eq!(work.merkle_branch.len(), 1);
        assert_eq!(work.merkle_branch[0], work.transactions[0].txid);
        assert_eq!(work.coinbase_script_prefix, [0x02, 0xb1, 0x01]);
    }
}
