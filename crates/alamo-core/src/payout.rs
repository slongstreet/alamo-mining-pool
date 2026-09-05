//! Mapping stratum usernames to payout scripts.

/// Where a worker's block reward goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Payout {
    /// The address that will be paid, as a string for display.
    pub address: String,
    /// The scriptPubKey paying that address.
    pub script: Vec<u8>,
    /// Worker suffix from the username (`address.worker`), empty if none.
    pub worker: String,
    /// True when the username was not a valid address and the fallback was used.
    pub fallback: bool,
}

/// Resolves stratum usernames to payouts. Implemented per coin by `alamo-coins`.
pub trait PayoutResolver: Send + Sync {
    /// Resolve `username` (typically `address` or `address.worker`).
    fn resolve(&self, username: &str) -> Payout;
}

/// Split a stratum username into its address part and worker suffix.
pub fn split_username(username: &str) -> (&str, &str) {
    match username.split_once('.') {
        Some((address, worker)) => (address.trim(), worker.trim()),
        None => (username.trim(), ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_worker_suffix() {
        assert_eq!(split_username("addr.rig1"), ("addr", "rig1"));
        assert_eq!(split_username("addr"), ("addr", ""));
        assert_eq!(split_username(" addr . rig.2 "), ("addr", "rig.2"));
    }
}
