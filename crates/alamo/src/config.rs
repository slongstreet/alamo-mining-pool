//! Top-level configuration file.

use alamo_coins::CoinConfig;
use alamo_stratum::StratumConfig;
use alamo_web::WebConfig;
use anyhow::{bail, Context};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The whole `alamo.toml`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Pool identity and storage.
    pub pool: PoolConfig,
    /// Stratum server.
    pub stratum: StratumConfig,
    /// Dashboard and API server.
    pub web: WebConfig,
    /// Coins keyed by short name (`ltc`, `doge`).
    #[serde(default)]
    pub coins: BTreeMap<String, CoinConfig>,
}

/// Pool identity and storage.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolConfig {
    /// Display name shown on the dashboard.
    pub name: String,
    /// Directory for the database and other state.
    pub data_dir: PathBuf,
}

impl Config {
    /// Read and validate a config file.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let config: Config =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    /// Path of the SQLite database.
    pub fn database_path(&self) -> PathBuf {
        self.pool.data_dir.join("alamo.db")
    }

    fn validate(&self) -> anyhow::Result<()> {
        self.stratum
            .vardiff
            .validate()
            .map_err(anyhow::Error::msg)?;

        let enabled: Vec<&str> = self
            .coins
            .iter()
            .filter(|(_, c)| c.enabled)
            .map(|(k, _)| k.as_str())
            .collect();
        if enabled.is_empty() {
            bail!("at least one coin must be enabled under [coins]");
        }
        for (key, coin) in &self.coins {
            if alamo_coins::builtin(key).is_none() {
                bail!("unknown coin '{key}' under [coins]; supported: ltc, doge");
            }
            if let Some(parent) = &coin.merge_mined_with {
                let Some(parent_cfg) = self.coins.get(parent) else {
                    bail!("coin '{key}' is merge-mined with '{parent}', which is not configured");
                };
                if coin.enabled && !parent_cfg.enabled {
                    bail!("coin '{key}' is merge-mined with '{parent}', which is disabled");
                }
                if parent_cfg.merge_mined_with.is_some() {
                    bail!("coin '{parent}' cannot be both a parent and an aux chain");
                }
            }
            if coin.rpc_url.is_empty() || coin.fallback_address.is_empty() {
                bail!("coin '{key}' needs rpc_url and fallback_address");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses_and_validates() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/alamo.example.toml");
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.coins.len(), 2);
        assert_eq!(cfg.coins["doge"].merge_mined_with.as_deref(), Some("ltc"));
        assert_eq!(cfg.stratum.listen.port(), 3333);
    }

    #[test]
    fn rejects_unknown_parent() {
        let text = r#"
[pool]
name = "x"
data_dir = "./data"
[stratum]
listen = "127.0.0.1:3333"
[web]
listen = "127.0.0.1:8080"
[coins.doge]
merge_mined_with = "ltc"
rpc_url = "http://x"
rpc_user = "u"
rpc_password = "p"
fallback_address = "D..."
"#;
        let cfg: Config = toml::from_str(text).unwrap();
        assert!(cfg
            .validate()
            .unwrap_err()
            .to_string()
            .contains("not configured"));
    }
}
