//! Top-level configuration file.

use alamo_coins::CoinConfig;
use alamo_core::Algorithm;
use alamo_stratum::{StratumConfig, VardiffConfig};
use alamo_web::WebConfig;
use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The whole `alamo.toml`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Pool identity and storage.
    pub pool: PoolConfig,
    /// Stratum listener and vardiff for every parent coin without its own
    /// `[coins.<key>.stratum]` section.
    pub stratum: StratumConfig,
    /// Dashboard and API server.
    pub web: WebConfig,
    /// Coins keyed by short name (`ltc`, `doge`), lower case once loaded.
    #[serde(default)]
    pub coins: BTreeMap<String, CoinConfig>,
}

/// Pool identity and storage.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
        Self::parse(&text).with_context(|| format!("config {}", path.display()))
    }

    /// Parse, normalize, and validate config text (after `${NAME}` expansion).
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let mut config: Config = toml::from_str(text).context("parsing")?;
        config.normalize()?;
        config.validate()?;
        Ok(config)
    }

    /// Lower-case every coin key and `merge_mined_with` reference, once, so everything
    /// downstream (lane ids, stored setting keys, database rows) sees one spelling.
    fn normalize(&mut self) -> anyhow::Result<()> {
        let mut coins = BTreeMap::new();
        let mut spelled: BTreeMap<String, String> = BTreeMap::new();
        for (key, mut coin) in std::mem::take(&mut self.coins) {
            let lower = key.to_ascii_lowercase();
            if let Some(parent) = &mut coin.merge_mined_with {
                *parent = parent.to_ascii_lowercase();
            }
            if let Some(first) = spelled.get(&lower) {
                bail!("coins '{first}' and '{key}' differ only by case; configure each coin once");
            }
            spelled.insert(lower.clone(), key);
            coins.insert(lower, coin);
        }
        self.coins = coins;
        Ok(())
    }

    /// One lane per enabled parent coin, ordered by key.
    pub fn lanes(&self) -> Vec<LaneConfig> {
        self.coins
            .iter()
            .filter(|(_, c)| c.enabled && c.merge_mined_with.is_none())
            .map(|(key, coin)| {
                let stratum = coin.stratum.clone().unwrap_or_else(|| self.stratum.clone());
                // Validation refuses unknown keys, so the fallback is never used.
                let algorithm = alamo_coins::builtin(key)
                    .map(|c| c.algorithm())
                    .unwrap_or(Algorithm::Scrypt);
                LaneConfig {
                    key: key.clone(),
                    aux: self
                        .coins
                        .iter()
                        .filter(|(_, c)| c.enabled && c.merge_mined_with.as_deref() == Some(key))
                        .map(|(k, _)| k.clone())
                        .collect(),
                    own_stratum: coin.stratum.is_some(),
                    algorithm,
                    vardiff: stratum
                        .vardiff
                        .clone()
                        .unwrap_or_else(|| VardiffConfig::default_for(algorithm)),
                    stratum,
                }
            })
            .collect()
    }

    /// Index into [`lanes`](Self::lanes) of the primary lane: the one parent that uses the
    /// default `[stratum]` section, or, if none or several do, the first by key. The
    /// primary lane is what the single-lane API surface describes.
    pub fn primary_lane_index(lanes: &[LaneConfig]) -> usize {
        let mut default = lanes.iter().enumerate().filter(|(_, l)| !l.own_stratum);
        match (default.next(), default.next()) {
            (Some((i, _)), None) => i,
            _ => 0,
        }
    }

    /// The primary lane (see [`primary_lane_index`](Self::primary_lane_index)). A
    /// validated config always has one.
    pub fn primary_lane(&self) -> Option<LaneConfig> {
        let lanes = self.lanes();
        let i = Self::primary_lane_index(&lanes);
        lanes.into_iter().nth(i)
    }

    /// Path of the SQLite database.
    pub fn database_path(&self) -> PathBuf {
        self.pool.data_dir.join("alamo.db")
    }

    /// The effective configuration as TOML, with every RPC password replaced by a
    /// placeholder, for the dashboard's diagnostics view.
    pub fn redacted_toml(&self) -> String {
        let mut copy = self.clone();
        for coin in copy.coins.values_mut() {
            coin.rpc_password = "<redacted>".to_string();
        }
        toml::to_string_pretty(&copy).unwrap_or_else(|err| format!("# could not render: {err}"))
    }

    fn validate(&self) -> anyhow::Result<()> {
        if let Some(vardiff) = &self.stratum.vardiff {
            vardiff.validate().map_err(anyhow::Error::msg)?;
        }

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
                bail!(
                    "unknown coin '{key}' under [coins]; supported: {}",
                    alamo_coins::BUILTIN_KEYS.join(", ")
                );
            }
            if let (Some(parent), Some(_)) = (&coin.merge_mined_with, &coin.stratum) {
                bail!(
                    "coin '{key}' is merge-mined with '{parent}' and cannot have its own \
                     [coins.{key}.stratum] section; its miners connect to '{parent}'"
                );
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

        let lanes = self.lanes();
        if lanes.is_empty() {
            bail!("no enabled parent coin configured: at least one enabled coin must not be merge-mined");
        }
        for lane in &lanes {
            if lane.own_stratum {
                lane.vardiff
                    .validate_at(&lane.vardiff_path())
                    .map_err(anyhow::Error::msg)?;
            }
        }
        for (i, a) in lanes.iter().enumerate() {
            for b in &lanes[i + 1..] {
                if listeners_collide(a.stratum.listen, b.stratum.listen) {
                    bail!(
                        "coins '{}' and '{}' would both listen for stratum on {}; give one \
                         its own [coins.<key>.stratum] listen address",
                        a.key,
                        b.key,
                        b.stratum.listen
                    );
                }
            }
        }
        Ok(())
    }
}

/// One lane as configured: a parent coin, the aux coins merge-mined with it, and the
/// stratum listener and vardiff it serves miners with.
#[derive(Clone, Debug)]
pub struct LaneConfig {
    /// The parent coin's config key, which is also the lane id.
    pub key: String,
    /// Enabled coins merge-mined with the parent, by key.
    pub aux: Vec<String>,
    /// Whether the parent has its own `[coins.<key>.stratum]` section; otherwise
    /// `stratum` is a copy of the pool-wide `[stratum]`.
    pub own_stratum: bool,
    /// The stratum section in force for this lane.
    pub stratum: StratumConfig,
    /// The parent coin's proof-of-work algorithm.
    pub algorithm: Algorithm,
    /// The vardiff the file gives this lane: the table in `stratum` if it has one, else
    /// the built-in default for `algorithm`. A lane with its own `[coins.<key>.stratum]`
    /// section never inherits `[stratum.vardiff]`, which is in another algorithm's units
    /// when the lanes differ.
    pub vardiff: VardiffConfig,
}

impl LaneConfig {
    /// Config path of the vardiff table this lane uses; stored overrides of it are keyed
    /// `<path>.<field>`.
    pub fn vardiff_path(&self) -> String {
        if self.own_stratum {
            format!("coins.{}.stratum.vardiff", self.key)
        } else {
            "stratum.vardiff".to_string()
        }
    }

    /// Whether the lane merge-mines, which costs coinbase bytes for the aux commitment.
    pub fn merge_mines(&self) -> bool {
        !self.aux.is_empty()
    }

    /// The lane's file vardiff: the `vardiff` field.
    pub fn vardiff(&self) -> &VardiffConfig {
        &self.vardiff
    }
}

/// Whether two listen addresses would fight over one port. Port 0 asks the OS for a free
/// port, so it never collides; an unspecified address (`0.0.0.0`, `::`) overlaps every
/// address of its family.
fn listeners_collide(a: std::net::SocketAddr, b: std::net::SocketAddr) -> bool {
    a.port() != 0
        && a.port() == b.port()
        && a.is_ipv4() == b.is_ipv4()
        && (a.ip() == b.ip() || a.ip().is_unspecified() || b.ip().is_unspecified())
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
    use alamo_coins::Coin;

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
    fn regtest_config_parses_with_a_bitcoin_lane() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/alamo.regtest.toml");
        let cfg = Config::load(&path).unwrap();
        let lanes = cfg.lanes();
        let btc = lanes.iter().find(|l| l.key == "btc").unwrap();
        assert_eq!(btc.algorithm, Algorithm::Sha256d);
        assert_eq!(btc.stratum.listen.port(), 3334);
        let params = alamo_coins::Bitcoin.address_params(alamo_coins::Chain::Regtest);
        alamo_core::address::payout_script(&cfg.coins["btc"].fallback_address, &params).unwrap();
    }

    #[test]
    fn redacted_toml_hides_passwords() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/alamo.example.toml");
        let cfg = Config::load(&path).unwrap();
        let text = cfg.redacted_toml();
        assert!(!text.contains("change-me"), "{text}");
        assert!(text.contains("rpc_password = \"<redacted>\""), "{text}");
        assert!(text.contains("[coins.doge]"), "{text}");
        let again: Config = toml::from_str(&text).unwrap();
        assert_eq!(again.coins["ltc"].rpc_url, cfg.coins["ltc"].rpc_url);
    }

    const HEAD: &str = r#"
[pool]
name = "x"
data_dir = "./data"
[stratum]
listen = "0.0.0.0:3333"
[web]
listen = "127.0.0.1:8080"
"#;

    fn coin(key: &str, extra: &str) -> String {
        format!(
            "[coins.{key}]\nrpc_url = \"http://x\"\nrpc_user = \"u\"\nrpc_password = \"p\"\n\
             fallback_address = \"a\"\n{extra}\n"
        )
    }

    fn parse(coins: &[String]) -> anyhow::Result<Config> {
        Config::parse(&format!("{HEAD}{}", coins.concat()))
    }

    fn err(coins: &[String]) -> String {
        format!("{:#}", parse(coins).unwrap_err())
    }

    #[test]
    fn a_parent_without_a_section_uses_the_default_stratum() {
        let cfg = parse(&[coin("ltc", ""), coin("doge", "merge_mined_with = \"ltc\"")]).unwrap();
        let lanes = cfg.lanes();
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].key, "ltc");
        assert_eq!(lanes[0].aux, vec!["doge".to_string()]);
        assert!(!lanes[0].own_stratum);
        assert!(lanes[0].merge_mines());
        assert_eq!(lanes[0].stratum.listen.port(), 3333);
        assert_eq!(lanes[0].vardiff(), &VardiffConfig::default());
        assert_eq!(lanes[0].vardiff_path(), "stratum.vardiff");
        assert_eq!(cfg.primary_lane().unwrap().key, "ltc");
    }

    #[test]
    fn a_parent_with_its_own_section_gets_its_own_listener_and_vardiff() {
        let doge = coin(
            "doge",
            r#"[coins.doge.stratum]
listen = "0.0.0.0:3334"
[coins.doge.stratum.vardiff]
initial_difficulty = 8
min_difficulty = 1
max_difficulty = 64
target_share_seconds = 5
retarget_seconds = 30
variance_percent = 20"#,
        );
        let cfg = parse(&[coin("ltc", ""), doge]).unwrap();
        let lanes = cfg.lanes();
        assert_eq!(
            lanes.iter().map(|l| l.key.as_str()).collect::<Vec<_>>(),
            vec!["doge", "ltc"]
        );
        let (doge, ltc) = (&lanes[0], &lanes[1]);
        assert!(doge.own_stratum && !doge.merge_mines());
        assert_eq!(doge.stratum.listen.port(), 3334);
        assert_eq!(doge.vardiff().initial_difficulty, 8.0);
        assert_eq!(doge.vardiff_path(), "coins.doge.stratum.vardiff");
        assert_eq!(ltc.stratum.listen.port(), 3333);
        assert_eq!(ltc.vardiff(), &VardiffConfig::default());
        assert_eq!(
            cfg.primary_lane().unwrap().key,
            "ltc",
            "the lane on the default section is primary even when it is not first"
        );

        // Without a vardiff table the section gets the built-in defaults, like [stratum].
        let cfg = parse(&[
            coin("ltc", ""),
            coin("doge", "[coins.doge.stratum]\nlisten = \"0.0.0.0:3334\""),
        ])
        .unwrap();
        assert_eq!(cfg.lanes()[0].vardiff(), &VardiffConfig::default());

        // A partial vardiff table is refused.
        let e = err(&[
            coin("ltc", ""),
            coin(
                "doge",
                "[coins.doge.stratum]\nlisten = \"0.0.0.0:3334\"\n[coins.doge.stratum.vardiff]\nmin_difficulty = 1",
            ),
        ]);
        assert!(e.contains("missing field"), "{e}");
    }

    #[test]
    fn vardiff_defaults_follow_the_lane_algorithm_when_no_table_is_written() {
        let btc = |extra: &str| {
            coin(
                "btc",
                &format!("[coins.btc.stratum]\nlisten = \"0.0.0.0:3334\"\n{extra}"),
            )
        };
        let lane =
            |cfg: &Config, key: &str| cfg.lanes().into_iter().find(|l| l.key == key).unwrap();

        // No table anywhere: each lane gets its own algorithm's built-in values.
        let cfg = parse(&[coin("ltc", ""), btc("")]).unwrap();
        assert_eq!(lane(&cfg, "ltc").vardiff(), &VardiffConfig::SCRYPT);
        assert_eq!(lane(&cfg, "btc").algorithm, Algorithm::Sha256d);
        assert_eq!(lane(&cfg, "btc").vardiff(), &VardiffConfig::SHA256D);
        assert!(cfg.stratum.vardiff.is_none());

        // A BTC-only pool on [stratum] gets the SHA-256d values too.
        let cfg = parse(&[coin("btc", "")]).unwrap();
        assert_eq!(lane(&cfg, "btc").vardiff(), &VardiffConfig::SHA256D);
        assert_eq!(lane(&cfg, "btc").vardiff_path(), "stratum.vardiff");

        // A [stratum.vardiff] table is used as written, even when it spells out the scrypt
        // defaults: absent and default are different things.
        let table = "[stratum.vardiff]\ninitial_difficulty = 65536\nmin_difficulty = 1024\n\
                     max_difficulty = 16777216\ntarget_share_seconds = 10\nretarget_seconds = 60\n\
                     variance_percent = 30\n";
        let cfg = Config::parse(&format!("{HEAD}{table}{}", coin("btc", ""))).unwrap();
        assert_eq!(lane(&cfg, "btc").vardiff(), &VardiffConfig::SCRYPT);
        // ...but it does not reach a lane with its own section, which is in other units.
        let cfg = Config::parse(&format!("{HEAD}{table}{}{}", coin("ltc", ""), btc(""))).unwrap();
        assert_eq!(lane(&cfg, "ltc").vardiff(), &VardiffConfig::SCRYPT);
        assert_eq!(lane(&cfg, "btc").vardiff(), &VardiffConfig::SHA256D);

        // Fractional values are fine in a table.
        let cfg = parse(&[
            coin("ltc", ""),
            btc(
                "[coins.btc.stratum.vardiff]\ninitial_difficulty = 0.5\nmin_difficulty = 0.001\n\
                 max_difficulty = 1e9\ntarget_share_seconds = 10\nretarget_seconds = 60\n\
                 variance_percent = 30",
            ),
        ])
        .unwrap();
        assert_eq!(lane(&cfg, "btc").vardiff().min_difficulty, 0.001);
        assert_eq!(lane(&cfg, "btc").vardiff().initial_difficulty, 0.5);

        // The redacted copy keeps an absent table absent.
        let cfg = parse(&[coin("ltc", ""), btc("")]).unwrap();
        let text = cfg.redacted_toml();
        assert!(!text.contains("vardiff"), "{text}");
        let again = Config::parse(&text).unwrap();
        assert_eq!(lane(&again, "btc").vardiff(), &VardiffConfig::SHA256D);
    }

    #[test]
    fn primary_lane_falls_back_to_the_first_key() {
        let cfg = parse(&[
            coin("ltc", "[coins.ltc.stratum]\nlisten = \"0.0.0.0:3335\""),
            coin("doge", "[coins.doge.stratum]\nlisten = \"0.0.0.0:3334\""),
        ])
        .unwrap();
        assert_eq!(
            cfg.primary_lane().unwrap().key,
            "doge",
            "none on the default"
        );
        // Several on the default section cannot pass validation unless they ask for
        // port 0, but the rule still picks the first.
        let lanes = vec![
            LaneConfig {
                key: "doge".into(),
                aux: vec![],
                own_stratum: false,
                stratum: cfg.stratum.clone(),
                algorithm: Algorithm::Scrypt,
                vardiff: VardiffConfig::default(),
            },
            LaneConfig {
                key: "ltc".into(),
                aux: vec![],
                own_stratum: false,
                stratum: cfg.stratum.clone(),
                algorithm: Algorithm::Scrypt,
                vardiff: VardiffConfig::default(),
            },
        ];
        assert_eq!(Config::primary_lane_index(&lanes), 0);
    }

    #[test]
    fn coin_keys_are_lower_cased_once() {
        let cfg = parse(&[coin("LTC", ""), coin("Doge", "merge_mined_with = \"Ltc\"")]).unwrap();
        assert_eq!(cfg.coins.keys().collect::<Vec<_>>(), vec!["doge", "ltc"]);
        assert_eq!(cfg.coins["doge"].merge_mined_with.as_deref(), Some("ltc"));
        assert_eq!(cfg.lanes()[0].key, "ltc");
        assert_eq!(cfg.lanes()[0].aux, vec!["doge".to_string()]);

        let e = err(&[coin("LTC", ""), coin("ltc", "")]);
        assert!(e.contains("'LTC' and 'ltc' differ only by case"), "{e}");
    }

    #[test]
    fn lane_validation_errors_name_the_coins() {
        // No parent at all.
        let e = err(&[
            coin("doge", "merge_mined_with = \"ltc\""),
            coin("ltc", "enabled = false"),
        ]);
        assert!(e.contains("disabled"), "{e}");
        let e = err(&[coin("ltc", "enabled = false")]);
        assert!(e.contains("at least one coin must be enabled"), "{e}");

        // A stratum section on an aux coin.
        let e = err(&[
            coin("ltc", ""),
            coin(
                "doge",
                "merge_mined_with = \"ltc\"\n[coins.doge.stratum]\nlisten = \"0.0.0.0:3334\"",
            ),
        ]);
        assert!(e.contains("coin 'doge' is merge-mined with 'ltc'"), "{e}");
        assert!(e.contains("[coins.doge.stratum]"), "{e}");

        // Two parents on the default section.
        let e = err(&[coin("ltc", ""), coin("doge", "")]);
        assert!(
            e.contains("coins 'doge' and 'ltc' would both listen for stratum on 0.0.0.0:3333"),
            "{e}"
        );
        // An explicit section that lands on the default address, or overlaps it.
        let e = err(&[
            coin("ltc", ""),
            coin("doge", "[coins.doge.stratum]\nlisten = \"127.0.0.1:3333\""),
        ]);
        assert!(e.contains("coins 'doge' and 'ltc'"), "{e}");
        // Port 0 never collides.
        parse(&[
            coin("ltc", "[coins.ltc.stratum]\nlisten = \"127.0.0.1:0\""),
            coin("doge", "[coins.doge.stratum]\nlisten = \"127.0.0.1:0\""),
        ])
        .unwrap();

        // Every lane's vardiff validates, and the message names its table.
        let e = err(&[
            coin("ltc", ""),
            coin(
                "doge",
                r#"[coins.doge.stratum]
listen = "0.0.0.0:3334"
[coins.doge.stratum.vardiff]
initial_difficulty = 8
min_difficulty = 16
max_difficulty = 64
target_share_seconds = 5
retarget_seconds = 30
variance_percent = 20"#,
            ),
        ]);
        assert!(
            e.contains("coins.doge.stratum.vardiff.initial_difficulty must lie within"),
            "{e}"
        );
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
