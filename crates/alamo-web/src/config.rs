//! Web server configuration.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

/// HTTP listener settings.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    /// Address to listen on.
    pub listen: SocketAddr,
}
