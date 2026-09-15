//! Proof-of-work algorithms.

use crate::hash::{scrypt_pow, sha256d, Hash256};
use crate::target::{hash_difficulty, Target};
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

    /// How much easier a stratum share of difficulty 1 is than [`Target::DIFF1`].
    ///
    /// Scrypt miners and pools inherited a 2^16 factor from cgminer, whose `set_target`
    /// multiplies the difficulty-1 target by 65536 when scrypt is enabled; node-stratum-pool
    /// carries the same `multiplier: Math.pow(2, 16)` for scrypt. So a scrypt share at
    /// stratum difficulty 65536 is exactly one unit of network difficulty. SHA-256d has no
    /// such factor. Getting this wrong rejects every share a real scrypt ASIC sends.
    pub fn share_multiplier(self) -> f64 {
        match self {
            Algorithm::Scrypt => 65536.0,
            Algorithm::Sha256d => 1.0,
        }
    }

    /// The target a share must meet at `difficulty` under this algorithm's stratum
    /// convention.
    pub fn share_target(self, difficulty: f64) -> Target {
        Target::from_difficulty(difficulty / self.share_multiplier())
    }

    /// The stratum share difficulty a proof-of-work hash achieves, comparable to the
    /// difficulty sent in `mining.set_difficulty`.
    pub fn share_difficulty(self, hash: &Hash256) -> f64 {
        hash_difficulty(hash) * self.share_multiplier()
    }

    /// Work represented by a share of stratum `difficulty`, in network difficulty-1 units
    /// (2^32 hashes each). This is the unit network difficulty, round progress, luck, and
    /// hashrate are computed in.
    pub fn share_work(self, difficulty: f64) -> f64 {
        difficulty / self.share_multiplier()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrypt_share_diff1_matches_cgminer() {
        // cgminer 3.7.2 set_target(): d64 = truediffone; if (opt_scrypt) d64 *= 65536;
        // d64 /= diff. node-stratum-pool algoProperties.js: scrypt multiplier 2^16.
        assert_eq!(
            Algorithm::Scrypt.share_target(1.0).to_string(),
            "0000ffff00000000000000000000000000000000000000000000000000000000"
        );
        assert_eq!(
            Algorithm::Scrypt.share_target(65536.0).to_string(),
            Target::DIFF1.to_string()
        );
        assert_eq!(Algorithm::Sha256d.share_target(1.0), Target::DIFF1);
    }

    #[test]
    fn scrypt_share_difficulty_and_work_round_trip() {
        // A hash exactly on the network diff-1 boundary is a scrypt share of difficulty 65536.
        let mut hash = [0u8; 32];
        hash[27] = 0xff;
        hash[26] = 0xff;
        let d = Algorithm::Scrypt.share_difficulty(&hash);
        assert!((d - 65536.0).abs() < 1e-6, "got {d}");
        assert!((Algorithm::Sha256d.share_difficulty(&hash) - 1.0).abs() < 1e-12);
        assert!((Algorithm::Scrypt.share_work(d) - 1.0).abs() < 1e-9);
        // A 330 MH/s scrypt miner at stratum difficulty 16384 finds a share every ~3.25 s.
        let hashes = Algorithm::Scrypt.share_work(16384.0) * 4_294_967_296.0;
        assert!((hashes / 330e6 - 3.254).abs() < 0.01);
        // At scrypt difficulty d the target is 65536x larger than at network difficulty d.
        assert!(Algorithm::Scrypt.share_target(1024.0) > Target::from_difficulty(1024.0));
    }
}
