//! In-memory pool statistics built from stratum events.

use alamo_core::odds::HASHES_PER_DIFF1;
use alamo_stratum::PoolEvent;
use alamo_web::WorkerStatus;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

/// Window over which hashrate is estimated.
const HASHRATE_WINDOW: Duration = Duration::from_secs(600);

#[derive(Debug, Default)]
struct WorkerStats {
    address: String,
    fallback: bool,
    sessions: HashSet<u64>,
    difficulty: f64,
    accepted: u64,
    rejected: u64,
    best_difficulty: f64,
    last_share: Option<Instant>,
    /// Accepted shares in the hashrate window: (time, job difficulty).
    window: VecDeque<(Instant, f64)>,
}

/// Aggregated pool statistics.
#[derive(Debug, Default)]
pub struct Stats {
    workers: HashMap<String, WorkerStats>,
    shares_accepted: u64,
    shares_rejected: u64,
}

impl Stats {
    /// Apply one event.
    pub fn apply(&mut self, event: PoolEvent, now: Instant) {
        match event {
            PoolEvent::Authorized {
                session,
                worker,
                address,
                fallback,
            } => {
                let w = self.workers.entry(worker).or_default();
                w.address = address;
                w.fallback = fallback;
                w.sessions.insert(session);
            }
            PoolEvent::Disconnected { session, workers } => {
                for name in workers {
                    if let Some(w) = self.workers.get_mut(&name) {
                        w.sessions.remove(&session);
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
                let w = self.workers.entry(worker).or_default();
                if rejected.is_some() {
                    w.rejected += 1;
                    self.shares_rejected += 1;
                } else {
                    w.accepted += 1;
                    self.shares_accepted += 1;
                    w.last_share = Some(now);
                    w.best_difficulty = w.best_difficulty.max(share_difficulty);
                    w.window.push_back((now, job_difficulty));
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
                    .filter(|w| w.sessions.contains(&session))
                {
                    w.difficulty = difficulty;
                }
            }
        }
    }

    /// Accepted shares since start.
    pub fn shares_accepted(&self) -> u64 {
        self.shares_accepted
    }

    /// Rejected shares since start.
    pub fn shares_rejected(&self) -> u64 {
        self.shares_rejected
    }

    /// Per-worker status, connected workers first, then by name.
    pub fn workers(&self, now: Instant) -> Vec<WorkerStatus> {
        let mut out: Vec<WorkerStatus> = self
            .workers
            .iter()
            .map(|(name, w)| WorkerStatus {
                name: name.clone(),
                address: w.address.clone(),
                fallback: w.fallback,
                connections: w.sessions.len(),
                difficulty: w.difficulty,
                hashrate: hashrate(&w.window, now),
                shares_accepted: w.accepted,
                shares_rejected: w.rejected,
                best_difficulty: w.best_difficulty,
                last_share_seconds: w.last_share.map(|t| now.duration_since(t).as_secs()),
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

fn trim(window: &mut VecDeque<(Instant, f64)>, now: Instant) {
    while let Some((t, _)) = window.front() {
        if now.duration_since(*t) > HASHRATE_WINDOW {
            window.pop_front();
        } else {
            break;
        }
    }
}

/// Estimate hashrate from shares in the window. The window length used is the time since
/// the oldest share, so a worker that just connected is not underestimated.
fn hashrate(window: &VecDeque<(Instant, f64)>, now: Instant) -> f64 {
    let Some((oldest, _)) = window.front() else {
        return 0.0;
    };
    let span = now.duration_since(*oldest).as_secs_f64().max(30.0);
    let work: f64 = window.iter().map(|(_, d)| d * HASHES_PER_DIFF1).sum();
    work / span
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashrate_from_shares() {
        let mut s = Stats::default();
        let t0 = Instant::now();
        s.apply(
            PoolEvent::Authorized {
                session: 1,
                worker: "a".into(),
                address: "x".into(),
                fallback: false,
            },
            t0,
        );
        for i in 0..10 {
            s.apply(
                PoolEvent::Share {
                    session: 1,
                    worker: "a".into(),
                    coin: "LTC",
                    job_difficulty: 1.0,
                    share_difficulty: 2.0,
                    rejected: None,
                },
                t0 + Duration::from_secs(i * 10),
            );
        }
        // 10 shares of difficulty 1 over 90s = 10 * 2^32 / 90.
        let w = &s.workers(t0 + Duration::from_secs(90))[0];
        assert!(
            (w.hashrate - 10.0 * HASHES_PER_DIFF1 / 90.0).abs() < 1.0,
            "{}",
            w.hashrate
        );
        assert_eq!(w.shares_accepted, 10);
        assert_eq!(w.best_difficulty, 2.0);
        assert_eq!(w.connections, 1);
        s.apply(
            PoolEvent::Disconnected {
                session: 1,
                workers: vec!["a".into()],
            },
            t0,
        );
        assert_eq!(s.workers(t0)[0].connections, 0);
    }
}
