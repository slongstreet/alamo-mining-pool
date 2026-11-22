//! Stratum listener and vardiff configuration: plain serde data, shared by the pool-wide
//! `[stratum]` section and each parent coin's optional `[coins.<key>.stratum]` section.

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

/// Stratum listener and difficulty settings.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StratumConfig {
    /// Address to listen on.
    pub listen: SocketAddr,
    /// Variable-difficulty settings. When the table is absent the built-in defaults apply;
    /// when present it must name every field.
    #[serde(default)]
    pub vardiff: VardiffConfig,
}

/// Variable-difficulty settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// Minimum seconds between difficulty changes. The work-rate estimate behind each
    /// change spans four times this.
    pub retarget_seconds: f64,
    /// Only retarget when the estimated optimal difficulty differs from the current one
    /// by more than this percentage.
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
    /// Check that the settings are internally consistent, naming fields under
    /// `stratum.vardiff`.
    pub fn validate(&self) -> Result<(), String> {
        self.validate_at("stratum.vardiff")
    }

    /// [`validate`](Self::validate), naming fields under `path` (the config path of the
    /// table, such as `coins.ltc.stratum.vardiff`).
    pub fn validate_at(&self, path: &str) -> Result<(), String> {
        if self.min_difficulty <= 0.0 {
            return Err(format!("{path}.min_difficulty must be positive"));
        }
        if self.max_difficulty < self.min_difficulty {
            return Err(format!("{path}.max_difficulty must be >= min_difficulty"));
        }
        if self.initial_difficulty < self.min_difficulty
            || self.initial_difficulty > self.max_difficulty
        {
            return Err(format!(
                "{path}.initial_difficulty must lie within [min, max]"
            ));
        }
        if self.target_share_seconds <= 0.0 || self.retarget_seconds <= 0.0 {
            return Err(format!("{path} timing values must be positive"));
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
        let err = cfg.validate_at("coins.ltc.stratum.vardiff").unwrap_err();
        assert!(
            err.starts_with("coins.ltc.stratum.vardiff.initial_difficulty"),
            "{err}"
        );
    }
}
