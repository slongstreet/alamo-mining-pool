//! Per-coin configuration.

use serde::{Deserialize, Serialize};

/// Configuration for one coin's node connection and payout behavior.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoinConfig {
    /// Whether the pool mines this coin.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Config key of the parent coin, if this coin is merge-mined.
    #[serde(default)]
    pub merge_mined_with: Option<String>,
    /// JSON-RPC endpoint of the coin's node.
    pub rpc_url: String,
    /// RPC username.
    pub rpc_user: String,
    /// RPC password. Never logged.
    pub rpc_password: String,
    /// Optional ZMQ `hashblock` endpoint for new-block notifications.
    #[serde(default)]
    pub zmq_hashblock: Option<String>,
    /// Address paid when a worker does not supply a valid one.
    pub fallback_address: String,
    /// Text placed in the coinbase scriptSig. Parent chains only.
    #[serde(default)]
    pub coinbase_tag: Option<String>,
    /// How often to check the node for a new chain tip, in milliseconds.
    #[serde(default = "default_poll_ms")]
    pub poll_interval_ms: u64,
    /// How often to refresh the template without a new tip, in seconds.
    #[serde(default = "default_refresh_secs")]
    pub template_refresh_secs: u64,
}

fn default_true() -> bool {
    true
}

fn default_poll_ms() -> u64 {
    500
}

fn default_refresh_secs() -> u64 {
    30
}

impl std::fmt::Display for CoinConfig {
    /// Redacted display: never shows the RPC password.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "rpc_url={} rpc_user={} enabled={}",
            self.rpc_url, self.rpc_user, self.enabled
        )
    }
}
