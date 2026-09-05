//! Block odds: the probability math behind the dashboard.
//!
//! Finding a block is a Poisson process. With pool hashrate `H` (hashes per second) and
//! network difficulty `D`, the expected number of hashes per block is `D * 2^32`, so the
//! block rate is `λ = H / (D * 2^32)` blocks per second.

use serde::{Deserialize, Serialize};

/// Hashes expected per difficulty unit: 2^32.
pub const HASHES_PER_DIFF1: f64 = 4_294_967_296.0;

const HOUR: f64 = 3_600.0;
const DAY: f64 = 24.0 * HOUR;
const WEEK: f64 = 7.0 * DAY;
const MONTH: f64 = 30.0 * DAY;
const YEAR: f64 = 365.25 * DAY;

/// Expected number of hashes needed to find one block at `difficulty`.
pub fn expected_hashes(difficulty: f64) -> f64 {
    difficulty * HASHES_PER_DIFF1
}

/// Expected blocks per second for a given hashrate and network difficulty.
pub fn block_rate(hashrate: f64, difficulty: f64) -> f64 {
    if hashrate <= 0.0 || difficulty <= 0.0 {
        return 0.0;
    }
    hashrate / expected_hashes(difficulty)
}

/// Probability of finding at least one block within `seconds`.
pub fn probability_within(hashrate: f64, difficulty: f64, seconds: f64) -> f64 {
    let rate = block_rate(hashrate, difficulty);
    if rate == 0.0 || seconds <= 0.0 {
        return 0.0;
    }
    1.0 - (-rate * seconds).exp()
}

/// Expected seconds until the next block. Infinite when hashrate is zero.
pub fn expected_seconds_to_block(hashrate: f64, difficulty: f64) -> f64 {
    let rate = block_rate(hashrate, difficulty);
    if rate == 0.0 {
        f64::INFINITY
    } else {
        1.0 / rate
    }
}

/// Luck as a percentage: expected work over actual work. 100 is exactly average,
/// above 100 is lucky.
pub fn luck_percent(expected_shares: f64, actual_shares: f64) -> f64 {
    if actual_shares <= 0.0 {
        return 100.0;
    }
    expected_shares / actual_shares * 100.0
}

/// A summary of block odds for the standard dashboard horizons.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct OddsSummary {
    /// Pool hashrate used for the calculation, in hashes per second.
    pub hashrate: f64,
    /// Network difficulty used for the calculation.
    pub difficulty: f64,
    /// Probability of a block within one hour.
    pub p_hour: f64,
    /// Probability of a block within one day.
    pub p_day: f64,
    /// Probability of a block within one week.
    pub p_week: f64,
    /// Probability of a block within 30 days.
    pub p_month: f64,
    /// Probability of a block within one year.
    pub p_year: f64,
    /// Expected seconds to the next block; `None` when the hashrate is zero.
    pub expected_seconds: Option<f64>,
}

impl OddsSummary {
    /// Compute the summary for a hashrate and network difficulty.
    pub fn compute(hashrate: f64, difficulty: f64) -> Self {
        Self {
            hashrate,
            difficulty,
            p_hour: probability_within(hashrate, difficulty, HOUR),
            p_day: probability_within(hashrate, difficulty, DAY),
            p_week: probability_within(hashrate, difficulty, WEEK),
            p_month: probability_within(hashrate, difficulty, MONTH),
            p_year: probability_within(hashrate, difficulty, YEAR),
            expected_seconds: Some(expected_seconds_to_block(hashrate, difficulty))
                .filter(|s| s.is_finite()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn one_expected_block_per_day() {
        let difficulty = 1_000_000.0;
        let hashrate = expected_hashes(difficulty) / DAY;
        assert!(approx(
            expected_seconds_to_block(hashrate, difficulty),
            DAY,
            1e-6
        ));
        // P(at least one in one mean interval) = 1 - 1/e
        assert!(approx(
            probability_within(hashrate, difficulty, DAY),
            1.0 - (-1.0f64).exp(),
            1e-12
        ));
    }

    #[test]
    fn zero_hashrate_has_no_chance() {
        assert_eq!(probability_within(0.0, 1e6, YEAR), 0.0);
        assert_eq!(expected_seconds_to_block(0.0, 1e6), f64::INFINITY);
    }

    #[test]
    fn probabilities_are_monotonic_in_time() {
        let s = OddsSummary::compute(1e9, 3e7);
        assert!(s.p_hour < s.p_day && s.p_day < s.p_week);
        assert!(s.p_week < s.p_month && s.p_month < s.p_year);
        assert!(s.p_year <= 1.0);
        assert!(s.expected_seconds.unwrap() > 0.0);
        assert_eq!(OddsSummary::compute(0.0, 1e6).expected_seconds, None);
    }

    #[test]
    fn luck() {
        assert_eq!(luck_percent(100.0, 100.0), 100.0);
        assert_eq!(luck_percent(100.0, 50.0), 200.0);
        assert_eq!(luck_percent(100.0, 0.0), 100.0);
    }
}
