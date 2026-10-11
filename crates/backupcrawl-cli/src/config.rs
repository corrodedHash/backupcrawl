//! Config file loading.
//!
//! Config lives at `~/.config/backupcrawl/config.toml` (XDG aware). The
//! `[ignore]` table holds path globs and `[plugins]` holds plugin selection,
//! ordering, trust hashes and per-plugin tables (`[plugins.git]`). Unknown
//! top-level keys are rejected so typos do not get silently ignored.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// Application config.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Ignore rules.
    #[serde(default)]
    pub ignore: IgnoreConfig,

    /// Plugin settings.
    ///
    /// Read from M1.3 on (discovery, trust, ordering); frozen here for the
    /// parallel M1 tasks.
    #[allow(dead_code)]
    #[serde(default)]
    pub plugins: PluginsConfig,
}

/// `[ignore]` table.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IgnoreConfig {
    /// Glob patterns excluded from the crawl, matched against full paths.
    #[serde(default)]
    pub paths: Vec<String>,
}

/// `[plugins]` table.
///
/// Unknown keys inside this table are the per-plugin config tables
/// (`[plugins.git]`, `[plugins.pacman]`, ...) and are kept verbatim in
/// [`extra`](PluginsConfig::extra); the matching detector receives its table
/// as `scan-info.config` (M1.3/M1.4 wire it through).
#[derive(Debug, Default, Deserialize)]
#[allow(dead_code)] // fields are consumed by M1.3/M1.5/M1.6
pub struct PluginsConfig {
    /// Explicitly enabled plugins; empty means "all discovered".
    #[serde(default)]
    pub enabled: Vec<String>,

    /// Order override for enabled detectors, before the manifest priorities.
    #[serde(default)]
    pub order: Vec<String>,

    /// Trusted manifest hashes, plugin name -> `"sha256:..."` (M1.5).
    #[serde(default)]
    pub trust: BTreeMap<String, String>,

    /// Per-plugin config tables, e.g. `[plugins.git]`.
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Table>,
}

impl Config {
    /// The default config location, `~/.config/backupcrawl/config.toml`.
    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|dir| dir.join("backupcrawl").join("config.toml"))
    }

    /// Loads the config from `path`.
    ///
    /// A missing file is only an error when `required` is set (an explicit
    /// `--config` path); the default location may simply not exist yet.
    pub fn load(path: &Path, required: bool) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(contents) => toml::from_str(&contents)
                .with_context(|| format!("invalid config file {}", path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound && !required => {
                Ok(Self::default())
            }
            Err(err) => {
                Err(err).with_context(|| format!("cannot read config file {}", path.display()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml_str: &str) -> Config {
        toml::from_str(toml_str).expect("config must parse")
    }

    #[test]
    fn plugins_table_parses() {
        let config = parse(
            r#"
            [plugins]
            enabled = ["git", "pacman"]
            order = ["git", "pacman"]
            trust = { git = "sha256:abc", pacman = "sha256:def" }

            [plugins.git]
            no-upstream = "unknown"
            ancestor-repos = true

            [plugins.pacman]
            check-level = "metadata"
            "#,
        );

        assert_eq!(config.plugins.enabled, ["git", "pacman"]);
        assert_eq!(config.plugins.order, ["git", "pacman"]);
        assert_eq!(
            config.plugins.trust.get("git").map(String::as_str),
            Some("sha256:abc")
        );
        assert_eq!(
            config.plugins.trust.get("pacman").map(String::as_str),
            Some("sha256:def")
        );

        let git = config.plugins.extra.get("git").expect("plugins.git table");
        assert_eq!(
            git.get("no-upstream").and_then(|v| v.as_str()),
            Some("unknown")
        );
        assert_eq!(
            git.get("ancestor-repos").and_then(|v| v.as_bool()),
            Some(true)
        );

        let pacman = config
            .plugins
            .extra
            .get("pacman")
            .expect("plugins.pacman table");
        assert_eq!(
            pacman.get("check-level").and_then(|v| v.as_str()),
            Some("metadata")
        );
    }

    #[test]
    fn plugins_default_to_empty() {
        let config = parse("[ignore]\npaths = [\"/proc\"]\n");

        assert_eq!(config.ignore.paths, ["/proc"]);
        assert!(config.plugins.enabled.is_empty());
        assert!(config.plugins.order.is_empty());
        assert!(config.plugins.trust.is_empty());
        assert!(config.plugins.extra.is_empty());
    }

    #[test]
    fn plugins_table_without_settings_keeps_only_extra() {
        let config = parse("[plugins.git]\nreport-ignored = false\n");

        assert!(config.plugins.enabled.is_empty());
        assert_eq!(config.plugins.extra.len(), 1);
        assert_eq!(
            config.plugins.extra["git"]
                .get("report-ignored")
                .and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn non_table_plugins_key_is_rejected() {
        let err = toml::from_str::<Config>("[plugins]\nenabled-by-typo = [\"git\"]\n")
            .expect_err("a non-table unknown key must not be accepted as per-plugin config");

        assert!(err.to_string().contains("invalid type"), "got: {err}");
    }

    #[test]
    fn unknown_top_level_key_is_rejected() {
        toml::from_str::<Config>("[plugin]\nenabled = [\"git\"]\n")
            .expect_err("unknown top-level tables must be rejected");
    }
}
