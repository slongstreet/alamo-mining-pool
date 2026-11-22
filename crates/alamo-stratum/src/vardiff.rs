//! Variable difficulty: keep each worker near one share per `target_share_seconds`.
//!
//! The estimate is the worker's accepted work rate (sum of share difficulties over time)
//! across a sliding window several retarget intervals long, so shares submitted at
//! different difficulties are comparable and one lucky or unlucky minute does not move
//! the target. Changes are rate-limited to one per `retarget_seconds`, stepped by at most
//! 2x either way (4x down when the worker has gone completely quiet), and an increase
//! needs enough shares behind it to be a measurement rather than a burst. The algorithm
//! is independent of the proof-of-work: it works in whatever share units the session
//! uses, scrypt or sha256d.
//!
//! The exception is a gross mismatch. A worker measured at [`FAST_MIN_RATIO`] times the
//! target rate or more (an ASIC that connected at a small miner's starting difficulty)
//! jumps straight to the measured difficulty after [`FAST_RETARGET_SECS`], without the
//! step cap: doubling once a minute would leave it flooding shares for many minutes.

use crate::config::VardiffConfig;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Window length as a multiple of `retarget_seconds`.
const WINDOW_RETARGETS: f64 = 4.0;
/// Largest step up or down per retarget.
const MAX_STEP: f64 = 2.0;
/// Step down when no share arrived in the whole window.
const QUIET_STEP: f64 = 0.25;
/// Shares needed in the window before difficulty is raised.
const MIN_SHARES_TO_RAISE: usize = 8;
/// A worker this many times over the target rate skips the step cap and the retarget
/// interval and moves straight to the measured difficulty.
const FAST_MIN_RATIO: f64 = 8.0;
/// Minimum seconds between changes on the fast path (and after connecting).
const FAST_RETARGET_SECS: f64 = 5.0;
/// Upper bound on remembered shares; a worker above this rate is far above target anyway.
const MAX_WINDOW_SHARES: usize = 512;

#[derive(Debug)]
pub struct Vardiff {
    cfg: VardiffConfig,
    /// Accepted shares in the window: (time, difficulty of the job it was for).
    shares: VecDeque<(Instant, f64)>,
    started: Instant,
    last_change: Instant,
}

impl Vardiff {
    pub fn new(cfg: VardiffConfig, now: Instant) -> Self {
        Self {
            cfg,
            shares: VecDeque::new(),
            started: now,
            last_change: now,
        }
    }

    pub fn clamp(&self, difficulty: f64) -> f64 {
        difficulty.clamp(self.cfg.min_difficulty, self.cfg.max_difficulty)
    }

    fn window(&self) -> Duration {
        Duration::from_secs_f64(self.cfg.retarget_seconds * WINDOW_RETARGETS)
    }

    /// Record an accepted share for a job at `difficulty`.
    pub fn on_share(&mut self, now: Instant, difficulty: f64) {
        self.shares.push_back((now, difficulty));
        if self.shares.len() > MAX_WINDOW_SHARES {
            self.shares.pop_front();
        }
        self.trim(now);
    }

    fn trim(&mut self, now: Instant) {
        let window = self.window();
        while let Some((t, _)) = self.shares.front() {
            if now.duration_since(*t) > window {
                self.shares.pop_front();
            } else {
                break;
            }
        }
    }

    /// The difficulty the worker should move to, if it is time and the evidence says so.
    pub fn evaluate(&mut self, now: Instant, current: f64) -> Option<f64> {
        let since_change = now.duration_since(self.last_change).as_secs_f64();
        // Before the retarget interval only the fast path can act, and it needs a
        // measurement; bail out early so the common case costs nothing per share.
        let due = since_change >= self.cfg.retarget_seconds;
        if !due && (since_change < FAST_RETARGET_SECS || self.shares.len() < MIN_SHARES_TO_RAISE) {
            return None;
        }
        self.trim(now);
        // Span the estimate covers: the whole window, or the session so far if shorter.
        let span = now
            .duration_since(self.started)
            .min(self.window())
            .as_secs_f64()
            .max(1.0);
        let work: f64 = self.shares.iter().map(|(_, d)| d).sum();
        let optimal = work / span * self.cfg.target_share_seconds;
        let measured = self.shares.len() >= MIN_SHARES_TO_RAISE;
        let gross = measured && optimal >= current * FAST_MIN_RATIO;
        if !due && !gross {
            return None;
        }

        let ratio = if self.shares.is_empty() {
            QUIET_STEP
        } else {
            let deviation = ((optimal - current) / current).abs() * 100.0;
            if deviation <= self.cfg.variance_percent {
                return None;
            }
            let ratio = optimal / current;
            if ratio > 1.0 && !measured {
                return None;
            }
            if gross {
                ratio
            } else {
                ratio.clamp(1.0 / MAX_STEP, MAX_STEP)
            }
        };
        let proposed = self.clamp(current * ratio);
        if (proposed - current).abs() / current < 1e-9 {
            return None;
        }
        self.last_change = now;
        Some(proposed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> VardiffConfig {
        VardiffConfig {
            initial_difficulty: 1000.0,
            min_difficulty: 10.0,
            max_difficulty: 100_000.0,
            target_share_seconds: 10.0,
            retarget_seconds: 60.0,
            variance_percent: 30.0,
        }
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    /// Feed shares at a fixed interval for `for_secs`, starting at `from`.
    fn shares_every(v: &mut Vardiff, t0: Instant, from: u64, for_secs: u64, every: u64, diff: f64) {
        let mut t = from + every;
        while t <= from + for_secs {
            v.on_share(t0 + secs(t), diff);
            t += every;
        }
    }

    #[test]
    fn nothing_before_retarget_interval() {
        let t0 = Instant::now();
        let mut v = Vardiff::new(cfg(), t0);
        // 5x too fast is not a gross mismatch: it waits for the interval.
        shares_every(&mut v, t0, 0, 30, 2, 1000.0);
        assert_eq!(v.evaluate(t0 + secs(30), 1000.0), None);
    }

    /// An ASIC that connects at a small miner's difficulty is moved to its measured
    /// difficulty within seconds, not doubled once a minute.
    #[test]
    fn gross_mismatch_jumps_straight_to_the_measured_difficulty() {
        let t0 = Instant::now();
        let mut v = Vardiff::new(cfg(), t0);
        // 20 shares a second at difficulty 10: 200x the target rate.
        for i in 1..=200u64 {
            v.on_share(t0 + Duration::from_millis(i * 50), 10.0);
        }
        // Too soon after connecting, even with plenty of shares.
        assert_eq!(v.evaluate(t0 + secs(4), 10.0), None);
        let new = v.evaluate(t0 + secs(10), 10.0).unwrap();
        assert!(
            (new - 2000.0).abs() < 1e-9,
            "200 work/s x 10 s target: {new}"
        );
        // Shares for the old job keep arriving until the next job; the estimate is in
        // work, so they do not move the difficulty again.
        for i in 201..=400u64 {
            v.on_share(t0 + Duration::from_millis(i * 50), 10.0);
        }
        assert_eq!(v.evaluate(t0 + secs(20), 2000.0), None);
    }

    #[test]
    fn a_burst_of_few_shares_does_not_jump() {
        let t0 = Instant::now();
        let mut v = Vardiff::new(cfg(), t0);
        // Seven lucky shares in six seconds look like 11x the target rate, but that is
        // below the count that makes a measurement.
        for i in 1..=7u64 {
            v.on_share(t0 + secs(i - 1), 1000.0);
        }
        assert_eq!(v.evaluate(t0 + secs(6), 1000.0), None);
    }

    #[test]
    fn fast_jump_respects_the_maximum() {
        let t0 = Instant::now();
        let mut v = Vardiff::new(cfg(), t0);
        for i in 1..=100u64 {
            v.on_share(t0 + Duration::from_millis(i * 100), 50_000.0);
        }
        assert_eq!(v.evaluate(t0 + secs(10), 50_000.0), Some(100_000.0));
    }

    #[test]
    fn within_variance_is_stable() {
        let t0 = Instant::now();
        let mut v = Vardiff::new(cfg(), t0);
        shares_every(&mut v, t0, 0, 60, 10, 1000.0); // exactly on target
        assert_eq!(v.evaluate(t0 + secs(60), 1000.0), None);
    }

    #[test]
    fn too_many_shares_raises_by_at_most_2x() {
        let t0 = Instant::now();
        let mut v = Vardiff::new(cfg(), t0);
        shares_every(&mut v, t0, 0, 60, 2, 1000.0); // 5x too fast
        let new = v.evaluate(t0 + secs(60), 1000.0).unwrap();
        assert!((new - 2000.0).abs() < 1e-9, "capped at 2x: {new}");
    }

    #[test]
    fn a_few_big_shares_do_not_raise() {
        let t0 = Instant::now();
        let mut v = Vardiff::new(cfg(), t0);
        // 5 shares at 4000 in a minute is 3x the target work rate, but five shares is
        // luck, not a measurement: wait for more before raising.
        shares_every(&mut v, t0, 0, 60, 12, 4000.0);
        assert_eq!(v.evaluate(t0 + secs(60), 1000.0), None);
        // With enough shares behind the same rate, the raise happens (capped at 2x).
        shares_every(&mut v, t0, 60, 60, 12, 4000.0);
        assert_eq!(v.evaluate(t0 + secs(120), 1000.0), Some(2000.0));
    }

    #[test]
    fn no_shares_lowers_by_4x() {
        let t0 = Instant::now();
        let mut v = Vardiff::new(cfg(), t0);
        assert_eq!(v.evaluate(t0 + secs(60), 1000.0), Some(250.0));
        // Still quiet a minute later: another 4x step, and never more often than that.
        assert_eq!(v.evaluate(t0 + secs(90), 250.0), None);
        assert_eq!(v.evaluate(t0 + secs(120), 250.0), Some(62.5));
    }

    #[test]
    fn slightly_slow_lowers_gently() {
        let t0 = Instant::now();
        let mut v = Vardiff::new(cfg(), t0);
        shares_every(&mut v, t0, 0, 60, 20, 1000.0); // half the target rate
        assert_eq!(v.evaluate(t0 + secs(60), 1000.0), Some(500.0));
    }

    #[test]
    fn respects_bounds() {
        let t0 = Instant::now();
        let mut v = Vardiff::new(cfg(), t0);
        shares_every(&mut v, t0, 0, 60, 1, 60_000.0);
        assert_eq!(v.evaluate(t0 + secs(60), 60_000.0), Some(100_000.0));
    }

    #[test]
    fn shares_at_mixed_difficulties_measure_work_not_count() {
        let t0 = Instant::now();
        let mut v = Vardiff::new(cfg(), t0);
        // 6 shares at 4000 in a minute is the work of 24 shares at 1000: way over target.
        shares_every(&mut v, t0, 0, 60, 10, 4000.0);
        shares_every(&mut v, t0, 60, 60, 10, 4000.0);
        let new = v.evaluate(t0 + secs(120), 1000.0).unwrap();
        assert!((new - 2000.0).abs() < 1e-9, "{new}");
    }

    /// A steady miner converges on one difficulty and then stops being retargeted, which
    /// is the whole point: the old 60-second count estimate swung 2-4x every minute.
    #[test]
    fn steady_miner_converges_without_oscillating() {
        let t0 = Instant::now();
        let mut v = Vardiff::new(cfg(), t0);
        // Hashrate such that difficulty 3000 gives exactly one share per 10 s.
        let work_per_second = 300.0;
        let mut diff: f64 = 1000.0;
        let mut changes = Vec::new();
        let mut next_share = 0.0;
        let mut t = 0.0;
        while t < 1800.0 {
            t += 1.0;
            let now = t0 + Duration::from_secs_f64(t);
            while next_share <= t {
                v.on_share(t0 + Duration::from_secs_f64(next_share), diff);
                next_share += diff / work_per_second;
            }
            if let Some(d) = v.evaluate(now, diff) {
                changes.push((t as u64, d));
                diff = d;
            }
        }
        assert!(
            (diff - 3000.0).abs() / 3000.0 < 0.3,
            "settled at {diff}: {changes:?}"
        );
        assert!(changes.len() <= 3, "retargeted too often: {changes:?}");
        // Nothing moves after the first few minutes.
        assert!(changes.iter().all(|(t, _)| *t <= 600), "{changes:?}");
    }
}
