//! Live operator settings: overrides stored in the database, layered on the config file
//! and applied without a restart.
//!
//! The file remains the source of every value that has no override. Overrides are keyed
//! like the config path they override: `pool.name`, `coins.ltc.fallback_address`,
//! `log.level`, `pool.coinbase_tag` (which sets every coin's tag at once), and a lane's
//! vardiff: `stratum.vardiff.<field>` for every lane on the default `[stratum]` section,
//! `coins.<key>.stratum.vardiff.<field>` for a lane with its own section.
//!
//! Settings start from the config alone, before any node answers. A fallback-address
//! override can only be checked against its coin's network, so one for a coin whose lane
//! has not connected yet is kept pending and validated when that lane connects.

use crate::config::{Config, LaneConfig};
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
    AppState, CoinSettings, LaneSettings, LogControl, NodeProbe, Operator, Setting, SettingsDoc,
    SettingsError, SettingsPatch, VardiffPatch, VardiffSettings,
};
use anyhow::Context;
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
/// The vardiff fields an override may set, in the order the settings page lists them.
const VARDIFF_FIELDS: [&str; 6] = [
    "initial_difficulty",
    "min_difficulty",
    "max_difficulty",
    "target_share_seconds",
    "retarget_seconds",
    "variance_percent",
];

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

/// One lane's settings, known from the config alone.
struct LaneSlot {
    key: String,
    /// Config path of the lane's vardiff table; overrides are `<path>.<field>`.
    vardiff_path: String,
    file_vardiff: VardiffConfig,
    vardiff_tx: watch::Sender<VardiffConfig>,
    stratum_listen: String,
    own_stratum: bool,
    /// The lane's coin keys, parent first.
    coin_keys: Vec<String>,
    /// The same coins' configuration, for the document before they connect.
    coin_configs: Vec<CoinConfig>,
}

/// A lane whose nodes have connected: its coins and the payout tables built from them.
struct ConnectedLane {
    lane: usize,
    /// Parent first, then aux coins in config order.
    coins: Vec<CoinRef>,
    payouts_tx: watch::Sender<Arc<PayoutSet>>,
}

/// The values the config file provides.
struct FileValues {
    pool_name: String,
    coinbase_tag: String,
    log_filter: String,
}

/// The effective values after overrides.
struct Effective {
    pool_name: String,
    coinbase_tag: String,
    /// Fallback address per connected coin key.
    fallback: BTreeMap<String, String>,
    /// Vardiff per lane, in lane order.
    vardiff: Vec<VardiffConfig>,
    log_level: Option<String>,
}

/// Settings hook installed into the web state. Holds the senders the pool tasks watch.
pub struct LiveSettings {
    store: Store,
    state: AppState,
    lanes: Vec<LaneSlot>,
    /// Index of the primary lane, listed first in the document.
    primary: usize,
    /// Tightest coinbase tag budget over every configured lane.
    tag_budget: usize,
    /// Lanes that have connected, in lane order.
    connected: Mutex<Vec<Arc<ConnectedLane>>>,
    file: FileValues,
    config_toml: String,
    tag_tx: watch::Sender<Vec<u8>>,
    log: LogSetup,
    /// The filter directive last handed to the log control.
    applied_filter: Mutex<String>,
    /// Overrides in force. `apply` serializes on `apply_lock` and updates this last.
    overrides: Mutex<BTreeMap<String, String>>,
    /// Stored fallback-address overrides for coins whose lane has not connected yet.
    pending: Mutex<BTreeMap<String, String>>,
    apply_lock: tokio::sync::Mutex<()>,
}

/// Longest coinbase tag a parent coinbase can carry: the scriptSig limit less the BIP34
/// height push, the tag's own push opcode, the extranonce, and, when merge mining, the
/// aux commitment push.
fn tag_budget_for(merge_mining: bool) -> usize {
    let commitment = if merge_mining { 1 + COMMITMENT_LEN } else { 0 };
    MAX_COINBASE_SCRIPT_LEN - 5 - 1 - (EXTRANONCE1_LEN + EXTRANONCE2_LEN) - commitment
}

impl LiveSettings {
    /// Build the settings from the config file, load stored overrides, and apply them.
    /// Overrides for keys this configuration does not have, or that fail validation, are
    /// dropped; fallback addresses wait for their lane (see [`lane_connected`]).
    ///
    /// [`lane_connected`]: Self::lane_connected
    pub async fn start(
        config: &Config,
        store: Store,
        state: AppState,
        log: LogSetup,
    ) -> anyhow::Result<Arc<Self>> {
        let lane_configs = config.lanes();
        let primary = Config::primary_lane_index(&lane_configs);
        let lanes: Vec<LaneSlot> = lane_configs
            .iter()
            .map(|l: &LaneConfig| {
                let coin_keys: Vec<String> = std::iter::once(l.key.clone())
                    .chain(l.aux.iter().cloned())
                    .collect();
                LaneSlot {
                    key: l.key.clone(),
                    vardiff_path: l.vardiff_path(),
                    file_vardiff: l.vardiff().clone(),
                    vardiff_tx: watch::Sender::new(l.vardiff().clone()),
                    stratum_listen: l.stratum.listen.to_string(),
                    own_stratum: l.own_stratum,
                    coin_configs: coin_keys
                        .iter()
                        .map(|k| {
                            config
                                .coins
                                .get(k)
                                .cloned()
                                .expect("lane coins are configured")
                        })
                        .collect(),
                    coin_keys,
                }
            })
            .collect();
        let file = FileValues {
            pool_name: config.pool.name.clone(),
            coinbase_tag: lane_configs
                .get(primary)
                .and_then(|l| config.coins.get(&l.key))
                .and_then(|c| c.coinbase_tag.clone())
                .unwrap_or_else(|| DEFAULT_COINBASE_TAG.to_string()),
            log_filter: log.filter.clone(),
        };
        let this = Self {
            store,
            state,
            lanes,
            primary,
            tag_budget: tag_budget_for(lane_configs.iter().any(LaneConfig::merge_mines)),
            connected: Mutex::new(Vec::new()),
            config_toml: config.redacted_toml(),
            tag_tx: watch::Sender::new(file.coinbase_tag.clone().into_bytes()),
            applied_filter: Mutex::new(log.filter.clone()),
            file,
            log,
            overrides: Mutex::new(BTreeMap::new()),
            pending: Mutex::new(BTreeMap::new()),
            apply_lock: tokio::sync::Mutex::new(()),
        };

        let stored = this.store.load_settings().await?;
        let mut kept = BTreeMap::new();
        let mut pending = BTreeMap::new();
        for (key, value) in stored {
            if this.known_key(&key) && Self::fallback_coin(&key).is_some() {
                pending.insert(key, value);
                continue;
            }
            let mut trial = kept.clone();
            trial.insert(key.clone(), value.clone());
            match this.validate(&trial, &[]) {
                Ok(_) => kept = trial,
                Err(err) => {
                    tracing::warn!(setting = %key, %value, %err, "dropping stored setting that no longer validates");
                    this.store.clear_setting(&key).await?;
                }
            }
        }
        if !kept.is_empty() {
            tracing::info!(overrides = ?kept.keys().collect::<Vec<_>>(), "applying stored settings");
        }
        if !pending.is_empty() {
            tracing::info!(overrides = ?pending.keys().collect::<Vec<_>>(), "stored settings wait for their coin's node");
        }
        let effective = this.validate(&kept, &[]).expect("kept overrides validate");
        this.apply_live(&effective, &[], true);
        *this.overrides.lock().expect("overrides lock") = kept;
        *this.pending.lock().expect("pending lock") = pending;
        Ok(Arc::new(this))
    }

    /// Register a lane whose nodes have all connected: validate the stored fallback
    /// addresses that were waiting for its coins (dropping the invalid ones, as at
    /// startup), and return the receiver for its payout tables.
    pub async fn lane_connected(
        &self,
        chains: &Chains,
    ) -> anyhow::Result<watch::Receiver<Arc<PayoutSet>>> {
        let _guard = self.apply_lock.lock().await;
        let lane = self
            .lane_index(&chains.parent.key)
            .with_context(|| format!("lane '{}' is not configured", chains.parent.key))?;
        let coins: Vec<CoinRef> = chains
            .nodes()
            .map(|node| CoinRef {
                key: node.key.clone(),
                coin: node.coin.clone(),
                chain: node.chain,
                rpc: node.rpc.clone(),
                config: node.config.clone(),
            })
            .collect();
        let entry = Arc::new(ConnectedLane {
            lane,
            coins,
            payouts_tx: watch::Sender::new(Arc::new(chains.payout_set())),
        });
        let receiver = entry.payouts_tx.subscribe();
        let connected = {
            let mut all = self.connected.lock().expect("connected lock");
            all.retain(|c| c.lane != lane);
            all.push(entry);
            all.sort_by_key(|c| c.lane);
            all.clone()
        };

        let mut current = self.overrides.lock().expect("overrides lock").clone();
        let waiting: Vec<(String, String)> = {
            let mut pending = self.pending.lock().expect("pending lock");
            let keys: Vec<String> = pending
                .keys()
                .filter(|k| {
                    Self::fallback_coin(k)
                        .is_some_and(|coin| self.lanes[lane].coin_keys.iter().any(|c| c == coin))
                })
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|k| pending.remove_entry(&k))
                .collect()
        };
        for (key, value) in waiting {
            let mut trial = current.clone();
            trial.insert(key.clone(), value.clone());
            match self.validate(&trial, &connected) {
                Ok(_) => {
                    tracing::info!(setting = %key, "applying stored setting");
                    current = trial;
                }
                Err(err) => {
                    tracing::warn!(setting = %key, %value, %err, "dropping stored setting that no longer validates");
                    self.store.clear_setting(&key).await?;
                }
            }
        }
        let effective = self
            .validate(&current, &connected)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        self.apply_live(&effective, &connected, false);
        *self.overrides.lock().expect("overrides lock") = current;
        Ok(receiver)
    }

    /// Forget a lane that failed after [`lane_connected`](Self::lane_connected): its coins
    /// leave the document's connected set, and their fallback-address overrides go back
    /// to waiting (kept in the database) so a restart with a fixed config applies them.
    pub async fn lane_detached(&self, key: &str) {
        let _guard = self.apply_lock.lock().await;
        let Some(lane) = self.lane_index(key) else {
            return;
        };
        let connected = {
            let mut all = self.connected.lock().expect("connected lock");
            all.retain(|c| c.lane != lane);
            all.clone()
        };
        let mut current = self.overrides.lock().expect("overrides lock").clone();
        let mut pending = self.pending.lock().expect("pending lock");
        let coins = &self.lanes[lane].coin_keys;
        let moved: Vec<String> = current
            .keys()
            .filter(|k| Self::fallback_coin(k).is_some_and(|c| coins.iter().any(|k| k == c)))
            .cloned()
            .collect();
        for key in moved {
            if let Some(value) = current.remove(&key) {
                pending.insert(key, value);
            }
        }
        drop(pending);
        match self.validate(&current, &connected) {
            Ok(effective) => self.apply_live(&effective, &connected, false),
            Err(err) => tracing::warn!(%err, "settings not reapplied after a lane failed"),
        }
        *self.overrides.lock().expect("overrides lock") = current;
    }

    /// Receiver for the coinbase tag bytes, shared by every lane.
    pub fn coinbase_tag(&self) -> watch::Receiver<Vec<u8>> {
        self.tag_tx.subscribe()
    }

    /// Receiver for one lane's vardiff settings.
    pub fn vardiff(&self, lane: &str) -> Option<watch::Receiver<VardiffConfig>> {
        let i = self.lane_index(lane)?;
        Some(self.lanes[i].vardiff_tx.subscribe())
    }

    /// Longest coinbase tag every configured lane's coinbase can carry, from the config
    /// alone: a lane merge-mines when it has an enabled aux coin.
    pub fn tag_budget(&self) -> usize {
        self.tag_budget
    }

    fn lane_index(&self, key: &str) -> Option<usize> {
        self.lanes.iter().position(|l| l.key == key)
    }

    fn connected(&self) -> Vec<Arc<ConnectedLane>> {
        self.connected.lock().expect("connected lock").clone()
    }

    /// The coin a `coins.<key>.fallback_address` key names.
    fn fallback_coin(key: &str) -> Option<&str> {
        key.strip_prefix("coins.")?
            .strip_suffix(".fallback_address")
            .filter(|k| !k.contains('.'))
    }

    fn fallback_key(key: &str) -> String {
        format!("coins.{key}.fallback_address")
    }

    fn vardiff_key(lane: &LaneSlot, field: &str) -> String {
        format!("{}.{field}", lane.vardiff_path)
    }

    /// Check a full set of overrides against the lanes in `connected` and compute the
    /// values they produce. A fallback address for a coin that has not connected cannot
    /// be checked, so it is refused here; `start` holds such rows back as pending.
    fn validate(
        &self,
        ov: &BTreeMap<String, String>,
        connected: &[Arc<ConnectedLane>],
    ) -> Result<Effective, SettingsError> {
        let invalid = |msg: String| SettingsError::Invalid(msg);
        for key in ov.keys() {
            if !self.known_key(key) {
                return Err(invalid(format!("unknown setting {key}")));
            }
            if let Some(coin) = Self::fallback_coin(key) {
                if !connected
                    .iter()
                    .flat_map(|l| &l.coins)
                    .any(|c| c.key == coin)
                {
                    return Err(invalid(format!(
                        "coin {coin} has not connected yet; its address cannot be checked"
                    )));
                }
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
        for coin in connected.iter().flat_map(|l| &l.coins) {
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

        let vardiff = self
            .lanes
            .iter()
            .map(|lane| Self::lane_vardiff(lane, ov))
            .collect::<Result<Vec<_>, _>>()?;

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

    /// One lane's vardiff: its file table with that table's overrides applied.
    fn lane_vardiff(
        lane: &LaneSlot,
        ov: &BTreeMap<String, String>,
    ) -> Result<VardiffConfig, SettingsError> {
        let invalid = |msg: String| SettingsError::Invalid(msg);
        let path = &lane.vardiff_path;
        let mut vardiff = lane.file_vardiff.clone();
        let number = |field: &str, current: f64| -> Result<f64, SettingsError> {
            match ov.get(&Self::vardiff_key(lane, field)) {
                Some(raw) => raw
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .ok_or_else(|| invalid(format!("{path}.{field} must be a number"))),
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
        vardiff.validate_at(path).map_err(invalid)?;
        if vardiff.variance_percent < 0.0 {
            return Err(invalid(format!(
                "{path}.variance_percent must not be negative"
            )));
        }
        Ok(vardiff)
    }

    /// Whether `key` is a setting this configuration has, whether or not its lane has
    /// connected.
    fn known_key(&self, key: &str) -> bool {
        key == "pool.name"
            || key == "pool.coinbase_tag"
            || key == "log.level"
            || self
                .lanes
                .iter()
                .flat_map(|l| &l.coin_keys)
                .any(|c| key == Self::fallback_key(c))
            || self.lanes.iter().any(|lane| {
                VARDIFF_FIELDS
                    .iter()
                    .any(|f| key == Self::vardiff_key(lane, f))
            })
    }

    /// Push effective values to the running pool. At startup everything is sent; later
    /// only what changed, so watchers are not woken for nothing.
    fn apply_live(&self, e: &Effective, connected: &[Arc<ConnectedLane>], startup: bool) {
        if startup || self.state.pool_name() != e.pool_name {
            self.state.set_pool_name(&e.pool_name);
        }
        let tag = e.coinbase_tag.clone().into_bytes();
        if startup || *self.tag_tx.borrow() != tag {
            self.tag_tx.send_replace(tag);
        }
        for (lane, vardiff) in self.lanes.iter().zip(&e.vardiff) {
            if startup || *lane.vardiff_tx.borrow() != *vardiff {
                lane.vardiff_tx.send_replace(vardiff.clone());
            }
        }
        for lane in connected {
            let current: Vec<String> = {
                let set = lane.payouts_tx.borrow();
                std::iter::once(set.parent.fallback_address().to_string())
                    .chain(
                        set.aux
                            .iter()
                            .map(|a| a.table.fallback_address().to_string()),
                    )
                    .collect()
            };
            let wanted: Vec<&String> = lane.coins.iter().map(|c| &e.fallback[&c.key]).collect();
            if current.iter().ne(wanted.iter().copied()) {
                match Self::payout_set(&lane.coins, &e.fallback) {
                    Ok(set) => {
                        lane.payouts_tx.send_replace(Arc::new(set));
                    }
                    Err(err) => tracing::error!(%err, "payout tables not rebuilt"),
                }
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

    fn payout_set(
        coins: &[CoinRef],
        fallback: &BTreeMap<String, String>,
    ) -> Result<PayoutSet, String> {
        let table = |c: &CoinRef| {
            PayoutTable::new(c.coin.address_params(c.chain), &fallback[&c.key])
                .map_err(|e| format!("{}: {e}", c.key))
        };
        let (parent, aux) = coins.split_first().ok_or("no coins")?;
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

    /// Lane indexes with the primary first, then the rest in config order: the order of
    /// the document's `lanes` and `coins`, like the snapshot's.
    fn lane_order(&self) -> impl Iterator<Item = usize> + '_ {
        std::iter::once(self.primary).chain((0..self.lanes.len()).filter(|&i| i != self.primary))
    }

    fn vardiff_doc(lane: &LaneSlot, ov: &BTreeMap<String, String>) -> VardiffSettings {
        let number = |field: &str, file: f64| match ov
            .get(&Self::vardiff_key(lane, field))
            .and_then(|v| v.parse::<f64>().ok())
        {
            Some(v) => Setting::overridden(file, v),
            None => Setting::from_file(file),
        };
        let v = &lane.file_vardiff;
        VardiffSettings {
            initial_difficulty: number("initial_difficulty", v.initial_difficulty),
            min_difficulty: number("min_difficulty", v.min_difficulty),
            max_difficulty: number("max_difficulty", v.max_difficulty),
            target_share_seconds: number("target_share_seconds", v.target_share_seconds),
            retarget_seconds: number("retarget_seconds", v.retarget_seconds),
            variance_percent: number("variance_percent", v.variance_percent),
        }
    }

    /// The settings document. `coins` lists every configured coin, connected or not;
    /// a stored fallback address still waiting for its coin's node shows as overridden.
    fn doc(&self, ov: &BTreeMap<String, String>) -> SettingsDoc {
        let pending = self.pending.lock().expect("pending lock").clone();
        let text = |key: &str, file: &str| match ov.get(key).or_else(|| pending.get(key)) {
            Some(v) => Setting::overridden(file.to_string(), v.clone()),
            None => Setting::from_file(file.to_string()),
        };
        let connected = self.connected();
        let mut coins = Vec::new();
        for i in self.lane_order() {
            let lane = &self.lanes[i];
            let live = connected.iter().find(|c| c.lane == i);
            for (key, config) in lane.coin_keys.iter().zip(&lane.coin_configs) {
                let coin = live.and_then(|l| l.coins.iter().find(|c| &c.key == key));
                coins.push(CoinSettings {
                    key: key.clone(),
                    symbol: alamo_coins::builtin(key)
                        .map(|c| c.symbol().to_string())
                        .unwrap_or_else(|| key.to_ascii_uppercase()),
                    lane: lane.key.clone(),
                    connected: coin.is_some(),
                    chain: coin.map(|c| c.chain.to_string()),
                    merge_mined_with: config.merge_mined_with.clone(),
                    fallback_address: text(&Self::fallback_key(key), &config.fallback_address),
                    rpc_url: config.rpc_url.clone(),
                    rpc_user: config.rpc_user.clone(),
                    zmq_hashblock: config.zmq_hashblock.clone(),
                    poll_interval_ms: config.poll_interval_ms,
                    template_refresh_secs: config.template_refresh_secs,
                    template_stale_secs: config.template_stale_secs,
                });
            }
        }
        SettingsDoc {
            read_only: false,
            pool_name: text("pool.name", &self.file.pool_name),
            coinbase_tag: text("pool.coinbase_tag", &self.file.coinbase_tag),
            coinbase_tag_max_bytes: self.tag_budget(),
            coins,
            lanes: self
                .lane_order()
                .map(|i| {
                    let lane = &self.lanes[i];
                    LaneSettings {
                        key: lane.key.clone(),
                        stratum_listen: lane.stratum_listen.clone(),
                        own_stratum_section: lane.own_stratum,
                        vardiff: Self::vardiff_doc(lane, ov),
                    }
                })
                .collect(),
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
        let connected = self.connected();
        for (key, change) in &patch.fallback_addresses {
            if !self.known_key(&Self::fallback_key(key)) {
                return Err(SettingsError::Invalid(format!("unknown coin {key}")));
            }
            if !connected
                .iter()
                .flat_map(|l| &l.coins)
                .any(|c| c.key == *key)
            {
                return Err(SettingsError::Invalid(format!(
                    "coin {key} has not connected yet; its address cannot be checked"
                )));
            }
            set(Self::fallback_key(key), &Some(change.clone()));
        }
        // Two lanes on the default section write the same keys, so a patch may not give
        // one key two different values.
        let mut vardiff: Vec<(usize, &str, &VardiffPatch)> = Vec::new();
        for (key, lane_patch) in &patch.lanes {
            let lane = self
                .lane_index(key)
                .ok_or_else(|| SettingsError::Invalid(format!("unknown lane {key}")))?;
            vardiff.push((lane, key.as_str(), &lane_patch.vardiff));
        }
        let mut written: BTreeMap<String, (Option<String>, String)> = BTreeMap::new();
        for (lane, source, v) in vardiff {
            let lane = &self.lanes[lane];
            for (field, change) in [
                ("initial_difficulty", &v.initial_difficulty),
                ("min_difficulty", &v.min_difficulty),
                ("max_difficulty", &v.max_difficulty),
                ("target_share_seconds", &v.target_share_seconds),
                ("retarget_seconds", &v.retarget_seconds),
                ("variance_percent", &v.variance_percent),
            ] {
                let Some(change) = change else {
                    continue;
                };
                let as_text = change.map(|n| n.to_string());
                let key = Self::vardiff_key(lane, field);
                let origin = format!("lanes.{source}.vardiff.{field}");
                if let Some((earlier, from)) = written.get(&key) {
                    if *earlier != as_text {
                        return Err(SettingsError::Invalid(format!(
                            "{origin} and {from} both set {key} to different values"
                        )));
                    }
                }
                written.insert(key.clone(), (as_text.clone(), origin));
                set(key, &Some(as_text));
            }
        }
        Ok(next)
    }

    async fn apply_patch(&self, patch: SettingsPatch) -> Result<SettingsDoc, SettingsError> {
        let _guard = self.apply_lock.lock().await;
        let current = self.overrides.lock().expect("overrides lock").clone();
        let next = self.patched(&current, &patch)?;
        let connected = self.connected();
        let effective = self.validate(&next, &connected)?;
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
        self.apply_live(&effective, &connected, false);
        *self.overrides.lock().expect("overrides lock") = next.clone();
        Ok(self.doc(&next))
    }

    async fn probe(&self, key: &str) -> Result<NodeProbe, String> {
        let (coin_key, symbol, rpc) = self
            .connected()
            .iter()
            .flat_map(|l| &l.coins)
            .find(|c| c.key == key)
            .map(|c| (c.key.clone(), c.coin.symbol(), c.rpc.clone()))
            .ok_or_else(|| {
                if self
                    .lanes
                    .iter()
                    .any(|l| l.coin_keys.iter().any(|c| c == key))
                {
                    format!("coin {key} has not connected yet")
                } else {
                    format!("unknown coin {key}")
                }
            })?;
        let started = Instant::now();
        let chain: serde_json::Value = rpc
            .call("getblockchaininfo", &[])
            .await
            .map_err(|e| e.to_string())?;
        let network: serde_json::Value = rpc
            .call("getnetworkinfo", &[])
            .await
            .map_err(|e| e.to_string())?;
        let text = |v: &serde_json::Value, k: &str| v[k].as_str().unwrap_or_default().to_string();
        let int = |v: &serde_json::Value, k: &str| v[k].as_u64().unwrap_or_default();
        Ok(NodeProbe {
            key: coin_key,
            symbol: symbol.to_string(),
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
    use alamo_web::LanePatch;
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
    static DOGE_OTHER: LazyLock<String> = LazyLock::new(|| {
        encode_base58(Dogecoin.address_params(Chain::Main).p2pkh_prefix, &[4; 20])
    });

    /// A `lanes` patch changing one lane's vardiff.
    fn lane_vardiff(key: &str, vardiff: VardiffPatch) -> BTreeMap<String, LanePatch> {
        [(key.to_string(), LanePatch { vardiff })].into()
    }

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
                stratum: None,
            },
        }
    }

    /// LTC with DOGE merge-mined: one lane.
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

    /// One lane per parent, for the two-lane config.
    fn lane_chains(key: &str) -> Chains {
        let parent = match key {
            "ltc" => node("ltc", Arc::new(Litecoin), &LTC_FALLBACK, None),
            _ => node("doge", Arc::new(Dogecoin), &DOGE_FALLBACK, None),
        };
        Chains {
            parent,
            aux: vec![],
        }
    }

    const HEAD: &str = r#"
[pool]
name = "File Pool"
data_dir = "./data"
[stratum]
listen = "127.0.0.1:3333"
[web]
listen = "127.0.0.1:8080"
"#;

    fn ltc_section() -> String {
        format!(
            r#"
[coins.ltc]
rpc_url = "http://x"
rpc_user = "u"
rpc_password = "secret"
fallback_address = "{}"
coinbase_tag = "/file/"
"#,
            *LTC_FALLBACK
        )
    }

    /// LTC with DOGE merge-mined, as the example config has it.
    fn config() -> Config {
        let text = format!(
            r#"{HEAD}{}
[coins.doge]
merge_mined_with = "ltc"
rpc_url = "http://y"
rpc_user = "u"
rpc_password = "secret"
fallback_address = "{}"
"#,
            ltc_section(),
            *DOGE_FALLBACK
        );
        Config::parse(&text).unwrap()
    }

    /// Two lanes: LTC on the default `[stratum]`, DOGE as a parent with its own section.
    fn two_lane_config() -> Config {
        let text = format!(
            r#"{HEAD}{}
[coins.doge]
rpc_url = "http://y"
rpc_user = "u"
rpc_password = "secret"
fallback_address = "{}"
[coins.doge.stratum]
listen = "127.0.0.1:3334"
[coins.doge.stratum.vardiff]
initial_difficulty = 8
min_difficulty = 1
max_difficulty = 64
target_share_seconds = 5
retarget_seconds = 30
variance_percent = 20
"#,
            ltc_section(),
            *DOGE_FALLBACK
        );
        Config::parse(&text).unwrap()
    }

    struct Harness {
        live: Arc<LiveSettings>,
        payouts: watch::Receiver<Arc<PayoutSet>>,
        state: AppState,
        store: Store,
        filters: Arc<StdMutex<Vec<String>>>,
        path: std::path::PathBuf,
    }

    async fn temp_store(tag: &str) -> (Store, std::path::PathBuf) {
        let path = alamo_store::temp_path(&format!("settings-{tag}"));
        (Store::open(&path).await.unwrap(), path)
    }

    fn log_setup() -> (LogSetup, Arc<StdMutex<Vec<String>>>) {
        let filters = Arc::new(StdMutex::new(Vec::new()));
        let seen = filters.clone();
        let log = LogSetup {
            filter: "info".into(),
            control: Arc::new(move |d: &str| {
                seen.lock().unwrap().push(d.to_string());
                Ok(())
            }),
        };
        (log, filters)
    }

    async fn harness(tag: &str) -> Harness {
        let (store, path) = temp_store(tag).await;
        let state = AppState::new("File Pool", 3333, store.clone());
        let (log, filters) = log_setup();
        let live = LiveSettings::start(&config(), store.clone(), state.clone(), log)
            .await
            .unwrap();
        let payouts = live.lane_connected(&chains()).await.unwrap();
        Harness {
            live,
            payouts,
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
        assert_eq!(doc.lanes[0].key, "ltc");
        assert_eq!(doc.lanes[0].vardiff.min_difficulty.value, 1024.0);
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
                lanes: lane_vardiff(
                    "ltc",
                    VardiffPatch {
                        min_difficulty: Some(Some(2048.0)),
                        ..Default::default()
                    },
                ),
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
        assert_eq!(
            h.live.vardiff("ltc").unwrap().borrow().min_difficulty,
            2048.0
        );
        assert_eq!(
            h.payouts.borrow().parent.fallback_address(),
            LTC_OTHER.as_str()
        );
        assert_eq!(
            h.payouts.borrow().aux[0].table.fallback_address(),
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
                    lanes: lane_vardiff(
                        "ltc",
                        VardiffPatch {
                            min_difficulty: Some(Some(1e9)),
                            ..Default::default()
                        },
                    ),
                    ..Default::default()
                },
                "stratum.vardiff.max_difficulty must be >= min_difficulty",
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
        let (log, filters) = log_setup();
        let state = AppState::new("File Pool", 3333, h.store.clone());
        let live = LiveSettings::start(&config(), h.store.clone(), state.clone(), log)
            .await
            .unwrap();
        assert_eq!(state.pool_name(), "Stored");
        assert!(filters.lock().unwrap()[0].starts_with("trace,"));
        let stored = h.store.load_settings().await.unwrap();
        assert_eq!(
            stored.keys().collect::<Vec<_>>(),
            vec!["coins.doge.fallback_address", "log.level", "pool.name"],
            "the unknown key is gone; the address waits for its node"
        );

        live.lane_connected(&chains()).await.unwrap();
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

    #[tokio::test]
    async fn vardiff_overrides_are_keyed_by_the_table_they_override() {
        let (store, path) = temp_store("lanes-vardiff").await;
        store
            .set_setting("stratum.vardiff.min_difficulty", "2048", 1)
            .await
            .unwrap();
        store
            .set_setting("coins.doge.stratum.vardiff.min_difficulty", "2", 1)
            .await
            .unwrap();
        // LTC uses the default section, so this key overrides nothing in this config.
        store
            .set_setting("coins.ltc.stratum.vardiff.min_difficulty", "5", 1)
            .await
            .unwrap();
        let state = AppState::new("File Pool", 3333, store.clone());
        let (log, _) = log_setup();
        let live = LiveSettings::start(&two_lane_config(), store.clone(), state, log)
            .await
            .unwrap();
        let (ltc, doge) = (live.vardiff("ltc").unwrap(), live.vardiff("doge").unwrap());
        assert_eq!(ltc.borrow().min_difficulty, 2048.0);
        assert_eq!(ltc.borrow().initial_difficulty, 65_536.0);
        assert_eq!(doge.borrow().min_difficulty, 2.0);
        assert_eq!(
            doge.borrow().initial_difficulty,
            8.0,
            "doge's own file table"
        );
        assert!(live.vardiff("btc").is_none());
        let stored = store.load_settings().await.unwrap();
        assert!(!stored.contains_key("coins.ltc.stratum.vardiff.min_difficulty"));
        assert_eq!(stored.len(), 2);

        // LTC's document and patch go to the default [stratum] table it uses.
        let doc = live.settings();
        assert_eq!(doc.lanes[0].key, "ltc");
        assert_eq!(
            doc.lanes[0].vardiff.min_difficulty,
            Setting::overridden(1024.0, 2048.0)
        );
        live.apply(SettingsPatch {
            lanes: lane_vardiff(
                "ltc",
                VardiffPatch {
                    min_difficulty: Some(Some(4096.0)),
                    ..Default::default()
                },
            ),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(ltc.borrow().min_difficulty, 4096.0);
        assert_eq!(doge.borrow().min_difficulty, 2.0, "other lanes untouched");
        let stored = store.load_settings().await.unwrap();
        assert_eq!(stored["stratum.vardiff.min_difficulty"], "4096");
        assert_eq!(stored["coins.doge.stratum.vardiff.min_difficulty"], "2");

        // A stored value that breaks a lane's table is dropped with the lane named.
        store
            .set_setting("coins.doge.stratum.vardiff.max_difficulty", "0.5", 1)
            .await
            .unwrap();
        let state = AppState::new("File Pool", 3333, store.clone());
        let (log, _) = log_setup();
        let live = LiveSettings::start(&two_lane_config(), store.clone(), state, log)
            .await
            .unwrap();
        assert_eq!(live.vardiff("doge").unwrap().borrow().max_difficulty, 64.0);
        let err = live
            .validate(
                &[(
                    "coins.doge.stratum.vardiff.max_difficulty".to_string(),
                    "0.5".to_string(),
                )]
                .into(),
                &[],
            )
            .err()
            .unwrap()
            .to_string();
        assert!(
            err.contains("coins.doge.stratum.vardiff.max_difficulty must be >= min_difficulty"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn tag_budget_comes_from_the_config_alone() {
        let (store, path) = temp_store("tag-budget").await;
        let start = |config: Config| {
            let store = store.clone();
            async move {
                let state = AppState::new("File Pool", 3333, store.clone());
                let (log, _) = log_setup();
                LiveSettings::start(&config, store, state, log)
                    .await
                    .unwrap()
            }
        };
        // Merge mining costs the commitment, even before any node has answered.
        let merged = start(config()).await;
        assert_eq!(merged.tag_budget(), 41);
        assert_eq!(merged.settings().coinbase_tag_max_bytes, 41);
        assert!(
            merged
                .settings()
                .coins
                .iter()
                .all(|c| !c.connected && c.chain.is_none()),
            "nothing connected yet"
        );
        let err = merged
            .apply(SettingsPatch {
                coinbase_tag: Some(Some("x".repeat(42))),
                ..Default::default()
            })
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("room for 41"), "{err}");

        // No lane merge-mines: the commitment's bytes are free.
        let plain = start(two_lane_config()).await;
        assert_eq!(plain.tag_budget(), 41 + 1 + COMMITMENT_LEN);

        // With both built-in coins as parents neither lane merge-mines. A merge-mining lane
        // beside a plain one needs a third coin; `tag_budget_for` takes the tightest
        // (`any` lane merging) and is all the mixed case would add.
        assert_eq!(tag_budget_for(true), 41);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn fallback_overrides_wait_for_their_lane() {
        let (store, path) = temp_store("pending").await;
        store
            .set_setting("coins.doge.fallback_address", &DOGE_OTHER, 1)
            .await
            .unwrap();
        store
            .set_setting("coins.ltc.fallback_address", "not-an-address", 1)
            .await
            .unwrap();
        // No such coin in this configuration: dropped at startup.
        store
            .set_setting("coins.btc.fallback_address", "bc1q", 1)
            .await
            .unwrap();
        let state = AppState::new("File Pool", 3333, store.clone());
        let (log, _) = log_setup();
        let live = LiveSettings::start(&two_lane_config(), store.clone(), state, log)
            .await
            .unwrap();
        let keys = |stored: BTreeMap<String, String>| stored.into_keys().collect::<Vec<_>>();
        assert_eq!(
            keys(store.load_settings().await.unwrap()),
            vec!["coins.doge.fallback_address", "coins.ltc.fallback_address"],
            "kept while their lanes are down"
        );
        // Every configured coin is listed, unconnected, with its waiting address shown.
        let doc = live.settings();
        assert_eq!(
            doc.coins
                .iter()
                .map(|c| (c.key.as_str(), c.lane.as_str(), c.connected))
                .collect::<Vec<_>>(),
            vec![("ltc", "ltc", false), ("doge", "doge", false)],
            "primary lane first"
        );
        assert_eq!(
            doc.coins[1].fallback_address,
            Setting::overridden(DOGE_FALLBACK.clone(), DOGE_OTHER.clone())
        );
        assert!(doc.coins[1].chain.is_none());
        let err = live
            .apply(SettingsPatch {
                fallback_addresses: [("doge".to_string(), Some(DOGE_FALLBACK.clone()))].into(),
                ..Default::default()
            })
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("coin doge has not connected yet"), "{err}");
        // An unrelated patch while they wait leaves them alone.
        live.apply(SettingsPatch {
            pool_name: Some(Some("Renamed".into())),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(store.load_settings().await.unwrap().len(), 3);

        // DOGE connects: its stored address is checked against its network and applied.
        let doge = live.lane_connected(&lane_chains("doge")).await.unwrap();
        assert_eq!(doge.borrow().parent.fallback_address(), DOGE_OTHER.as_str());
        let doc = live.settings();
        assert!(doc.coins[1].connected);
        assert_eq!(doc.coins[1].chain.as_deref(), Some("main"));
        assert!(!doc.coins[0].connected);
        assert_eq!(
            doc.coins[1].fallback_address,
            Setting::overridden(DOGE_FALLBACK.clone(), DOGE_OTHER.clone())
        );
        assert!(store
            .load_settings()
            .await
            .unwrap()
            .contains_key("coins.ltc.fallback_address"));

        // LTC connects: only now is its invalid address found out and dropped.
        let ltc = live.lane_connected(&lane_chains("ltc")).await.unwrap();
        assert_eq!(
            ltc.borrow().parent.fallback_address(),
            LTC_FALLBACK.as_str()
        );
        assert_eq!(
            keys(store.load_settings().await.unwrap()),
            vec!["coins.doge.fallback_address", "pool.name"]
        );
        let doc = live.settings();
        assert!(doc.coins.iter().all(|c| c.connected));
        assert!(!doc.coins[0].fallback_address.overridden);
        assert_eq!(doge.borrow().parent.fallback_address(), DOGE_OTHER.as_str());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn lanes_in_the_document_and_per_lane_vardiff_patches() {
        let (store, path) = temp_store("lane-patches").await;
        let state = AppState::new("File Pool", 3333, store.clone());
        let (log, _) = log_setup();
        let live = LiveSettings::start(&two_lane_config(), store.clone(), state, log)
            .await
            .unwrap();
        let doc = live.settings();
        assert_eq!(
            doc.lanes
                .iter()
                .map(|l| (
                    l.key.as_str(),
                    l.stratum_listen.as_str(),
                    l.own_stratum_section
                ))
                .collect::<Vec<_>>(),
            vec![
                ("ltc", "127.0.0.1:3333", false),
                ("doge", "127.0.0.1:3334", true)
            ]
        );
        assert_eq!(doc.lanes[1].vardiff.initial_difficulty.value, 8.0);
        assert_eq!(doc.lanes[0].vardiff.min_difficulty.value, 1024.0);

        // A lane by name writes its own table's keys.
        let doc = live
            .apply(SettingsPatch {
                lanes: [(
                    "doge".to_string(),
                    LanePatch {
                        vardiff: VardiffPatch {
                            min_difficulty: Some(Some(2.0)),
                            ..Default::default()
                        },
                    },
                )]
                .into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            doc.lanes[1].vardiff.min_difficulty,
            Setting::overridden(1.0, 2.0)
        );
        assert!(!doc.lanes[0].vardiff.min_difficulty.overridden);
        assert_eq!(live.vardiff("doge").unwrap().borrow().min_difficulty, 2.0);
        assert_eq!(live.vardiff("ltc").unwrap().borrow().min_difficulty, 1024.0);
        let stored = store.load_settings().await.unwrap();
        assert_eq!(stored["coins.doge.stratum.vardiff.min_difficulty"], "2");

        let err = live
            .apply(SettingsPatch {
                lanes: [("btc".to_string(), LanePatch::default())].into(),
                ..Default::default()
            })
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown lane btc"), "{err}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn lanes_sharing_the_default_section_share_its_overrides() {
        // Both parents on the default [stratum] section, which validates only with port 0.
        let text = format!(
            r#"
[pool]
name = "File Pool"
data_dir = "./data"
[stratum]
listen = "127.0.0.1:0"
[web]
listen = "127.0.0.1:8080"
{}
[coins.doge]
rpc_url = "http://y"
rpc_user = "u"
rpc_password = "secret"
fallback_address = "{}"
"#,
            ltc_section(),
            *DOGE_FALLBACK
        );
        let config = Config::parse(&text).unwrap();
        let (store, path) = temp_store("shared-section").await;
        let state = AppState::new("File Pool", 0, store.clone());
        let (log, _) = log_setup();
        let live = LiveSettings::start(&config, store.clone(), state, log)
            .await
            .unwrap();
        let doc = live
            .apply(SettingsPatch {
                lanes: [(
                    "ltc".to_string(),
                    LanePatch {
                        vardiff: VardiffPatch {
                            max_difficulty: Some(Some(1.0e6)),
                            ..Default::default()
                        },
                    },
                )]
                .into(),
                ..Default::default()
            })
            .await
            .unwrap();
        // One stored key, and every lane on [stratum] follows it.
        assert_eq!(store.load_settings().await.unwrap().len(), 1);
        assert!(doc.lanes.iter().all(|l| !l.own_stratum_section
            && l.vardiff.max_difficulty == Setting::overridden(16_777_216.0, 1.0e6)));
        assert_eq!(live.vardiff("doge").unwrap().borrow().max_difficulty, 1.0e6);
        assert_eq!(live.vardiff("ltc").unwrap().borrow().max_difficulty, 1.0e6);

        // Both lanes name the same key; they may repeat a value but not contradict it.
        let both = |ltc: f64, doge: f64| SettingsPatch {
            lanes: [
                (
                    "ltc".to_string(),
                    LanePatch {
                        vardiff: VardiffPatch {
                            min_difficulty: Some(Some(ltc)),
                            ..Default::default()
                        },
                    },
                ),
                (
                    "doge".to_string(),
                    LanePatch {
                        vardiff: VardiffPatch {
                            min_difficulty: Some(Some(doge)),
                            ..Default::default()
                        },
                    },
                ),
            ]
            .into(),
            ..Default::default()
        };
        live.apply(both(2048.0, 2048.0)).await.unwrap();
        assert_eq!(live.vardiff("ltc").unwrap().borrow().min_difficulty, 2048.0);
        assert_eq!(
            live.vardiff("doge").unwrap().borrow().min_difficulty,
            2048.0
        );
        let err = live
            .apply(both(1.0e3, 2.0e3))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("both set stratum.vardiff.min_difficulty to different values"),
            "{err}"
        );
        assert_eq!(live.vardiff("ltc").unwrap().borrow().min_difficulty, 2048.0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn a_detached_lane_puts_its_addresses_back_to_waiting() {
        let (store, path) = temp_store("detach").await;
        store
            .set_setting("coins.doge.fallback_address", &DOGE_OTHER, 1)
            .await
            .unwrap();
        let state = AppState::new("File Pool", 3333, store.clone());
        let (log, _) = log_setup();
        let live = LiveSettings::start(&two_lane_config(), store.clone(), state, log)
            .await
            .unwrap();
        live.lane_connected(&lane_chains("doge")).await.unwrap();
        assert!(live.settings().coins[1].connected);
        live.lane_detached("doge").await;
        let doc = live.settings();
        assert!(!doc.coins[1].connected);
        assert!(doc.coins[1].fallback_address.overridden, "still stored");
        // Other changes still validate with the lane gone.
        live.apply(SettingsPatch {
            pool_name: Some(Some("After".into())),
            ..Default::default()
        })
        .await
        .unwrap();
        assert!(store
            .load_settings()
            .await
            .unwrap()
            .contains_key("coins.doge.fallback_address"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
