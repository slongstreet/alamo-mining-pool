//! Fetch block templates from a node and turn them into [`WorkTemplate`]s.

use crate::rpc::{RpcClient, RpcError};
use crate::zmq::ZmqStatus;
use crate::Coin;
use alamo_core::encode::{push_data, push_script_num};
use alamo_core::hash::from_display_hex;
use alamo_core::hash::Hash256;
use alamo_core::job::JobId;
use alamo_core::time::now_unix;
use alamo_core::work::{TemplateTx, WorkTemplate};
use serde::Deserialize;
use std::collections::HashMap;
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
    /// Witness transaction id (`hash` in `getblocktemplate`), display order. It commits to
    /// every byte of `data`, so it is what [`TxCache`] keys on. Optional: a missing or
    /// malformed value only means the transaction is never reused from the cache.
    #[serde(default)]
    pub hash: Option<String>,
}

/// The decoded transactions of the previous template, keyed by wtxid, so the next template
/// can share the bytes of transactions the two have in common instead of decoding a second
/// copy. Holds at most one template's transactions.
#[derive(Debug, Default)]
pub struct TxCache {
    by_wtxid: HashMap<Hash256, Arc<[u8]>>,
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
    convert_cached(
        raw,
        coin,
        coinbase_tag,
        id,
        clean_jobs,
        &mut TxCache::default(),
    )
}

/// [`convert`], reusing the bytes of any transaction `cache` already holds by wtxid. On
/// success the cache is replaced by this template's transactions; on error it is left as
/// it was.
pub fn convert_cached(
    raw: RawTemplate,
    coin: &dyn Coin,
    coinbase_tag: &[u8],
    id: JobId,
    clean_jobs: bool,
    cache: &mut TxCache,
) -> Result<WorkTemplate, TemplateError> {
    let prev_hash = from_display_hex(&raw.previousblockhash)
        .map_err(|_| TemplateError::Hex("previousblockhash"))?;
    let bits = u32::from_str_radix(&raw.bits, 16).map_err(|_| TemplateError::Hex("bits"))?;
    let extra_payload = coin.extra_block_payload(&raw)?;
    let mut transactions = Vec::with_capacity(raw.transactions.len());
    let mut next_cache = HashMap::with_capacity(raw.transactions.len());
    for tx in raw.transactions {
        let wtxid = tx.hash.as_deref().and_then(|h| from_display_hex(h).ok());
        let cached = wtxid
            .and_then(|w| cache.by_wtxid.get(&w))
            .filter(|bytes| tx.data.len() == 2 * bytes.len());
        let data: Arc<[u8]> = match cached {
            Some(bytes) => Arc::clone(bytes),
            None => hex::decode(&tx.data)
                .map_err(|_| TemplateError::Hex("transactions.data"))?
                .into(),
        };
        if let Some(w) = wtxid {
            next_cache.insert(w, Arc::clone(&data));
        }
        transactions.push(TemplateTx {
            data,
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
    cache.by_wtxid = next_cache;
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
    /// Bytes for the coinbase scriptSig tag. A change is picked up on the next poll and
    /// produces a fresh template (a non-clean job, so shares in flight stay valid).
    pub coinbase_tag: watch::Receiver<Vec<u8>>,
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
        // A receiver keeps its value after the sender is dropped, so a fixed tag needs
        // no live channel.
        let (_, coinbase_tag) = watch::channel(coinbase_tag);
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
        let mut last_tag: Option<Vec<u8>> = None;
        let mut last_refresh = Instant::now() - self.refresh_interval;
        let mut last_ok: Option<Instant> = None;
        let mut health = NodeHealth::default();
        let mut tx_cache = TxCache::default();
        let mut zmq_blocks = self
            .zmq
            .as_mut()
            .map_or(0, |rx| rx.borrow_and_update().blocks);
        loop {
            match self
                .fetch(
                    &last_tip,
                    &last_tag,
                    last_refresh,
                    JobId(next_id),
                    &mut tx_cache,
                )
                .await
            {
                Ok(fetched) => {
                    if health.failures > 0 {
                        tracing::info!(coin = self.coin.symbol(), "node reachable again");
                    }
                    last_ok = Some(Instant::now());
                    health.connected = true;
                    health.failures = 0;
                    health.last_error = None;
                    health.last_ok = Some(now_unix());
                    if let Some((tip, tag, work)) = fetched {
                        next_id += 1;
                        last_refresh = Instant::now();
                        if last_tag.as_ref().is_some_and(|t| *t != tag) {
                            tracing::info!(
                                coin = work.coin,
                                tag = %String::from_utf8_lossy(&tag),
                                "coinbase tag changed"
                            );
                        }
                        last_tag = Some(tag);
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

    /// Sleep for `delay`, or return early when ZMQ reports a new block or the coinbase
    /// tag changes.
    async fn wait(&mut self, delay: Duration, zmq_blocks: &mut u64) {
        let sleep = tokio::time::sleep(delay);
        tokio::pin!(sleep);
        // A tag fixed at construction has no sender; then only the timer and ZMQ apply.
        let mut tag_live = !self.coinbase_tag.has_changed().is_err();
        let Self {
            coin,
            coinbase_tag,
            zmq,
            ..
        } = self;
        loop {
            tokio::select! {
                _ = &mut sleep => return,
                changed = coinbase_tag.changed(), if tag_live => {
                    if changed.is_ok() {
                        return;
                    }
                    tag_live = false;
                }
                changed = changed_or_pending(zmq.as_mut()), if zmq.is_some() => {
                    if changed.is_err() {
                        *zmq = None;
                        continue;
                    }
                    let Some(rx) = zmq.as_mut() else { continue };
                    let blocks = rx.borrow_and_update().blocks;
                    if blocks != *zmq_blocks {
                        *zmq_blocks = blocks;
                        tracing::debug!(coin = coin.symbol(), "block notification; polling now");
                        return;
                    }
                    // Connection state changed but no new block: keep waiting.
                }
            }
        }
    }

    /// Fetch a template if the tip moved, the tag changed, or the refresh interval
    /// elapsed. Returns the tip and tag the template was built for.
    async fn fetch(
        &self,
        last_tip: &Option<String>,
        last_tag: &Option<Vec<u8>>,
        last_refresh: Instant,
        id: JobId,
        tx_cache: &mut TxCache,
    ) -> Result<Option<(String, Vec<u8>, WorkTemplate)>, TemplateError> {
        let tip = self.rpc.get_best_block_hash().await?;
        let tip_changed = last_tip.as_deref() != Some(tip.as_str());
        let tag = self.coinbase_tag.borrow().clone();
        let tag_changed = last_tag.as_ref() != Some(&tag);
        if !tip_changed && !tag_changed && last_refresh.elapsed() < self.refresh_interval {
            return Ok(None);
        }
        let raw = self
            .rpc
            .get_block_template(self.coin.template_rules())
            .await?;
        let work = convert_cached(raw, self.coin.as_ref(), &tag, id, tip_changed, tx_cache)?;
        Ok(Some((tip, tag, work)))
    }
}

/// `rx.changed()`, or a future that never completes when there is no receiver. Lets a
/// `select!` branch over an optional receiver be built unconditionally; the branch's
/// precondition keeps it disabled while the receiver is absent.
async fn changed_or_pending<T>(
    rx: Option<&mut watch::Receiver<T>>,
) -> Result<(), watch::error::RecvError> {
    match rx {
        Some(rx) => rx.changed().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Bitcoin, Dogecoin, Litecoin};
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

    fn hash_of(n: u8) -> String {
        format!("{n:02x}").repeat(32)
    }

    /// A Bitcoin-shaped template whose transactions are `(data, txid, hash)` triples.
    fn raw_with(txs: &[(&str, u8, Option<u8>)]) -> RawTemplate {
        let transactions: Vec<_> = txs
            .iter()
            .map(|(data, txid, hash)| {
                let mut tx = json!({ "data": data, "txid": hash_of(*txid) });
                if let Some(h) = hash {
                    tx["hash"] = json!(hash_of(*h));
                }
                tx
            })
            .collect();
        serde_json::from_value(json!({
            "version": 536870912,
            "previousblockhash": hash_of(0xee),
            "transactions": transactions,
            "coinbasevalue": 5000000000u64,
            "mintime": 1296688603,
            "curtime": 1788619193,
            "bits": "207fffff",
            "height": 5,
            "default_witness_commitment": "6a24aa21a9ede2f61c3f71d1defd3fa999dfa36953755c690689799962b48bebd836974e8cf9"
        }))
        .unwrap()
    }

    #[test]
    fn successive_templates_share_common_transaction_bytes() {
        let first = [
            ("aa01", 1, Some(0x81)), // kept
            ("bb02", 2, Some(0x82)), // kept
            ("cc03", 3, Some(0x83)), // same txid next time, different witness
            ("dd04", 4, None),       // no hash: never cached
            ("ee05", 5, Some(0x85)), // dropped from the next template
        ];
        let second = [
            ("bb02", 2, Some(0x82)),
            ("cc0399", 3, Some(0x93)),
            ("dd04", 4, None),
            ("aa01", 1, Some(0x81)),
            ("ff06", 6, Some(0x86)), // new
        ];
        let mut cache = TxCache::default();
        let a =
            convert_cached(raw_with(&first), &Bitcoin, b"", JobId(1), true, &mut cache).unwrap();
        assert_eq!(
            cache.by_wtxid.len(),
            4,
            "the hashless transaction is not cached"
        );
        let b = convert_cached(
            raw_with(&second),
            &Bitcoin,
            b"",
            JobId(2),
            false,
            &mut cache,
        )
        .unwrap();

        // Shared transactions are the same allocation.
        assert!(Arc::ptr_eq(
            &a.transactions[1].data,
            &b.transactions[0].data
        ));
        assert!(Arc::ptr_eq(
            &a.transactions[0].data,
            &b.transactions[3].data
        ));
        // Same txid, different wtxid: decoded fresh with its own bytes.
        assert_eq!(a.transactions[2].txid, b.transactions[1].txid);
        assert!(!Arc::ptr_eq(
            &a.transactions[2].data,
            &b.transactions[1].data
        ));
        assert_eq!(&b.transactions[1].data[..], &[0xcc, 0x03, 0x99]);
        // No hash: never reused, even with identical bytes.
        assert!(!Arc::ptr_eq(
            &a.transactions[3].data,
            &b.transactions[2].data
        ));

        // The cache now holds exactly the second template's hashed transactions.
        let mut keys: Vec<Hash256> = cache.by_wtxid.keys().copied().collect();
        keys.sort();
        let mut expected: Vec<Hash256> = [0x81, 0x82, 0x86, 0x93]
            .iter()
            .map(|n| from_display_hex(&hash_of(*n)).unwrap())
            .collect();
        expected.sort();
        assert_eq!(keys, expected);
        for (i, wtxid) in [(3, 0x81), (0, 0x82)] {
            let key = from_display_hex(&hash_of(wtxid)).unwrap();
            assert!(Arc::ptr_eq(&cache.by_wtxid[&key], &b.transactions[i].data));
        }

        // Identical work to cache-less conversion.
        for (work, raw) in [(&a, raw_with(&first)), (&b, raw_with(&second))] {
            let plain = convert(raw, &Bitcoin, b"", work.id, work.clean_jobs).unwrap();
            assert_eq!(work.transactions, plain.transactions);
            assert_eq!(work.merkle_branch, plain.merkle_branch);
            assert_eq!(work.extra_payload, plain.extra_payload);
        }
    }

    #[test]
    fn cached_bytes_need_a_matching_length_and_failures_keep_the_cache() {
        let mut cache = TxCache::default();
        let first = [("aa01", 1, Some(0x81))];
        let a =
            convert_cached(raw_with(&first), &Bitcoin, b"", JobId(1), true, &mut cache).unwrap();
        // Same wtxid but a different length: not trusted, decoded as given.
        let longer = [("aa0102", 1, Some(0x81))];
        let b = convert_cached(
            raw_with(&longer),
            &Bitcoin,
            b"",
            JobId(2),
            false,
            &mut cache,
        )
        .unwrap();
        assert!(!Arc::ptr_eq(
            &a.transactions[0].data,
            &b.transactions[0].data
        ));
        assert_eq!(&b.transactions[0].data[..], &[0xaa, 0x01, 0x02]);

        // A conversion that fails leaves the cache as it was.
        let bad = raw_with(&[("aa0102", 1, Some(0x81)), ("zz", 2, Some(0x82))]);
        assert!(convert_cached(bad, &Bitcoin, b"", JobId(3), false, &mut cache).is_err());
        assert_eq!(cache.by_wtxid.len(), 1);
        let key = from_display_hex(&hash_of(0x81)).unwrap();
        assert!(Arc::ptr_eq(&cache.by_wtxid[&key], &b.transactions[0].data));
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
