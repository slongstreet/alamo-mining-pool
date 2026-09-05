//! Variable difficulty: keep each worker submitting roughly one share per target interval.

use crate::config::VardiffConfig;
use std::time::{Duration, Instant};

/// Per-session vardiff state.
#[derive(Debug)]
pub struct Vardiff {
    cfg: VardiffConfig,
    window_start: Instant,
    shares: u32,
}

impl Vardiff {
    /// Start tracking from `now`.
    pub fn new(cfg: VardiffConfig, now: Instant) -> Self {
        Self {
            cfg,
            window_start: now,
            shares: 0,
        }
    }

    /// The configured difficulty bounds.
    pub fn clamp(&self, difficulty: f64) -> f64 {
        difficulty.clamp(self.cfg.min_difficulty, self.cfg.max_difficulty)
    }

    /// Record an accepted share.
    pub fn on_share(&mut self) {
        self.shares += 1;
    }

    /// Re-evaluate. Returns a new difficulty when a change is warranted.
    pub fn evaluate(&mut self, now: Instant, current: f64) -> Option<f64> {
        let elapsed = now.duration_since(self.window_start);
        if elapsed < Duration::from_secs_f64(self.cfg.retarget_seconds) {
            return None;
        }
        let target = self.cfg.target_share_seconds;
        // With no shares at all, treat the whole window as one interval: a lower bound
        // on how slow the worker is, so difficulty only goes down.
        let observed = elapsed.as_secs_f64() / f64::from(self.shares.max(1));
        self.window_start = now;
        self.shares = 0;

        let deviation = ((observed - target) / target).abs() * 100.0;
        if deviation <= self.cfg.variance_percent {
            return None;
        }
        // Move toward the target interval, but at most 4x per step to avoid overshoot.
        let ratio = (target / observed).clamp(0.25, 4.0);
        let proposed = self.clamp(current * ratio);
        if (proposed - current).abs() / current < 1e-9 {
            None
        } else {
            Some(proposed)
        }
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

    #[test]
    fn nothing_before_retarget_interval() {
        let t0 = Instant::now();
        let mut v = Vardiff::new(cfg(), t0);
        v.on_share();
        assert_eq!(v.evaluate(t0 + Duration::from_secs(30), 1000.0), None);
    }

    #[test]
    fn too_many_shares_raises_difficulty() {
        let t0 = Instant::now();
        let mut v = Vardiff::new(cfg(), t0);
        for _ in 0..30 {
            v.on_share(); // 30 shares in 60s = 2s interval, target 10s
        }
        let new = v.evaluate(t0 + Duration::from_secs(60), 1000.0).unwrap();
        assert!((new - 4000.0).abs() < 1e-9, "capped at 4x: {new}");
    }

    #[test]
    fn no_shares_lowers_difficulty() {
        let t0 = Instant::now();
        let mut v = Vardiff::new(cfg(), t0);
        let new = v.evaluate(t0 + Duration::from_secs(60), 1000.0).unwrap();
        assert!((250.0..1000.0).contains(&new));
    }

    #[test]
    fn within_variance_is_stable() {
        let t0 = Instant::now();
        let mut v = Vardiff::new(cfg(), t0);
        for _ in 0..6 {
            v.on_share(); // 6 shares in 60s = 10s interval exactly
        }
        assert_eq!(v.evaluate(t0 + Duration::from_secs(60), 1000.0), None);
    }

    #[test]
    fn respects_bounds() {
        let t0 = Instant::now();
        let mut v = Vardiff::new(cfg(), t0);
        for _ in 0..600 {
            v.on_share();
        }
        assert_eq!(
            v.evaluate(t0 + Duration::from_secs(60), 50_000.0),
            Some(100_000.0)
        );
    }
}
