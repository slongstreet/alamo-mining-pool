//! Web server configuration.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

/// HTTP listener settings.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    /// Address to listen on.
    pub listen: SocketAddr,
    /// Refuse every change from the dashboard: settings, share-count reset, worker
    /// removal. For deployments where the dashboard is reachable by people who should
    /// not operate the pool; the API has no authentication of its own.
    #[serde(default)]
    pub read_only: bool,
}
