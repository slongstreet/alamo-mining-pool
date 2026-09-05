//! Stratum v1 server.
//!
//! One tokio task per connection, line-delimited JSON-RPC. Each session derives its own
//! jobs from the shared [`WorkTemplate`](alamo_core::WorkTemplate) so that the coinbase
//! pays the address the worker connected with.

#![forbid(unsafe_code)]

pub mod config;
pub mod events;
pub mod job;
pub mod protocol;
pub mod server;
pub mod session;
pub mod validate;
pub mod vardiff;

pub use config::{StratumConfig, VardiffConfig};
pub use events::{BlockCandidate, PoolEvent};
pub use server::{StratumServer, WorkReceiver};
