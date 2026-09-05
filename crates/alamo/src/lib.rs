//! Wiring for the `alamo` daemon: configuration, statistics, and the pool runtime.

#![forbid(unsafe_code)]

pub mod config;
pub mod persist;
pub mod pool;
pub mod stats;

pub use config::Config;
