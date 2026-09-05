//! Per-session jobs derived from a shared work template.

use alamo_core::coinbase::CoinbaseParts;
use alamo_core::hash::Hash256;
use alamo_core::job::JobId;
use alamo_core::target::Target;
use alamo_core::work::WorkTemplate;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::Arc;

/// Size of the extranonce1 the server assigns per connection.
pub const EXTRANONCE1_LEN: usize = 4;
/// Size of the extranonce2 miners fill in.
pub const EXTRANONCE2_LEN: usize = 4;

/// A job sent to one session via `mining.notify`.
#[derive(Debug)]
pub struct SessionJob {
    /// Session-local job id.
    pub id: JobId,
    /// The template this job was derived from.
    pub work: Arc<WorkTemplate>,
    /// Coinbase split around the extranonce, paying this session's address.
    pub coinbase: CoinbaseParts,
    /// Share difficulty in force for this job.
    pub difficulty: f64,
    /// Share target derived from `difficulty`.
    pub target: Target,
    /// Set once a newer template with `clean_jobs` has replaced it.
    pub stale: bool,
    /// Shares already accepted, keyed by (extranonce2, ntime, nonce).
    pub seen: HashSet<(u32, u32, u32)>,
}

impl SessionJob {
    /// Parameters for `mining.notify`.
    pub fn notify_params(&self, clean_jobs: bool) -> Value {
        let branch: Vec<String> = self.work.merkle_branch.iter().map(hex::encode).collect();
        json!([
            self.id.to_string(),
            prevhash_to_stratum(&self.work.prev_hash),
            hex::encode(&self.coinbase.coinb1),
            hex::encode(&self.coinbase.coinb2),
            branch,
            format!("{:08x}", self.work.version as u32),
            format!("{:08x}", self.work.bits),
            format!("{:08x}", self.work.cur_time),
            clean_jobs,
        ])
    }
}

/// Encode a previous block hash the way stratum expects: header byte order with each
/// 4-byte word reversed.
pub fn prevhash_to_stratum(hash: &Hash256) -> String {
    let mut swapped = [0u8; 32];
    for (dst, src) in swapped.chunks_mut(4).zip(hash.chunks(4)) {
        dst.copy_from_slice(src);
        dst.reverse();
    }
    hex::encode(swapped)
}

/// Inverse of [`prevhash_to_stratum`], as a miner performs it.
pub fn prevhash_from_stratum(hex_str: &str) -> Option<Hash256> {
    let bytes = hex::decode(hex_str).ok()?;
    let mut out: Hash256 = bytes.try_into().ok()?;
    for word in out.chunks_mut(4) {
        word.reverse();
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alamo_core::hash::from_display_hex;

    #[test]
    fn stratum_prevhash_is_word_swapped_display_order() {
        // Display order ends with the leading zeros of the hash reversed to the front.
        let display = "00000000000000000009b7d2a2d5d5c58fd9a5a2e6d6f1d7c3b3a2f1e0d0c0b0";
        let internal = from_display_hex(display).unwrap();
        let stratum = prevhash_to_stratum(&internal);
        // Stratum form = display hex with its eight 8-char words in reverse order.
        let words: Vec<&str> = (0..8).map(|i| &display[i * 8..i * 8 + 8]).collect();
        let expected: String = words.iter().rev().copied().collect();
        assert_eq!(stratum, expected);
        assert_eq!(prevhash_from_stratum(&stratum).unwrap(), internal);
    }
}
