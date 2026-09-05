//! Stratum v1 server.
//!
//! One tokio task per connection, line-delimited JSON-RPC. Wave 0 accepts connections and
//! parses messages; job distribution, share validation, and vardiff arrive in Wave 1.

#![forbid(unsafe_code)]

pub mod config;
pub mod protocol;
pub mod server;

pub use config::{StratumConfig, VardiffConfig};
pub use server::serve;
