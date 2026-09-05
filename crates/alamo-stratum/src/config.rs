//! Stratum server configuration.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

/// Stratum listener and difficulty settings.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StratumConfig {
    /// Address to listen on.
    pub listen: SocketAddr,
    /// Variable-difficulty settings.
    #[serde(default)]
    pub vardiff: VardiffConfig,
}

/// Variable-difficulty settings.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VardiffConfig {
    /// Difficulty a new connection starts at.
    pub initial_difficulty: f64,
    /// Lowest difficulty vardiff will assign.
    pub min_difficulty: f64,
    /// Highest difficulty vardiff will assign.
    pub max_difficulty: f64,
    /// Desired seconds between shares from one worker.
    pub target_share_seconds: f64,
    /// Seconds between difficulty re-evaluations.
    pub retarget_seconds: f64,
    /// Only retarget when the observed interval differs by more than this percentage.
    pub variance_percent: f64,
}

impl Default for VardiffConfig {
    fn default() -> Self {
        Self {
            initial_difficulty: 65_536.0,
            min_difficulty: 1_024.0,
            max_difficulty: 16_777_216.0,
            target_share_seconds: 10.0,
            retarget_seconds: 60.0,
            variance_percent: 30.0,
        }
    }
}

impl VardiffConfig {
    /// Check that the settings are internally consistent.
    pub fn validate(&self) -> Result<(), String> {
        if self.min_difficulty <= 0.0 {
            return Err("stratum.vardiff.min_difficulty must be positive".into());
        }
        if self.max_difficulty < self.min_difficulty {
            return Err("stratum.vardiff.max_difficulty must be >= min_difficulty".into());
        }
        if self.initial_difficulty < self.min_difficulty
            || self.initial_difficulty > self.max_difficulty
        {
            return Err("stratum.vardiff.initial_difficulty must lie within [min, max]".into());
        }
        if self.target_share_seconds <= 0.0 || self.retarget_seconds <= 0.0 {
            return Err("stratum.vardiff timing values must be positive".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate() {
        VardiffConfig::default().validate().unwrap();
    }

    #[test]
    fn rejects_initial_outside_range() {
        let cfg = VardiffConfig {
            initial_difficulty: 1.0,
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }
}
