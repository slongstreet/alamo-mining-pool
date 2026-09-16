//! Live operator settings: overrides stored in the database, layered on the config file
//! and applied without a restart.
//!
//! The file remains the source of every value that has no override. Overrides are keyed
//! like the file (`pool.name`, `coins.ltc.fallback_address`, `stratum.vardiff.min_difficulty`,
//! `log.level`, and `pool.coinbase_tag`, which sets every coin's tag at once).

use crate::config::Config;
use crate::pool::Chains;
use alamo_coins::{Chain, Coin, CoinConfig, RpcClient};
use alamo_core::auxpow::COMMITMENT_LEN;
use alamo_core::coinbase::MAX_COINBASE_SCRIPT_LEN;
use alamo_core::payout::{AuxPayoutTable, PayoutSet, PayoutTable};
use alamo_core::time::now_unix;
use alamo_store::Store;
use alamo_stratum::job::{EXTRANONCE1_LEN, EXTRANONCE2_LEN};
use alamo_stratum::VardiffConfig;
use alamo_web::settings::BoxFuture;
use alamo_web::{
    AppState, CoinSettings, LogControl, NodeProbe, Operator, Setting, SettingsDoc, SettingsError,
    SettingsPatch, VardiffSettings,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::watch;

/// Tag used when the config file names none.
pub const DEFAULT_COINBASE_TAG: &str = "/alamo/";
/// Longest pool name accepted.
const MAX_POOL_NAME_CHARS: usize = 64;
/// Log levels the settings page offers.
const LOG_LEVELS: [&str; 5] = ["error", "warn", "info", "debug", "trace"];
/// Crates whose debug and trace output would drown the pool's own.
const NOISY_CRATES: &str = "sqlx=warn,hyper=info,hyper_util=info,h2=info,rustls=info,reqwest=info";

/// How the process filters its log, and the hook that changes it.
#[derive(Clone)]
pub struct LogSetup {
    /// The directive in force at startup (`RUST_LOG`, or `info`).
    pub filter: String,
    /// Replaces the process-wide filter.
    pub control: LogControl,
}

/// One connected coin, as the settings need it.
struct CoinRef {
    key: String,
    coin: Arc<dyn Coin>,
    chain: Chain,
    rpc: RpcClient,
    config: CoinConfig,
}

/// The values the config file provides.
struct FileValues {
    pool_name: String,
    coinbase_tag: String,
    vardiff: VardiffConfig,
    log_filter: String,
}

/// The effective values after overrides.
struct Effective {
    pool_name: String,
    coinbase_tag: String,
    fallback: BTreeMap<String, String>,
    vardiff: VardiffConfig,
    log_level: Option<String>,
}

/// Settings hook installed into the web state. Holds the senders the pool tasks watch.
pub struct LiveSettings {
    store: Store,
    state: AppState,
    coins: Vec<CoinRef>,
    file: FileValues,
    config_toml: String,
    tag_tx: watch::Sender<Vec<u8>>,
    payouts_tx: watch::Sender<Arc<PayoutSet>>,
    vardiff_tx: watch::Sender<VardiffConfig>,
    log: LogSetup,
    /// The filter directive last handed to the log control.
    applied_filter: Mutex<String>,
    /// Stored overrides. `apply` serializes on `apply_lock` and updates this last.
    overrides: Mutex<BTreeMap<String, String>>,
    apply_lock: tokio::sync::Mutex<()>,
}

impl LiveSettings {
    /// Build the settings from the file and the connected chains, load stored overrides
    /// (dropping any that no longer validate), and apply them.
    pub async fn start(
        config: &Config,
        chains: &Chains,
        store: Store,
        state: AppState,
        log: LogSetup,
    ) -> anyhow::Result<Arc<Self>> {
        let coins: Vec<CoinRef> = std::iter::once(&chains.parent)
            .chain(chains.aux.iter())
            .map(|node| CoinRef {
                key: node.key.clone(),
                coin: node.coin.clone(),
                chain: node.chain,
                rpc: node.rpc.clone(),
                config: node.config.clone(),
            })
            .collect();
        let file = FileValues {
            pool_name: config.pool.name.clone(),
            coinbase_tag: chains
                .parent
                .config
                .coinbase_tag
                .clone()
                .unwrap_or_else(|| DEFAULT_COINBASE_TAG.to_string()),
            vardiff: config.stratum.vardiff.clone(),
            log_filter: log.filter.clone(),
        };
        let this = Self {
            store,
            state,
            coins,
            config_toml: config.redacted_toml(),
            tag_tx: watch::Sender::new(file.coinbase_tag.clone().into_bytes()),
            payouts_tx: watch::Sender::new(Arc::new(chains.payout_set())),
            vardiff_tx: watch::Sender::new(file.vardiff.clone()),
            applied_filter: Mutex::new(log.filter.clone()),
            file,
            log,
            overrides: Mutex::new(BTreeMap::new()),
            apply_lock: tokio::sync::Mutex::new(()),
        };

        let stored = this.store.load_settings().await?;
        let mut kept = BTreeMap::new();
        for (key, value) in stored {
            let mut trial = kept.clone();
            trial.insert(key.clone(), value.clone());
            match this.validate(&trial) {
                Ok(_) => {
                    kept = trial;
                }
                Err(err) => {
                    tracing::warn!(setting = %key, %value, %err, "dropping stored setting that no longer validates");
                    this.store.clear_setting(&key).await?;
                }
            }
        }
        if !kept.is_empty() {
            tracing::info!(overrides = ?kept.keys().collect::<Vec<_>>(), "applying stored settings");
        }
        let effective = this.validate(&kept).expect("kept overrides validate");
        this.apply_live(&effective, true);
        *this.overrides.lock().expect("overrides lock") = kept;
        Ok(Arc::new(this))
    }

    /// Receiver for the coinbase tag bytes.
    pub fn coinbase_tag(&self) -> watch::Receiver<Vec<u8>> {
        self.tag_tx.subscribe()
    }

    /// Receiver for the payout tables.
    pub fn payouts(&self) -> watch::Receiver<Arc<PayoutSet>> {
        self.payouts_tx.subscribe()
    }

    /// Receiver for the vardiff settings.
    pub fn vardiff(&self) -> watch::Receiver<VardiffConfig> {
        self.vardiff_tx.subscribe()
    }

    fn merge_mining(&self) -> bool {
        self.coins.len() > 1
    }

    /// Longest coinbase tag the parent coinbase can carry: the scriptSig limit less the
    /// BIP34 height push, the tag's own push opcode, the extranonce, and, when merge
    /// mining, the aux commitment push.
    pub fn tag_budget(&self) -> usize {
        let commitment = if self.merge_mining() {
            1 + COMMITMENT_LEN
        } else {
            0
        };
        MAX_COINBASE_SCRIPT_LEN - 5 - 1 - (EXTRANONCE1_LEN + EXTRANONCE2_LEN) - commitment
    }

    fn coin(&self, key: &str) -> Option<&CoinRef> {
        self.coins.iter().find(|c| c.key == key)
    }

    fn fallback_key(key: &str) -> String {
        format!("coins.{key}.fallback_address")
    }

    fn vardiff_key(field: &str) -> String {
        format!("stratum.vardiff.{field}")
    }

    /// Check a full set of overrides and compute the values they produce.
    fn validate(&self, ov: &BTreeMap<String, String>) -> Result<Effective, SettingsError> {
        let invalid = |msg: String| SettingsError::Invalid(msg);
        for key in ov.keys() {
            if !self.known_key(key) {
                return Err(invalid(format!("unknown setting {key}")));
            }
        }

        let pool_name = ov
            .get("pool.name")
            .cloned()
            .unwrap_or_else(|| self.file.pool_name.clone());
        let trimmed = pool_name.trim();
        if trimmed.is_empty() {
            return Err(invalid("pool name must not be empty".into()));
        }
        if trimmed.chars().count() > MAX_POOL_NAME_CHARS {
            return Err(invalid(format!(
                "pool name must be at most {MAX_POOL_NAME_CHARS} characters"
            )));
        }

        let coinbase_tag = ov
            .get("pool.coinbase_tag")
            .cloned()
            .unwrap_or_else(|| self.file.coinbase_tag.clone());
        if coinbase_tag.chars().any(|c| c.is_control()) {
            return Err(invalid(
                "coinbase tag must not contain control characters".into(),
            ));
        }
        let budget = self.tag_budget();
        if coinbase_tag.len() > budget {
            return Err(invalid(format!(
                "coinbase tag is {} bytes; the coinbase has room for {budget}",
                coinbase_tag.len()
            )));
        }

        let mut fallback = BTreeMap::new();
        for coin in &self.coins {
            let address = ov
                .get(&Self::fallback_key(&coin.key))
                .cloned()
                .unwrap_or_else(|| coin.config.fallback_address.clone());
            let address = address.trim().to_string();
            PayoutTable::new(coin.coin.address_params(coin.chain), &address).map_err(|_| {
                invalid(format!(
                    "{address:?} is not a valid {} address for {}",
                    coin.coin.symbol(),
                    coin.chain
                ))
            })?;
            fallback.insert(coin.key.clone(), address);
        }

        let mut vardiff = self.file.vardiff.clone();
        let number = |field: &str, current: f64| -> Result<f64, SettingsError> {
            match ov.get(&Self::vardiff_key(field)) {
                Some(raw) => raw
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .ok_or_else(|| invalid(format!("stratum.vardiff.{field} must be a number"))),
                None => Ok(current),
            }
        };
        vardiff.initial_difficulty = number("initial_difficulty", vardiff.initial_difficulty)?;
        vardiff.min_difficulty = number("min_difficulty", vardiff.min_difficulty)?;
        vardiff.max_difficulty = number("max_difficulty", vardiff.max_difficulty)?;
        vardiff.target_share_seconds =
            number("target_share_seconds", vardiff.target_share_seconds)?;
        vardiff.retarget_seconds = number("retarget_seconds", vardiff.retarget_seconds)?;
        vardiff.variance_percent = number("variance_percent", vardiff.variance_percent)?;
        vardiff.validate().map_err(invalid)?;
        if vardiff.variance_percent < 0.0 {
            return Err(invalid(
                "stratum.vardiff.variance_percent must not be negative".into(),
            ));
        }

        let log_level = match ov.get("log.level") {
            Some(level) => {
                let level = level.trim().to_ascii_lowercase();
                if !LOG_LEVELS.contains(&level.as_str()) {
                    return Err(invalid(format!(
                        "log level must be one of {}",
                        LOG_LEVELS.join(", ")
                    )));
                }
                Some(level)
            }
            None => None,
        };

        Ok(Effective {
            pool_name: trimmed.to_string(),
            coinbase_tag,
            fallback,
            vardiff,
            log_level,
        })
    }

    fn known_key(&self, key: &str) -> bool {
        key == "pool.name"
            || key == "pool.coinbase_tag"
            || key == "log.level"
            || self.coins.iter().any(|c| key == Self::fallback_key(&c.key))
            || [
                "initial_difficulty",
                "min_difficulty",
                "max_difficulty",
                "target_share_seconds",
                "retarget_seconds",
                "variance_percent",
            ]
            .iter()
            .any(|f| key == Self::vardiff_key(f))
    }

    /// Push effective values to the running pool. At startup everything is sent; later
    /// only what changed, so watchers are not woken for nothing.
    fn apply_live(&self, e: &Effective, startup: bool) {
        if startup || self.state.pool_name() != e.pool_name {
            self.state.set_pool_name(&e.pool_name);
        }
        let tag = e.coinbase_tag.clone().into_bytes();
        if startup || *self.tag_tx.borrow() != tag {
            self.tag_tx.send_replace(tag);
        }
        if startup || *self.vardiff_tx.borrow() != e.vardiff {
            self.vardiff_tx.send_replace(e.vardiff.clone());
        }
        let current: Vec<String> = {
            let set = self.payouts_tx.borrow();
            std::iter::once(set.parent.fallback_address().to_string())
                .chain(
                    set.aux
                        .iter()
                        .map(|a| a.table.fallback_address().to_string()),
                )
                .collect()
        };
        let wanted: Vec<&String> = self.coins.iter().map(|c| &e.fallback[&c.key]).collect();
        if startup || current.iter().ne(wanted.iter().copied()) {
            match self.payout_set(&e.fallback) {
                Ok(set) => {
                    self.payouts_tx.send_replace(Arc::new(set));
                }
                Err(err) => tracing::error!(%err, "payout tables not rebuilt"),
            }
        }
        let directive = match &e.log_level {
            Some(level) if level == "debug" || level == "trace" => {
                format!("{level},{NOISY_CRATES}")
            }
            Some(level) => level.clone(),
            None => self.file.log_filter.clone(),
        };
        let mut applied = self.applied_filter.lock().expect("filter lock");
        if *applied != directive {
            match (self.log.control)(&directive) {
                Ok(()) => {
                    tracing::info!(filter = %directive, "log filter set");
                    *applied = directive;
                }
                Err(err) => tracing::warn!(%err, filter = %directive, "log filter not applied"),
            }
        }
    }

    fn payout_set(&self, fallback: &BTreeMap<String, String>) -> Result<PayoutSet, String> {
        let table = |c: &CoinRef| {
            PayoutTable::new(c.coin.address_params(c.chain), &fallback[&c.key])
                .map_err(|e| format!("{}: {e}", c.key))
        };
        let (parent, aux) = self.coins.split_first().ok_or("no coins")?;
        Ok(PayoutSet {
            parent: table(parent)?,
            aux: aux
                .iter()
                .map(|c| {
                    Ok(AuxPayoutTable {
                        coin: c.coin.symbol(),
                        table: table(c)?,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?,
        })
    }

    fn doc(&self, ov: &BTreeMap<String, String>) -> SettingsDoc {
        let text = |key: &str, file: &str| match ov.get(key) {
            Some(v) => Setting::overridden(file.to_string(), v.clone()),
            None => Setting::from_file(file.to_string()),
        };
        let number = |field: &str, file: f64| match ov
            .get(&Self::vardiff_key(field))
            .and_then(|v| v.parse::<f64>().ok())
        {
            Some(v) => Setting::overridden(file, v),
            None => Setting::from_file(file),
        };
        let v = &self.file.vardiff;
        SettingsDoc {
            read_only: false,
            pool_name: text("pool.name", &self.file.pool_name),
            coinbase_tag: text("pool.coinbase_tag", &self.file.coinbase_tag),
            coinbase_tag_max_bytes: self.tag_budget(),
            coins: self
                .coins
                .iter()
                .map(|c| CoinSettings {
                    key: c.key.clone(),
                    symbol: c.coin.symbol().to_string(),
                    chain: c.chain.to_string(),
                    merge_mined_with: c.config.merge_mined_with.clone(),
                    fallback_address: text(&Self::fallback_key(&c.key), &c.config.fallback_address),
                    rpc_url: c.config.rpc_url.clone(),
                    rpc_user: c.config.rpc_user.clone(),
                    zmq_hashblock: c.config.zmq_hashblock.clone(),
                    poll_interval_ms: c.config.poll_interval_ms,
                    template_refresh_secs: c.config.template_refresh_secs,
                    template_stale_secs: c.config.template_stale_secs,
                })
                .collect(),
            vardiff: VardiffSettings {
                initial_difficulty: number("initial_difficulty", v.initial_difficulty),
                min_difficulty: number("min_difficulty", v.min_difficulty),
                max_difficulty: number("max_difficulty", v.max_difficulty),
                target_share_seconds: number("target_share_seconds", v.target_share_seconds),
                retarget_seconds: number("retarget_seconds", v.retarget_seconds),
                variance_percent: number("variance_percent", v.variance_percent),
            },
            log_level: text("log.level", &self.file.log_filter),
            config_toml: self.config_toml.clone(),
        }
    }

    /// The overrides `patch` produces on top of `current`.
    fn patched(
        &self,
        current: &BTreeMap<String, String>,
        patch: &SettingsPatch,
    ) -> Result<BTreeMap<String, String>, SettingsError> {
        let mut next = current.clone();
        let mut set = |key: String, change: &Option<Option<String>>| match change {
            Some(Some(value)) => {
                next.insert(key, value.clone());
            }
            Some(None) => {
                next.remove(&key);
            }
            None => {}
        };
        set("pool.name".into(), &patch.pool_name);
        set("pool.coinbase_tag".into(), &patch.coinbase_tag);
        set("log.level".into(), &patch.log_level);
        for (key, change) in &patch.fallback_addresses {
            if self.coin(key).is_none() {
                return Err(SettingsError::Invalid(format!("unknown coin {key}")));
            }
            set(Self::fallback_key(key), &Some(change.clone()));
        }
        let v = &patch.vardiff;
        for (field, change) in [
            ("initial_difficulty", &v.initial_difficulty),
            ("min_difficulty", &v.min_difficulty),
            ("max_difficulty", &v.max_difficulty),
            ("target_share_seconds", &v.target_share_seconds),
            ("retarget_seconds", &v.retarget_seconds),
            ("variance_percent", &v.variance_percent),
        ] {
            let as_text = change.map(|c| c.map(|n| n.to_string()));
            set(Self::vardiff_key(field), &as_text);
        }
        Ok(next)
    }

    async fn apply_patch(&self, patch: SettingsPatch) -> Result<SettingsDoc, SettingsError> {
        let _guard = self.apply_lock.lock().await;
        let current = self.overrides.lock().expect("overrides lock").clone();
        let next = self.patched(&current, &patch)?;
        let effective = self.validate(&next)?;
        let store_err = |e: alamo_store::StoreError| SettingsError::Store(e.to_string());
        let now = now_unix() as i64;
        for (key, value) in &next {
            if current.get(key) != Some(value) {
                self.store
                    .set_setting(key, value, now)
                    .await
                    .map_err(store_err)?;
                tracing::info!(setting = %key, value = %value, "setting changed by operator");
            }
        }
        for key in current.keys() {
            if !next.contains_key(key) {
                self.store.clear_setting(key).await.map_err(store_err)?;
                tracing::info!(setting = %key, "setting reverted to the config file");
            }
        }
        self.apply_live(&effective, false);
        *self.overrides.lock().expect("overrides lock") = next.clone();
        Ok(self.doc(&next))
    }

    async fn probe(&self, key: &str) -> Result<NodeProbe, String> {
        let coin = self
            .coin(key)
            .ok_or_else(|| format!("unknown coin {key}"))?;
        let started = Instant::now();
        let chain: serde_json::Value = coin
            .rpc
            .call("getblockchaininfo", &[])
            .await
            .map_err(|e| e.to_string())?;
        let network: serde_json::Value = coin
            .rpc
            .call("getnetworkinfo", &[])
            .await
            .map_err(|e| e.to_string())?;
        let text = |v: &serde_json::Value, k: &str| v[k].as_str().unwrap_or_default().to_string();
        let int = |v: &serde_json::Value, k: &str| v[k].as_u64().unwrap_or_default();
        Ok(NodeProbe {
            key: coin.key.clone(),
            symbol: coin.coin.symbol().to_string(),
            chain: text(&chain, "chain"),
            height: int(&chain, "blocks"),
            subversion: text(&network, "subversion"),
            protocol_version: int(&network, "protocolversion"),
            connections: int(&network, "connections"),
            initial_block_download: chain["initialblockdownload"].as_bool().unwrap_or(false),
            latency_ms: started.elapsed().as_millis() as u64,
        })
    }
}

impl Operator for LiveSettings {
    fn settings(&self) -> SettingsDoc {
        let ov = self.overrides.lock().expect("overrides lock").clone();
        self.doc(&ov)
    }

    fn apply(&self, patch: SettingsPatch) -> BoxFuture<'_, Result<SettingsDoc, SettingsError>> {
        Box::pin(self.apply_patch(patch))
    }

    fn probe_node(&self, key: &str) -> BoxFuture<'_, Result<NodeProbe, String>> {
        let key = key.to_string();
        Box::pin(async move { self.probe(&key).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::ChainNode;
    use alamo_coins::{Dogecoin, Litecoin};
    use alamo_web::VardiffPatch;
    use std::sync::Mutex as StdMutex;

    use alamo_core::address::encode_base58;
    use std::sync::LazyLock;

    /// Valid mainnet P2PKH addresses built from fixed hashes.
    static LTC_FALLBACK: LazyLock<String> = LazyLock::new(|| {
        encode_base58(Litecoin.address_params(Chain::Main).p2pkh_prefix, &[1; 20])
    });
    static LTC_OTHER: LazyLock<String> = LazyLock::new(|| {
        encode_base58(Litecoin.address_params(Chain::Main).p2pkh_prefix, &[2; 20])
    });
    static DOGE_FALLBACK: LazyLock<String> = LazyLock::new(|| {
        encode_base58(Dogecoin.address_params(Chain::Main).p2pkh_prefix, &[3; 20])
    });

    fn node(key: &str, coin: Arc<dyn Coin>, fallback: &str, parent: Option<&str>) -> ChainNode {
        let chain = Chain::Main;
        ChainNode {
            key: key.into(),
            payouts: PayoutTable::new(coin.address_params(chain), fallback).unwrap(),
            coin,
            rpc: RpcClient::new("http://127.0.0.1:1", "u", "p"),
            chain,
            config: CoinConfig {
                enabled: true,
                merge_mined_with: parent.map(String::from),
                rpc_url: "http://127.0.0.1:1".into(),
                rpc_user: "u".into(),
                rpc_password: "secret".into(),
                zmq_hashblock: None,
                fallback_address: fallback.into(),
                coinbase_tag: Some("/file/".into()),
                poll_interval_ms: 500,
                template_refresh_secs: 30,
                template_stale_secs: 120,
            },
        }
    }

    fn chains() -> Chains {
        Chains {
            parent: node("ltc", Arc::new(Litecoin), &LTC_FALLBACK, None),
            aux: vec![node(
                "doge",
                Arc::new(Dogecoin),
                &DOGE_FALLBACK,
                Some("ltc"),
            )],
        }
    }

    fn config() -> Config {
        let text = format!(
            r#"
[pool]
name = "File Pool"
data_dir = "./data"
[stratum]
listen = "127.0.0.1:3333"
[web]
listen = "127.0.0.1:8080"
[coins.ltc]
rpc_url = "http://x"
rpc_user = "u"
rpc_password = "secret"
fallback_address = "{}"
coinbase_tag = "/file/"
[coins.doge]
merge_mined_with = "ltc"
rpc_url = "http://y"
rpc_user = "u"
rpc_password = "secret"
fallback_address = "{}"
"#,
            *LTC_FALLBACK, *DOGE_FALLBACK
        );
        toml::from_str(&text).unwrap()
    }

    struct Harness {
        live: Arc<LiveSettings>,
        state: AppState,
        store: Store,
        filters: Arc<StdMutex<Vec<String>>>,
        path: std::path::PathBuf,
    }

    async fn harness(tag: &str) -> Harness {
        let path = std::env::temp_dir()
            .join(format!("alamo-settings-{tag}-{}", std::process::id()))
            .join("pool.db");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let store = Store::open(&path).await.unwrap();
        let state = AppState::new("File Pool", 3333, store.clone());
        let filters = Arc::new(StdMutex::new(Vec::new()));
        let seen = filters.clone();
        let log = LogSetup {
            filter: "info".into(),
            control: Arc::new(move |d: &str| {
                seen.lock().unwrap().push(d.to_string());
                Ok(())
            }),
        };
        let live = LiveSettings::start(&config(), &chains(), store.clone(), state.clone(), log)
            .await
            .unwrap();
        Harness {
            live,
            state,
            store,
            filters,
            path,
        }
    }

    #[tokio::test]
    async fn document_reflects_the_file_until_a_patch_is_applied() {
        let h = harness("doc").await;
        let doc = h.live.settings();
        assert_eq!(doc.pool_name, Setting::from_file("File Pool".to_string()));
        assert_eq!(doc.coinbase_tag.value, "/file/");
        assert_eq!(doc.coinbase_tag_max_bytes, 41);
        assert_eq!(doc.coins.len(), 2);
        assert_eq!(doc.coins[0].symbol, "LTC");
        assert_eq!(doc.coins[1].merge_mined_with.as_deref(), Some("ltc"));
        assert_eq!(doc.coins[1].fallback_address.value, *DOGE_FALLBACK);
        assert_eq!(doc.vardiff.min_difficulty.value, 1024.0);
        assert_eq!(doc.log_level.value, "info");
        assert!(doc.config_toml.contains("<redacted>"));
        assert!(!doc.config_toml.contains("secret"));
        assert!(
            h.filters.lock().unwrap().is_empty(),
            "no log change at startup"
        );
        assert_eq!(*h.live.coinbase_tag().borrow(), b"/file/".to_vec());

        let doc = h
            .live
            .apply(SettingsPatch {
                pool_name: Some(Some("  Renamed  ".into())),
                coinbase_tag: Some(Some("/new tag/".into())),
                fallback_addresses: [("ltc".to_string(), Some(LTC_OTHER.clone()))].into(),
                vardiff: VardiffPatch {
                    min_difficulty: Some(Some(2048.0)),
                    ..Default::default()
                },
                log_level: Some(Some("debug".into())),
            })
            .await
            .unwrap();
        assert_eq!(
            doc.pool_name,
            Setting::overridden("File Pool".into(), "  Renamed  ".into())
        );
        assert_eq!(h.state.pool_name(), "Renamed", "applied trimmed");
        assert_eq!(h.state.snapshot().pool_name, "Renamed");
        assert_eq!(*h.live.coinbase_tag().borrow(), b"/new tag/".to_vec());
        assert_eq!(h.live.vardiff().borrow().min_difficulty, 2048.0);
        assert_eq!(
            h.live.payouts().borrow().parent.fallback_address(),
            LTC_OTHER.as_str()
        );
        assert_eq!(
            h.live.payouts().borrow().aux[0].table.fallback_address(),
            DOGE_FALLBACK.as_str()
        );
        assert!(doc.coins[0].fallback_address.overridden);
        assert!(!doc.coins[1].fallback_address.overridden);
        assert_eq!(doc.log_level.value, "debug");
        assert!(h.filters.lock().unwrap()[0].starts_with("debug,sqlx=warn"));

        let stored = h.store.load_settings().await.unwrap();
        assert_eq!(stored["pool.name"], "  Renamed  ");
        assert_eq!(stored["stratum.vardiff.min_difficulty"], "2048");
        assert_eq!(stored.len(), 5);

        // Reverting drops the row and restores the file value, including the log filter.
        let doc = h
            .live
            .apply(SettingsPatch {
                pool_name: Some(None),
                log_level: Some(None),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(!doc.pool_name.overridden);
        assert_eq!(h.state.pool_name(), "File Pool");
        assert_eq!(h.filters.lock().unwrap().last().unwrap(), "info");
        assert_eq!(h.store.load_settings().await.unwrap().len(), 3);
        let _ = std::fs::remove_dir_all(h.path.parent().unwrap());
    }

    #[tokio::test]
    async fn invalid_patches_change_nothing() {
        let h = harness("invalid").await;
        let cases: Vec<(SettingsPatch, &str)> = vec![
            (
                SettingsPatch {
                    pool_name: Some(Some("   ".into())),
                    ..Default::default()
                },
                "must not be empty",
            ),
            (
                SettingsPatch {
                    coinbase_tag: Some(Some("x".repeat(42))),
                    ..Default::default()
                },
                "room for 41",
            ),
            (
                SettingsPatch {
                    coinbase_tag: Some(Some("a\nb".into())),
                    ..Default::default()
                },
                "control characters",
            ),
            (
                SettingsPatch {
                    fallback_addresses: [("ltc".to_string(), Some(DOGE_FALLBACK.clone()))].into(),
                    ..Default::default()
                },
                "not a valid LTC address",
            ),
            (
                SettingsPatch {
                    fallback_addresses: [("btc".to_string(), Some("x".to_string()))].into(),
                    ..Default::default()
                },
                "unknown coin",
            ),
            (
                SettingsPatch {
                    vardiff: VardiffPatch {
                        min_difficulty: Some(Some(1e9)),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                "max_difficulty must be >= min_difficulty",
            ),
            (
                SettingsPatch {
                    log_level: Some(Some("loud".into())),
                    ..Default::default()
                },
                "log level must be one of",
            ),
        ];
        for (patch, expected) in cases {
            let err = h.live.apply(patch).await.unwrap_err().to_string();
            assert!(err.contains(expected), "{err}");
        }
        assert!(h.store.load_settings().await.unwrap().is_empty());
        assert_eq!(h.state.pool_name(), "File Pool");
        assert!(h.filters.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(h.path.parent().unwrap());
    }

    #[tokio::test]
    async fn stored_overrides_apply_at_startup_and_bad_ones_are_dropped() {
        let h = harness("restart").await;
        h.store.set_setting("pool.name", "Stored", 1).await.unwrap();
        h.store
            .set_setting("coins.doge.fallback_address", "not-an-address", 1)
            .await
            .unwrap();
        h.store.set_setting("log.level", "trace", 1).await.unwrap();
        h.store.set_setting("nope.key", "x", 1).await.unwrap();
        let filters = Arc::new(StdMutex::new(Vec::new()));
        let seen = filters.clone();
        let log = LogSetup {
            filter: "info".into(),
            control: Arc::new(move |d: &str| {
                seen.lock().unwrap().push(d.to_string());
                Ok(())
            }),
        };
        let state = AppState::new("File Pool", 3333, h.store.clone());
        let live = LiveSettings::start(&config(), &chains(), h.store.clone(), state.clone(), log)
            .await
            .unwrap();
        assert_eq!(state.pool_name(), "Stored");
        assert!(filters.lock().unwrap()[0].starts_with("trace,"));
        let doc = live.settings();
        assert!(doc.pool_name.overridden);
        assert!(
            !doc.coins[1].fallback_address.overridden,
            "invalid override dropped"
        );
        let stored = h.store.load_settings().await.unwrap();
        assert_eq!(
            stored.keys().collect::<Vec<_>>(),
            vec!["log.level", "pool.name"],
            "bad rows removed"
        );
        let _ = std::fs::remove_dir_all(h.path.parent().unwrap());
    }
}
