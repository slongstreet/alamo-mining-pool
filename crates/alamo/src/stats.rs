//! In-memory pool statistics built from stratum events, restorable from the store.

use alamo_core::odds::HASHES_PER_DIFF1;
use alamo_store::{ShareRow, Store, StoreError, WorkerRow};
use alamo_stratum::PoolEvent;
use alamo_web::{AuxPayoutStatus, WorkerStatus};
use std::collections::{HashMap, HashSet, VecDeque};

/// Window over which hashrate is estimated, in seconds.
pub const HASHRATE_WINDOW_SECS: u64 = 600;

#[derive(Debug, Default)]
struct WorkerStats {
    address: String,
    fallback: bool,
    aux: Vec<AuxPayoutStatus>,
    sessions: HashSet<u64>,
    difficulty: f64,
    accepted: u64,
    rejected: u64,
    best_difficulty: f64,
    last_share: Option<u64>,
    /// Accepted shares in the hashrate window: (unix time, job difficulty).
    window: VecDeque<(u64, f64)>,
}

/// Aggregated pool statistics.
#[derive(Debug, Default)]
pub struct Stats {
    workers: HashMap<String, WorkerStats>,
    shares_accepted: u64,
    shares_rejected: u64,
}

impl Stats {
    /// Rebuild counters and the hashrate window from persisted rows.
    pub fn restore(workers: &[WorkerRow], recent_accepted: &[ShareRow], now: u64) -> Self {
        let mut stats = Self::default();
        for row in workers {
            let w = WorkerStats {
                address: row.payout_address.clone(),
                fallback: row.fallback,
                aux: row
                    .aux_payouts
                    .iter()
                    .map(|a| AuxPayoutStatus {
                        coin: a.coin.clone(),
                        address: a.address.clone(),
                        fallback: a.fallback,
                    })
                    .collect(),
                sessions: HashSet::new(),
                difficulty: 0.0,
                accepted: row.shares_accepted.max(0) as u64,
                rejected: row.shares_rejected.max(0) as u64,
                best_difficulty: row.best_difficulty,
                last_share: (row.shares_accepted > 0).then_some(row.last_seen.max(0) as u64),
                window: VecDeque::new(),
            };
            stats.shares_accepted += w.accepted;
            stats.shares_rejected += w.rejected;
            stats.workers.insert(row.name.clone(), w);
        }
        let cutoff = now.saturating_sub(HASHRATE_WINDOW_SECS);
        for share in recent_accepted {
            if share.ts < cutoff as i64 {
                continue;
            }
            let w = stats
                .workers
                .entry(share.worker.clone())
                .or_insert_with(|| WorkerStats {
                    last_share: Some(share.ts as u64),
                    ..WorkerStats::default()
                });
            w.window.push_back((share.ts as u64, share.difficulty));
            if w.last_share.unwrap_or(0) < share.ts as u64 {
                w.last_share = Some(share.ts as u64);
            }
        }
        for w in stats.workers.values_mut() {
            trim(&mut w.window, now);
        }
        stats
    }

    /// Load workers and recent accepted shares from the store.
    pub async fn load(store: &Store, now: u64) -> Result<Self, StoreError> {
        let workers = store.load_workers().await?;
        let since = now.saturating_sub(HASHRATE_WINDOW_SECS) as i64;
        let shares = store.accepted_shares_since(since).await?;
        Ok(Self::restore(&workers, &shares, now))
    }

    /// Apply one event.
    pub fn apply(&mut self, event: &PoolEvent, now: u64) {
        match event {
            PoolEvent::Authorized {
                session,
                worker,
                address,
                fallback,
                aux,
            } => {
                let w = self.workers.entry(worker.clone()).or_default();
                w.address = address.clone();
                w.fallback = *fallback;
                w.aux = aux
                    .iter()
                    .map(|a| AuxPayoutStatus {
                        coin: a.coin.to_string(),
                        address: a.address.clone(),
                        fallback: a.fallback,
                    })
                    .collect();
                w.sessions.insert(*session);
            }
            PoolEvent::Disconnected { session, workers } => {
                for name in workers {
                    if let Some(w) = self.workers.get_mut(name) {
                        w.sessions.remove(session);
                    }
                }
            }
            PoolEvent::Share {
                worker,
                job_difficulty,
                share_difficulty,
                rejected,
                ..
            } => {
                let w = self.workers.entry(worker.clone()).or_default();
                if rejected.is_some() {
                    w.rejected += 1;
                    self.shares_rejected += 1;
                } else {
                    w.accepted += 1;
                    self.shares_accepted += 1;
                    w.last_share = Some(now);
                    w.best_difficulty = w.best_difficulty.max(*share_difficulty);
                    w.window.push_back((now, *job_difficulty));
                }
                trim(&mut w.window, now);
            }
            PoolEvent::DifficultyChanged {
                session,
                difficulty,
            } => {
                for w in self
                    .workers
                    .values_mut()
                    .filter(|w| w.sessions.contains(session))
                {
                    w.difficulty = *difficulty;
                }
            }
        }
    }

    /// Accepted shares recorded (lifetime, including restored).
    pub fn shares_accepted(&self) -> u64 {
        self.shares_accepted
    }

    /// Rejected shares recorded (lifetime, including restored).
    pub fn shares_rejected(&self) -> u64 {
        self.shares_rejected
    }

    /// Per-worker hashrate samples plus the pool total (empty name).
    pub fn hashrate_samples(&self, now: u64) -> Vec<(String, f64)> {
        let workers = self.workers(now);
        let mut samples = Vec::with_capacity(workers.len() + 1);
        let mut total = 0.0;
        for w in &workers {
            if w.connections > 0 || w.hashrate > 0.0 {
                samples.push((w.name.clone(), w.hashrate));
            }
            total += w.hashrate;
        }
        samples.push((String::new(), total));
        samples
    }

    /// Per-worker status, connected workers first, then by name.
    pub fn workers(&self, now: u64) -> Vec<WorkerStatus> {
        let mut out: Vec<WorkerStatus> = self
            .workers
            .iter()
            .map(|(name, w)| WorkerStatus {
                name: name.clone(),
                address: w.address.clone(),
                fallback: w.fallback,
                aux_payouts: w.aux.clone(),
                connections: w.sessions.len(),
                difficulty: w.difficulty,
                hashrate: hashrate(&w.window, now),
                shares_accepted: w.accepted,
                shares_rejected: w.rejected,
                best_difficulty: w.best_difficulty,
                last_share_seconds: w.last_share.map(|t| now.saturating_sub(t)),
            })
            .collect();
        out.sort_by(|a, b| {
            (b.connections > 0)
                .cmp(&(a.connections > 0))
                .then_with(|| a.name.cmp(&b.name))
        });
        out
    }
}

fn trim(window: &mut VecDeque<(u64, f64)>, now: u64) {
    while let Some((t, _)) = window.front() {
        if now.saturating_sub(*t) > HASHRATE_WINDOW_SECS {
            window.pop_front();
        } else {
            break;
        }
    }
}

/// Estimate hashrate from shares in the window. The window length used is the time since
/// the oldest share, so a worker that just connected is not underestimated.
fn hashrate(window: &VecDeque<(u64, f64)>, now: u64) -> f64 {
    let Some((oldest, _)) = window.front() else {
        return 0.0;
    };
    let span = (now.saturating_sub(*oldest) as f64).max(30.0);
    let work: f64 = window.iter().map(|(_, d)| d * HASHES_PER_DIFF1).sum();
    work / span
}

#[cfg(test)]
mod tests {
    use super::*;
    use alamo_store::{AuxPayoutRecord, NewShare, WorkerWrite};
    use std::path::PathBuf;

    fn authorized(session: u64, worker: &str) -> PoolEvent {
        PoolEvent::Authorized {
            session,
            worker: worker.into(),
            address: "x".into(),
            fallback: false,
            aux: Vec::new(),
        }
    }

    fn accepted_share(session: u64, worker: &str, job_difficulty: f64) -> PoolEvent {
        PoolEvent::Share {
            session,
            worker: worker.into(),
            coin: "LTC",
            job_difficulty,
            share_difficulty: 2.0,
            rejected: None,
        }
    }

    fn temp_db() -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "alamo-stats-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ))
            .join("pool.db")
    }

    #[test]
    fn hashrate_from_shares() {
        let mut s = Stats::default();
        let t0 = 1_700_000_000;
        s.apply(&authorized(1, "a"), t0);
        for i in 0..10 {
            s.apply(&accepted_share(1, "a", 1.0), t0 + i * 10);
        }
        // 10 shares of difficulty 1 over 90s = 10 * 2^32 / 90.
        let w = &s.workers(t0 + 90)[0];
        assert!(
            (w.hashrate - 10.0 * HASHES_PER_DIFF1 / 90.0).abs() < 1.0,
            "{}",
            w.hashrate
        );
        assert_eq!(w.shares_accepted, 10);
        assert_eq!(w.best_difficulty, 2.0);
        assert_eq!(w.connections, 1);
        s.apply(
            &PoolEvent::Disconnected {
                session: 1,
                workers: vec!["a".into()],
            },
            t0,
        );
        assert_eq!(s.workers(t0)[0].connections, 0);
    }

    #[tokio::test]
    async fn restore_rebuilds_counters_and_hashrate_after_restart() {
        let path = temp_db();
        let store = Store::open(&path).await.unwrap();
        let t0 = 1_700_000_000i64;
        store
            .persist_batch(
                &[WorkerWrite {
                    name: "rig1".into(),
                    payout_address: "ltc1qabc".into(),
                    fallback: true,
                    aux_payouts: vec![AuxPayoutRecord {
                        coin: "DOGE".into(),
                        address: "Dabc".into(),
                        fallback: false,
                    }],
                    ts: t0,
                }],
                &(0..10)
                    .map(|i| NewShare {
                        ts: t0 + i * 10,
                        worker: "rig1".into(),
                        difficulty: 1.0,
                        share_diff: 2.0,
                        accepted: true,
                        reject_reason: None,
                    })
                    .collect::<Vec<_>>(),
            )
            .await
            .unwrap();

        let restored = Stats::load(&store, (t0 + 90) as u64).await.unwrap();
        assert_eq!(restored.shares_accepted(), 10);
        let w = &restored.workers((t0 + 90) as u64)[0];
        assert_eq!(w.name, "rig1");
        assert_eq!(w.address, "ltc1qabc");
        assert!(w.fallback);
        assert_eq!(w.aux_payouts[0].coin, "DOGE");
        assert_eq!(w.connections, 0);
        assert_eq!(w.shares_accepted, 10);
        assert_eq!(w.best_difficulty, 2.0);
        assert!(
            (w.hashrate - 10.0 * HASHES_PER_DIFF1 / 90.0).abs() < 1.0,
            "{}",
            w.hashrate
        );
        let samples = restored.hashrate_samples((t0 + 90) as u64);
        let pool = samples.iter().find(|(n, _)| n.is_empty()).unwrap();
        assert!(pool.1 > 0.0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
