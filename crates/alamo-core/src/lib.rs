//! Pure computation for the Alamo mining pool.
//!
//! This crate has no I/O and no async. It holds the byte-exact code (hashing, headers,
//! targets) and the math behind the dashboard (block odds). Everything here should be
//! covered by known-answer tests against real chain data.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod algo;
pub mod hash;
pub mod header;
pub mod job;
pub mod odds;
pub mod target;

pub use algo::Algorithm;
pub use hash::Hash256;
pub use header::BlockHeader;
pub use job::{JobId, RejectReason, Share, ShareOutcome};
pub use target::Target;
