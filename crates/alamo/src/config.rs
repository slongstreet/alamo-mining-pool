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
    /// Read and validate a config file. `${NAME}` anywhere in the file is replaced with
    /// the environment variable `NAME` before parsing, so secrets such as RPC passwords
    /// can stay out of the file. An unset variable is an error that names the variable,
    /// never its value.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let text = expand_env(&raw, |name| std::env::var(name).ok())
            .with_context(|| format!("expanding config {}", path.display()))?;
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

/// Replace every `${NAME}` with `lookup(NAME)`. Names are `[A-Za-z0-9_]+`; anything else
/// after `${` is left untouched so TOML that happens to contain `${` still parses.
/// Comment lines are copied through unchanged so they can document the syntax.
fn expand_env(text: &str, lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<String> {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split_inclusive('\n').enumerate() {
        if line.trim_start().starts_with('#') {
            out.push_str(line);
        } else {
            expand_line(line, &lookup, &mut out)
                .with_context(|| format!("config line {}", i + 1))?;
        }
    }
    Ok(out)
}

fn expand_line(
    line: &str,
    lookup: &impl Fn(&str) -> Option<String>,
    out: &mut String,
) -> anyhow::Result<()> {
    let mut rest = line;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let name_len = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(after.len());
        let name = &after[..name_len];
        if name.is_empty() || !after[name_len..].starts_with('}') {
            out.push_str("${");
            rest = after;
            continue;
        }
        let value =
            lookup(name).with_context(|| format!("environment variable {name} is not set"))?;
        out.push_str(&value);
        rest = &after[name_len + 1..];
    }
    out.push_str(rest);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_env_references() {
        let env = |name: &str| match name {
            "HOST" => Some("10.21.42.10".to_string()),
            "PASS" => Some("s3cret".to_string()),
            _ => None,
        };
        let text = "url = \"http://${HOST}:9332\"\npass = \"${PASS}\"\nplain = \"$notavar ${ x}\"";
        let out = expand_env(text, env).unwrap();
        assert_eq!(
            out,
            "url = \"http://10.21.42.10:9332\"\npass = \"s3cret\"\nplain = \"$notavar ${ x}\""
        );
        let err = expand_env("x = \"${MISSING_VAR}\"", env).unwrap_err();
        assert!(format!("{err:#}").contains("MISSING_VAR"), "{err:#}");
        assert_eq!(expand_env("no refs", env).unwrap(), "no refs");
        assert_eq!(
            expand_env("# use ${MISSING_VAR}\nx = 1\n", env).unwrap(),
            "# use ${MISSING_VAR}\nx = 1\n"
        );
    }

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
