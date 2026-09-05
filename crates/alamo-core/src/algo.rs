//! Proof-of-work algorithms.

use crate::hash::{scrypt_pow, sha256d, Hash256};
use serde::{Deserialize, Serialize};

/// The proof-of-work algorithm a coin uses to hash block headers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Algorithm {
    /// scrypt(1024, 1, 1). Litecoin, Dogecoin.
    Scrypt,
    /// Double SHA-256. Bitcoin, Bitcoin Cash.
    Sha256d,
}

impl Algorithm {
    /// Compute the proof-of-work hash of a serialized 80-byte header.
    pub fn pow_hash(self, header: &[u8]) -> Hash256 {
        match self {
            Algorithm::Scrypt => scrypt_pow(header),
            Algorithm::Sha256d => sha256d(header),
        }
    }

    /// Human-readable name.
    pub fn name(self) -> &'static str {
        match self {
            Algorithm::Scrypt => "scrypt",
            Algorithm::Sha256d => "sha256d",
        }
    }
}
