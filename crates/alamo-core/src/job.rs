//! Job and share types shared between the stratum server and the validator.

use crate::hash::Hash256;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Identifier of a mining job handed to workers via `mining.notify`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct JobId(pub u64);

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:x}", self.0)
    }
}

impl std::str::FromStr for JobId {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        u64::from_str_radix(s, 16).map(JobId)
    }
}

/// A share submitted by a worker via `mining.submit`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Share {
    /// The job the share was mined against.
    pub job_id: JobId,
    /// Worker name as authorized (payout address plus optional worker suffix).
    pub worker: String,
    /// Worker-chosen extranonce2 bytes.
    pub extranonce2: Vec<u8>,
    /// Header timestamp the worker used.
    pub ntime: u32,
    /// Header nonce the worker found.
    pub nonce: u32,
}

/// Why a share was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    /// The job is no longer current.
    StaleJob,
    /// The job id is unknown to this session.
    UnknownJob,
    /// The hash did not meet the session's share target.
    LowDifficulty,
    /// The same share was already submitted.
    Duplicate,
    /// The ntime is outside the allowed window.
    InvalidNtime,
    /// The extranonce2 has the wrong length.
    InvalidExtranonce2,
    /// The worker has not been authorized.
    Unauthorized,
}

impl RejectReason {
    /// The stratum error code conventionally sent for this rejection.
    pub fn stratum_code(self) -> i32 {
        match self {
            RejectReason::StaleJob | RejectReason::UnknownJob => 21,
            RejectReason::Duplicate => 22,
            RejectReason::LowDifficulty => 23,
            RejectReason::Unauthorized => 24,
            RejectReason::InvalidNtime | RejectReason::InvalidExtranonce2 => 20,
        }
    }

    /// The stratum error message conventionally sent for this rejection.
    pub fn message(self) -> &'static str {
        match self {
            RejectReason::StaleJob => "Stale share",
            RejectReason::UnknownJob => "Job not found",
            RejectReason::LowDifficulty => "Low difficulty share",
            RejectReason::Duplicate => "Duplicate share",
            RejectReason::InvalidNtime => "Invalid ntime",
            RejectReason::InvalidExtranonce2 => "Invalid extranonce2",
            RejectReason::Unauthorized => "Unauthorized worker",
        }
    }
}

/// The result of validating a share.
#[derive(Clone, Debug, PartialEq)]
pub enum ShareOutcome {
    /// The share met the session target.
    Accepted {
        /// Difficulty actually achieved by the hash.
        difficulty: f64,
    },
    /// The share met one or more network targets and produced block candidates.
    Block {
        /// Difficulty actually achieved by the hash.
        difficulty: f64,
        /// Proof-of-work hash of the header.
        pow_hash: Hash256,
        /// Symbols of the chains whose targets were met (e.g. `["LTC", "DOGE"]`).
        chains: Vec<&'static str>,
    },
    /// The share was rejected.
    Rejected(RejectReason),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_id_hex_round_trip() {
        let id = JobId(0xdead_beef);
        assert_eq!(id.to_string(), "deadbeef");
        assert_eq!("deadbeef".parse::<JobId>().unwrap(), id);
    }
}
