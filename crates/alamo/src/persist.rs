//! Batch writes of share accounting, hashrate samples, and retention.

use crate::stats::Stats;
use alamo_store::{
    HashrateSample, NewShare, RetentionPolicy, RetentionReport, Store, StoreError, WorkerWrite,
};
use alamo_stratum::PoolEvent;

/// Flush pending rows once this many shares have queued.
const FLUSH_THRESHOLD: usize = 256;
/// Sample hashrate once a minute, aligned to the unix epoch.
const SAMPLE_INTERVAL: u64 = 60;
/// Run retention every ten minutes.
const RETAIN_INTERVAL: u64 = 600;

/// Queued accounting writes and periodic sampling/retention.
pub struct Persistence {
    store: Store,
    policy: RetentionPolicy,
    authorized: Vec<WorkerWrite>,
    shares: Vec<NewShare>,
    last_sample_ts: u64,
    last_retain_at: u64,
}

impl Persistence {
    /// Wrap a store. `now` is used so retention does not immediately re-run after a restart.
    pub fn new(store: Store, now: u64) -> Self {
        Self {
            store,
            policy: RetentionPolicy::default(),
            authorized: Vec::new(),
            shares: Vec::new(),
            last_sample_ts: 0,
            last_retain_at: now,
        }
    }

    /// Queue an event for the next flush. Difficulty and disconnects are session-only.
    pub fn observe(&mut self, event: &PoolEvent, ts: u64) {
        match event {
            PoolEvent::Authorized {
                worker,
                address,
                fallback,
                aux,
                ..
            } => self.authorized.push(WorkerWrite {
                name: worker.clone(),
                payout_address: address.clone(),
                fallback: *fallback,
                aux_payouts: aux
                    .iter()
                    .map(|a| alamo_store::AuxPayoutRecord {
                        coin: a.coin.to_string(),
                        address: a.address.clone(),
                        fallback: a.fallback,
                    })
                    .collect(),
                ts: ts as i64,
            }),
            PoolEvent::Share {
                worker,
                job_difficulty,
                share_difficulty,
                rejected,
                ..
            } => self.shares.push(NewShare {
                ts: ts as i64,
                worker: worker.clone(),
                difficulty: *job_difficulty,
                share_diff: *share_difficulty,
                accepted: rejected.is_none(),
                reject_reason: rejected.map(|r| r.as_str().to_string()),
            }),
            PoolEvent::Disconnected { .. } | PoolEvent::DifficultyChanged { .. } => {}
        }
    }

    /// True when the share queue is large enough to flush before the next snapshot tick.
    pub fn should_flush(&self) -> bool {
        self.shares.len() >= FLUSH_THRESHOLD
    }

    /// Write queued authorizations and shares. On failure the queue is kept for retry.
    pub async fn flush(&mut self) -> Result<(), StoreError> {
        if self.authorized.is_empty() && self.shares.is_empty() {
            return Ok(());
        }
        self.store
            .persist_batch(&self.authorized, &self.shares)
            .await?;
        tracing::debug!(
            workers = self.authorized.len(),
            shares = self.shares.len(),
            "persisted accounting"
        );
        self.authorized.clear();
        self.shares.clear();
        Ok(())
    }

    /// Flush, maybe sample hashrate, maybe retain. Called on the publisher tick.
    pub async fn on_tick(&mut self, stats: &Stats, now: u64) -> Result<(), StoreError> {
        self.flush().await?;
        self.maybe_sample(stats, now).await?;
        self.maybe_retain(now).await?;
        Ok(())
    }

    async fn maybe_sample(&mut self, stats: &Stats, now: u64) -> Result<(), StoreError> {
        let ts = (now / SAMPLE_INTERVAL) * SAMPLE_INTERVAL;
        if ts == 0 || ts <= self.last_sample_ts {
            return Ok(());
        }
        let samples: Vec<HashrateSample> = stats
            .hashrate_samples(now)
            .into_iter()
            .map(|(worker, hashrate)| HashrateSample {
                ts: ts as i64,
                worker,
                hashrate,
            })
            .collect();
        self.store.insert_hashrate_samples(&samples).await?;
        self.last_sample_ts = ts;
        Ok(())
    }

    async fn maybe_retain(&mut self, now: u64) -> Result<(), StoreError> {
        if now.saturating_sub(self.last_retain_at) < RETAIN_INTERVAL {
            return Ok(());
        }
        let report = self.store.retain(&self.policy, now as i64).await?;
        self.last_retain_at = now;
        log_retention(report);
        Ok(())
    }
}

fn log_retention(report: RetentionReport) {
    if report.shares_deleted > 0 || report.samples_deleted > 0 {
        tracing::info!(
            shares = report.shares_deleted,
            samples = report.samples_deleted,
            "retention"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alamo_core::job::RejectReason;
    use alamo_stratum::AuxPayoutInfo;
    use std::path::PathBuf;

    fn temp_db() -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "alamo-persist-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ))
            .join("pool.db")
    }

    #[tokio::test]
    async fn flush_then_reload_matches_live_counters() {
        let path = temp_db();
        let store = Store::open(&path).await.unwrap();
        let mut persist = Persistence::new(store.clone(), 1_000);
        let mut stats = Stats::default();
        let auth = PoolEvent::Authorized {
            session: 1,
            worker: "rig1".into(),
            address: "ltc1q".into(),
            fallback: false,
            aux: vec![AuxPayoutInfo {
                coin: "DOGE",
                address: "D1".into(),
                fallback: false,
            }],
        };
        let share = PoolEvent::Share {
            session: 1,
            worker: "rig1".into(),
            coin: "LTC",
            job_difficulty: 4.0,
            share_difficulty: 8.0,
            rejected: None,
        };
        let reject = PoolEvent::Share {
            session: 1,
            worker: "rig1".into(),
            coin: "LTC",
            job_difficulty: 4.0,
            share_difficulty: 0.0,
            rejected: Some(RejectReason::LowDifficulty),
        };
        for event in [&auth, &share, &reject] {
            stats.apply(event, 1_010);
            persist.observe(event, 1_010);
        }
        persist.flush().await.unwrap();

        let restored = Stats::load(&store, 1_010).await.unwrap();
        assert_eq!(restored.shares_accepted(), 1);
        assert_eq!(restored.shares_rejected(), 1);
        let w = &restored.workers(1_010)[0];
        assert_eq!(w.address, "ltc1q");
        assert_eq!(w.aux_payouts[0].address, "D1");
        assert_eq!(w.best_difficulty, 8.0);

        persist.on_tick(&stats, SAMPLE_INTERVAL).await.unwrap();
        let samples = store.hashrate_samples_since(Some(""), 0).await.unwrap();
        assert_eq!(samples.len(), 1);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
