//! Wall-clock helper shared by every crate.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch, saturating at zero if the clock is before 1970.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
