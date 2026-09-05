//! Pure computation for the Alamo mining pool.
//!
//! This crate has no I/O and no async. It holds the byte-exact code (hashing, headers,
//! targets) and the math behind the dashboard (block odds). Everything here should be
//! covered by known-answer tests against real chain data.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod address;
pub mod algo;
pub mod coinbase;
pub mod encode;
pub mod hash;
pub mod header;
pub mod job;
pub mod merkle;
pub mod odds;
pub mod payout;
pub mod target;
pub mod time;
pub mod work;

pub use address::AddressParams;
pub use algo::Algorithm;
pub use coinbase::CoinbaseParts;
pub use hash::Hash256;
pub use header::BlockHeader;
pub use job::{JobId, RejectReason, ShareOutcome};
pub use payout::{Payout, PayoutTable};
pub use target::Target;
pub use work::{TemplateTx, WorkTemplate};
