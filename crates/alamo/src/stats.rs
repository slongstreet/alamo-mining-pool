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
    /// Best share found, in network difficulty-1 units (comparable to block difficulty).
    best_difficulty: f64,
    /// Lifetime accepted work in network difficulty-1 units.
    work_accepted: f64,
    last_share: Option<u64>,
    /// Accepted shares in the hashrate window: (unix time, work in difficulty-1 units).
    window: VecDeque<(u64, f64)>,
}

/// Aggregated pool statistics.
///
/// Stratum share difficulty is a per-algorithm multiple of network difficulty (65536 for
/// scrypt, see [`alamo_core::algo::Algorithm::share_multiplier`]). Events and persisted
/// share rows carry stratum difficulty; everything accumulated here is converted to
/// network difficulty-1 units so work, hashrate, and best share compare to block
/// difficulty directly.
#[derive(Debug)]
pub struct Stats {
    share_multiplier: f64,
    workers: HashMap<String, WorkerStats>,
    shares_accepted: u64,
    shares_rejected: u64,
    /// Accepted work of workers since removed, so removal does not move the round.
    retired_work: f64,
    /// When the pool first saw a worker: the start of the score before any block.
    scoring_since: Option<u64>,
}

impl Default for Stats {
    fn default() -> Self {
        Self::new(1.0)
    }
}

impl Stats {
    /// Empty statistics for a pool whose parent chain uses `share_multiplier`.
    pub fn new(share_multiplier: f64) -> Self {
        Self {
            share_multiplier,
            workers: HashMap::new(),
            shares_accepted: 0,
            shares_rejected: 0,
            retired_work: 0.0,
            scoring_since: None,
        }
    }

    /// Rebuild counters and the hashrate window from persisted rows.
    pub fn restore(
        workers: &[WorkerRow],
        recent_accepted: &[ShareRow],
        now: u64,
        share_multiplier: f64,
    ) -> Self {
        let mut stats = Self::new(share_multiplier);
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
                // Persisted in stratum share units (a MAX and a SUM over share rows).
                best_difficulty: row.best_difficulty / share_multiplier,
                work_accepted: row.work_accepted / share_multiplier,
                last_share: (row.shares_accepted > 0).then_some(row.last_seen.max(0) as u64),
                window: VecDeque::new(),
            };
            stats.shares_accepted += w.accepted;
            stats.shares_rejected += w.rejected;
            stats.note_seen(row.first_seen.max(0) as u64);
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
            w.window
                .push_back((share.ts as u64, share.difficulty / share_multiplier));
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
    pub async fn load(store: &Store, now: u64, share_multiplier: f64) -> Result<Self, StoreError> {
        let workers = store.load_workers().await?;
        let since = now.saturating_sub(HASHRATE_WINDOW_SECS) as i64;
        let shares = store.accepted_shares_since(since).await?;
        let mut stats = Self::restore(&workers, &shares, now, share_multiplier);
        // Banked from worker rows, so stratum share units as well.
        stats.retired_work = store.retired_work().await? / share_multiplier;
        Ok(stats)
    }

    /// Stratum share difficulty per unit of network difficulty on the parent chain.
    pub fn share_multiplier(&self) -> f64 {
        self.share_multiplier
    }

    /// Unix time the pool first saw a worker, if it ever has. Survives worker removal,
    /// since the removed worker's work still counts.
    pub fn scoring_since(&self) -> Option<u64> {
        self.scoring_since
    }

    fn note_seen(&mut self, at: u64) {
        self.scoring_since = Some(self.scoring_since.map_or(at, |t| t.min(at)));
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
                self.note_seen(now);
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
                    w.best_difficulty = w
                        .best_difficulty
                        .max(*share_difficulty / self.share_multiplier);
                    let work = *job_difficulty / self.share_multiplier;
                    w.work_accepted += work;
                    w.window.push_back((now, work));
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

    /// Zero accepted/rejected counts and best share for every worker and the pool.
    /// Hashrate windows, accepted work, and payout details are kept.
    pub fn reset_counters(&mut self) {
        for w in self.workers.values_mut() {
            w.accepted = 0;
            w.rejected = 0;
            w.best_difficulty = 0.0;
        }
        self.shares_accepted = 0;
        self.shares_rejected = 0;
    }

    /// Forget a worker. Its share counts leave the pool totals but its accepted work is
    /// retired, not lost, so lifetime work and the current round stay put. Returns false
    /// when the worker is unknown or still has a live session.
    pub fn remove_worker(&mut self, name: &str) -> bool {
        match self.workers.get(name) {
            Some(w) if w.sessions.is_empty() => {}
            _ => return false,
        }
        let w = self.workers.remove(name).expect("checked above");
        self.shares_accepted -= w.accepted;
        self.shares_rejected -= w.rejected;
        self.retired_work += w.work_accepted;
        true
    }

    /// Whether `name` currently has a live stratum session.
    pub fn is_connected(&self, name: &str) -> bool {
        self.workers
            .get(name)
            .is_some_and(|w| !w.sessions.is_empty())
    }

    /// Accepted shares recorded (lifetime, including restored).
    pub fn shares_accepted(&self) -> u64 {
        self.shares_accepted
    }

    /// Rejected shares recorded (lifetime, including restored).
    pub fn shares_rejected(&self) -> u64 {
        self.shares_rejected
    }

    /// Lifetime accepted work in network difficulty-1 units, summed over every worker
    /// plus the work of workers since removed.
    pub fn total_work(&self) -> f64 {
        self.workers.values().map(|w| w.work_accepted).sum::<f64>() + self.retired_work
    }

    /// Best share any worker has found, in network difficulty-1 units.
    pub fn best_difficulty(&self) -> f64 {
        self.workers
            .values()
            .map(|w| w.best_difficulty)
            .fold(0.0, f64::max)
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

    /// Per-worker status: connected workers first, then the most recent share first,
    /// then by name.
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
                work_accepted: w.work_accepted,
                last_share_seconds: w.last_share.map(|t| now.saturating_sub(t)),
            })
            .collect();
        out.sort_by(|a, b| {
            (b.connections > 0)
                .cmp(&(a.connections > 0))
                .then_with(|| {
                    a.last_share_seconds
                        .unwrap_or(u64::MAX)
                        .cmp(&b.last_share_seconds.unwrap_or(u64::MAX))
                })
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

    #[tokio::test]
    async fn restore_converts_persisted_work_from_stratum_units() {
        let path = temp_db();
        let store = Store::open(&path).await.unwrap();
        let t0 = 1_700_000_000i64;
        let worker = |name: &str| WorkerWrite {
            name: name.into(),
            payout_address: "ltc1qabc".into(),
            fallback: false,
            aux_payouts: vec![],
            ts: t0,
        };
        // Two scrypt shares at stratum difficulty 65536: one unit of network work each.
        let share = |name: &str, i: i64| NewShare {
            ts: t0 + i,
            worker: name.into(),
            difficulty: 65_536.0,
            share_diff: 2.0 * 65_536.0,
            accepted: true,
            reject_reason: None,
        };
        store
            .persist_batch(
                &[worker("rig1"), worker("rig2")],
                &[share("rig1", 1), share("rig1", 2), share("rig2", 3)],
            )
            .await
            .unwrap();
        store.remove_worker("rig2").await.unwrap();

        let restored = Stats::load(&store, (t0 + 10) as u64, 65_536.0)
            .await
            .unwrap();
        let w = &restored.workers((t0 + 10) as u64)[0];
        assert_eq!(w.work_accepted, 2.0);
        assert_eq!(w.best_difficulty, 2.0);
        assert_eq!(restored.total_work(), 3.0, "retired work converts too");
        assert_eq!(restored.scoring_since(), Some(t0 as u64));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn workers_order_connected_first_then_most_recent_share() {
        let mut s = Stats::default();
        let t0 = 1_700_000_000;
        s.apply(&authorized(1, "old"), t0);
        s.apply(&accepted_share(1, "old", 1.0), t0);
        s.apply(&authorized(2, "fresh"), t0);
        s.apply(&accepted_share(2, "fresh", 1.0), t0 + 50);
        s.apply(&authorized(3, "live"), t0);
        s.apply(
            &PoolEvent::Disconnected {
                session: 1,
                workers: vec!["old".into()],
            },
            t0 + 60,
        );
        s.apply(
            &PoolEvent::Disconnected {
                session: 2,
                workers: vec!["fresh".into()],
            },
            t0 + 60,
        );
        let names: Vec<_> = s.workers(t0 + 60).into_iter().map(|w| w.name).collect();
        assert_eq!(names, ["live", "fresh", "old"]);
        assert_eq!(s.scoring_since(), Some(t0));
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
        assert_eq!(w.work_accepted, 10.0);
        assert_eq!(s.total_work(), 10.0);
        assert_eq!(s.best_difficulty(), 2.0);
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

        let restored = Stats::load(&store, (t0 + 90) as u64, 1.0).await.unwrap();
        assert_eq!(restored.shares_accepted(), 10);
        let w = &restored.workers((t0 + 90) as u64)[0];
        assert_eq!(w.name, "rig1");
        assert_eq!(w.address, "ltc1qabc");
        assert!(w.fallback);
        assert_eq!(w.aux_payouts[0].coin, "DOGE");
        assert_eq!(w.connections, 0);
        assert_eq!(w.shares_accepted, 10);
        assert_eq!(w.best_difficulty, 2.0);
        assert_eq!(restored.total_work(), 10.0);
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

    #[test]
    fn scrypt_shares_are_accounted_in_network_units() {
        // 65536 is the scrypt multiplier: a share at stratum difficulty 65536 is one unit
        // of network difficulty and worth 2^32 hashes.
        let mut s = Stats::new(65536.0);
        let t0 = 1_000;
        s.apply(&authorized(1, "a"), t0);
        for i in 0..10u64 {
            s.apply(&accepted_share(1, "a", 65536.0), t0 + i * 10);
        }
        let w = &s.workers(t0 + 90)[0];
        assert_eq!(w.work_accepted, 10.0);
        assert!((w.hashrate - 10.0 * HASHES_PER_DIFF1 / 90.0).abs() < 1.0);
        assert_eq!(s.total_work(), 10.0);
    }

    #[test]
    fn reset_zeroes_counts_and_best_but_keeps_hashrate_and_work() {
        let mut s = Stats::new(65536.0);
        let t0 = 1_000;
        s.apply(&authorized(1, "a"), t0);
        for i in 0..10u64 {
            s.apply(&accepted_share(1, "a", 65536.0), t0 + i * 10);
        }
        assert_eq!(s.shares_accepted(), 10);
        s.reset_counters();
        assert_eq!(s.shares_accepted(), 0);
        assert_eq!(s.shares_rejected(), 0);
        assert_eq!(s.best_difficulty(), 0.0);
        let w = &s.workers(t0 + 90)[0];
        assert_eq!(w.shares_accepted, 0);
        assert!(w.hashrate > 0.0, "hashrate window survives a reset");
        assert_eq!(w.work_accepted, 10.0);
    }

    #[test]
    fn removing_a_worker_drops_its_counts_but_retires_its_work() {
        let mut s = Stats::new(65536.0);
        let t0 = 1_000;
        s.apply(&authorized(1, "a"), t0);
        s.apply(&authorized(2, "b"), t0);
        for i in 0..10u64 {
            s.apply(&accepted_share(1, "a", 65536.0), t0 + i * 10);
        }
        s.apply(&accepted_share(2, "b", 65536.0), t0);
        assert!(!s.remove_worker("a"), "a live worker cannot be removed");
        assert!(!s.remove_worker("nobody"));
        s.apply(
            &PoolEvent::Disconnected {
                session: 1,
                workers: vec!["a".into()],
            },
            t0 + 100,
        );
        assert!(s.remove_worker("a"));
        assert_eq!(s.workers(t0 + 100).len(), 1);
        assert_eq!(s.shares_accepted(), 1);
        assert_eq!(s.total_work(), 11.0, "retired work still counts");
    }

    #[test]
    fn restored_best_share_is_converted_from_stratum_units() {
        let row = WorkerRow {
            name: "a".into(),
            payout_address: "addr".into(),
            fallback: false,
            aux_payouts: Vec::new(),
            first_seen: 0,
            last_seen: 0,
            shares_accepted: 1,
            shares_rejected: 0,
            best_difficulty: 229_126_140.0, // a 3496.2-diff share in scrypt share units
            work_accepted: 0.0,
        };
        let s = Stats::restore(&[row], &[], 100, 65536.0);
        assert!((s.best_difficulty() - 3496.187).abs() < 0.01);
    }
}
