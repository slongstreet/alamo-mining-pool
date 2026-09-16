//! Operator settings: the document the settings page shows, the patch it sends, and the
//! hook the pool installs to validate and apply changes.
//!
//! Every setting has a value from the config file and may carry an override stored in
//! the database. The page shows the effective value, whether it is an override, and the
//! file value it would revert to. Node endpoints and credentials are shown but never
//! editable here: the API has no authentication of its own.

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// A boxed future, so the operator hook can be a trait object.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Changes the process-wide log filter. Returns the reason when the directive is invalid.
pub type LogControl = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// One setting: its effective value and where it came from.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Setting<T> {
    /// The value in force.
    pub value: T,
    /// The value in the config file (or the built-in default), used when reverting.
    pub file_value: T,
    /// Whether `value` is an override stored in the database.
    pub overridden: bool,
}

impl<T: Clone> Setting<T> {
    /// A setting taken straight from the file.
    pub fn from_file(value: T) -> Self {
        Self {
            file_value: value.clone(),
            value,
            overridden: false,
        }
    }

    /// A setting with a stored override in force.
    pub fn overridden(file_value: T, value: T) -> Self {
        Self {
            value,
            file_value,
            overridden: true,
        }
    }
}

/// What the settings page shows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SettingsDoc {
    /// Whether the daemon refuses every change (`[web] read_only = true`).
    pub read_only: bool,
    /// Display name on the dashboard.
    pub pool_name: Setting<String>,
    /// Text placed in every coinbase scriptSig.
    pub coinbase_tag: Setting<String>,
    /// Longest tag the coinbase can carry, in bytes, given the chains being mined.
    pub coinbase_tag_max_bytes: usize,
    /// Each configured coin, parent first.
    pub coins: Vec<CoinSettings>,
    /// Variable-difficulty settings, in the units miners display.
    pub vardiff: VardiffSettings,
    /// Log filter directive (`info`, `debug`, ...).
    pub log_level: Setting<String>,
    /// The effective configuration as TOML with secrets redacted.
    pub config_toml: String,
}

/// One coin's settings and node details.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CoinSettings {
    /// Config key (`ltc`, `doge`).
    pub key: String,
    /// Ticker (`LTC`).
    pub symbol: String,
    /// Network the node is on (`main`, `test`, `regtest`).
    pub chain: String,
    /// The parent coin's key when this coin is merge-mined.
    pub merge_mined_with: Option<String>,
    /// Address paid when a miner supplies none that is valid.
    pub fallback_address: Setting<String>,
    /// Node RPC endpoint.
    pub rpc_url: String,
    /// Node RPC user. The password is never reported.
    pub rpc_user: String,
    /// ZMQ `hashblock` endpoint, if configured.
    pub zmq_hashblock: Option<String>,
    /// Tip polling interval in milliseconds.
    pub poll_interval_ms: u64,
    /// Template refresh interval in seconds.
    pub template_refresh_secs: u64,
    /// Seconds a node may stay unreachable before its template is withdrawn.
    pub template_stale_secs: u64,
}

/// Variable-difficulty settings, one [`Setting`] each.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VardiffSettings {
    /// Difficulty a new connection starts at.
    pub initial_difficulty: Setting<f64>,
    /// Lowest difficulty vardiff will assign.
    pub min_difficulty: Setting<f64>,
    /// Highest difficulty vardiff will assign.
    pub max_difficulty: Setting<f64>,
    /// Desired seconds between shares from one worker.
    pub target_share_seconds: Setting<f64>,
    /// Minimum seconds between difficulty changes.
    pub retarget_seconds: Setting<f64>,
    /// Retarget only when the estimate is off by more than this percentage.
    pub variance_percent: Setting<f64>,
}

/// A change request. Every field is optional; a field set to `null` reverts that setting
/// to the config file's value, and a value stores an override.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct SettingsPatch {
    /// Display name.
    #[serde(default, deserialize_with = "double_option")]
    pub pool_name: Option<Option<String>>,
    /// Coinbase tag.
    #[serde(default, deserialize_with = "double_option")]
    pub coinbase_tag: Option<Option<String>>,
    /// Fallback address per coin key.
    #[serde(default)]
    pub fallback_addresses: BTreeMap<String, Option<String>>,
    /// Vardiff fields.
    #[serde(default)]
    pub vardiff: VardiffPatch,
    /// Log filter directive.
    #[serde(default, deserialize_with = "double_option")]
    pub log_level: Option<Option<String>>,
}

/// Vardiff part of a [`SettingsPatch`].
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct VardiffPatch {
    /// See [`VardiffSettings::initial_difficulty`].
    #[serde(default, deserialize_with = "double_option")]
    pub initial_difficulty: Option<Option<f64>>,
    /// See [`VardiffSettings::min_difficulty`].
    #[serde(default, deserialize_with = "double_option")]
    pub min_difficulty: Option<Option<f64>>,
    /// See [`VardiffSettings::max_difficulty`].
    #[serde(default, deserialize_with = "double_option")]
    pub max_difficulty: Option<Option<f64>>,
    /// See [`VardiffSettings::target_share_seconds`].
    #[serde(default, deserialize_with = "double_option")]
    pub target_share_seconds: Option<Option<f64>>,
    /// See [`VardiffSettings::retarget_seconds`].
    #[serde(default, deserialize_with = "double_option")]
    pub retarget_seconds: Option<Option<f64>>,
    /// See [`VardiffSettings::variance_percent`].
    #[serde(default, deserialize_with = "double_option")]
    pub variance_percent: Option<Option<f64>>,
}

impl SettingsPatch {
    /// Whether the patch changes nothing.
    pub fn is_empty(&self) -> bool {
        self.pool_name.is_none()
            && self.coinbase_tag.is_none()
            && self.fallback_addresses.is_empty()
            && self.log_level.is_none()
            && self.vardiff == VardiffPatch::default()
    }
}

/// Distinguish an absent field (`None`) from an explicit `null` (`Some(None)`).
fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// Result of asking a node who it is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeProbe {
    /// Config key.
    pub key: String,
    /// Ticker.
    pub symbol: String,
    /// Network the node reports.
    pub chain: String,
    /// Chain height the node reports.
    pub height: u64,
    /// Node software as it identifies itself (`/Shibetoshi:1.14.9/`).
    pub subversion: String,
    /// Protocol version.
    pub protocol_version: u64,
    /// Peers the node is connected to.
    pub connections: u64,
    /// Whether the node is still syncing.
    pub initial_block_download: bool,
    /// Round-trip time of the probe in milliseconds.
    pub latency_ms: u64,
}

/// Why a change was refused.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// The patch is malformed or a value does not validate. Reported as 400.
    #[error("{0}")]
    Invalid(String),
    /// The change could not be stored. Reported as 500.
    #[error("could not store setting: {0}")]
    Store(String),
}

/// The pool's side of the settings page: installed into the app state once the pool is
/// connected to its nodes.
pub trait Operator: Send + Sync {
    /// The current document.
    fn settings(&self) -> SettingsDoc;
    /// Validate, store, and apply a change, then return the new document.
    fn apply(&self, patch: SettingsPatch) -> BoxFuture<'_, Result<SettingsDoc, SettingsError>>;
    /// Ask one coin's node who it is. `Err` carries the node's or the transport's reason.
    fn probe_node(&self, key: &str) -> BoxFuture<'_, Result<NodeProbe, String>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_distinguishes_absent_null_and_value() {
        let p: SettingsPatch = serde_json::from_str("{}").unwrap();
        assert!(p.is_empty());
        let p: SettingsPatch =
            serde_json::from_str(r#"{"pool_name": null, "coinbase_tag": "/x/"}"#).unwrap();
        assert_eq!(p.pool_name, Some(None));
        assert_eq!(p.coinbase_tag, Some(Some("/x/".into())));
        assert!(!p.is_empty());
        let p: SettingsPatch = serde_json::from_str(
            r#"{"fallback_addresses": {"ltc": null, "doge": "D1"}, "vardiff": {"min_difficulty": 2048}}"#,
        )
        .unwrap();
        assert_eq!(p.fallback_addresses["ltc"], None);
        assert_eq!(p.fallback_addresses["doge"].as_deref(), Some("D1"));
        assert_eq!(p.vardiff.min_difficulty, Some(Some(2048.0)));
        assert_eq!(p.vardiff.max_difficulty, None);
    }
}
