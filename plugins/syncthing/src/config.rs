//! Syncthing detector configuration (§7.3 of `docs/rust-rewrite-plan.md`).
//!
//! The `[plugins.syncthing]` table reaches the plugin as a flat list of string
//! attributes ([`backupcrawl_plugin_sdk::Config`]); this module validates it.
//! Invalid values for known keys are a hard `init` error so a typo never
//! silently changes behavior, while unknown keys are ignored (the optional
//! `[config-schema]` is not enforced in v1).
//!
//! # Config keys
//!
//! | Key | Default | Meaning |
//! | --- | --- | --- |
//! | `address` | `<gui><address>` or `127.0.0.1:8384` | REST API base URL/host:port |
//! | `api-key` | `<gui><apikey>` | `X-API-Key` value; the daemon is unreachable without it |
//! | `require-daemon` | `true` | Query the REST API. `false` trusts the marker only |
//! | `assume-marker-only` | `false` | On an API failure, report the marker as `clean` |
//! | `receive-only-changes` | `dirty` | `dirty`, `ahead` or `clean` |
//! | `timeout-ms` | *(host limit)* | Parsed and reserved (the host caps HTTP) |
//! | `config-dir` | auto-detect | Test/debug override for the config directory |
//!
//! `address`, `api-key`, `require-daemon` and `assume-marker-only` mirror the
//! plan's keys. `config-dir` is a test hook, like `git-path` in the git plugin:
//! production discovery walks `$STCONFDIR`, then
//! `~/.local/state/syncthing`, then `~/.config/syncthing`.

use std::path::PathBuf;

use backupcrawl_plugin_sdk::{Config, DetectorError};

/// What a pending receive-only change means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiveOnlyChanges {
    /// Local divergence is unbacked data (the default).
    Dirty,
    /// Local divergence is captured locally but not propagated.
    Ahead,
    /// Treat local divergence as in sync (explicit opt-out).
    Clean,
}

impl ReceiveOnlyChanges {
    /// The config spelling, used in diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dirty => "dirty",
            Self::Ahead => "ahead",
            Self::Clean => "clean",
        }
    }
}

/// Validated syncthing detector configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncthingConfig {
    /// REST API base URL override (`http://host:port`); `None` uses the GUI
    /// address from `config.xml`.
    pub address: Option<String>,
    /// API key override; `None` uses `<gui><apikey>`.
    pub api_key: Option<String>,
    /// Whether a daemon query is required/attempted.
    pub require_daemon: bool,
    /// Whether an API failure falls back to a marker-only `clean` claim.
    pub assume_marker_only: bool,
    /// Status for pending receive-only changes.
    pub receive_only_changes: ReceiveOnlyChanges,
    /// Reserved per-request budget in milliseconds. The `http` host import
    /// enforces the global HTTP limit instead; this key is parsed for parity
    /// with the plan's config surface.
    pub timeout_ms: Option<u64>,
    /// Test/debug override for the config directory.
    pub config_dir: Option<PathBuf>,
}

impl Default for SyncthingConfig {
    fn default() -> Self {
        Self {
            address: None,
            api_key: None,
            require_daemon: true,
            assume_marker_only: false,
            receive_only_changes: ReceiveOnlyChanges::Dirty,
            timeout_ms: None,
            config_dir: None,
        }
    }
}

impl SyncthingConfig {
    /// Parses and validates the `[plugins.syncthing]` table.
    pub fn from_config(config: &Config) -> Result<Self, DetectorError> {
        let mut out = Self::default();
        for attr in config.iter() {
            match attr.key.as_str() {
                "address" => out.address = Some(attr.value.clone()),
                "api-key" => out.api_key = Some(attr.value.clone()),
                "require-daemon" => out.require_daemon = parse_bool("require-daemon", &attr.value)?,
                "assume-marker-only" => {
                    out.assume_marker_only = parse_bool("assume-marker-only", &attr.value)?;
                }
                "receive-only-changes" => {
                    out.receive_only_changes = parse_receive_only_changes(&attr.value)?;
                }
                "timeout-ms" => out.timeout_ms = Some(parse_u64("timeout-ms", &attr.value)?),
                "config-dir" => out.config_dir = Some(PathBuf::from(&attr.value)),
                // Unknown config keys are ignored; the manifest's optional
                // `[config-schema]` is not enforced in v1.
                _ => {}
            }
        }
        Ok(out)
    }
}

fn parse_bool(key: &str, value: &str) -> Result<bool, DetectorError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(DetectorError::new(format!(
            "syncthing config `{key} = \"{other}\"` is invalid; expected \"true\" or \"false\""
        ))),
    }
}

fn parse_receive_only_changes(value: &str) -> Result<ReceiveOnlyChanges, DetectorError> {
    match value {
        "dirty" => Ok(ReceiveOnlyChanges::Dirty),
        "ahead" => Ok(ReceiveOnlyChanges::Ahead),
        "clean" => Ok(ReceiveOnlyChanges::Clean),
        other => Err(DetectorError::new(format!(
            "syncthing config `receive-only-changes = \"{other}\"` is invalid; \
             expected \"dirty\", \"ahead\" or \"clean\""
        ))),
    }
}

fn parse_u64(key: &str, value: &str) -> Result<u64, DetectorError> {
    value.parse().map_err(|_| {
        DetectorError::new(format!(
            "syncthing config `{key} = \"{value}\"` is invalid; expected a non-negative integer"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use backupcrawl_plugin_sdk::Attr;

    fn config(attrs: &[(&str, &str)]) -> Config {
        Config::new(
            attrs
                .iter()
                .map(|(key, value)| Attr::new(*key, *value))
                .collect(),
        )
    }

    #[test]
    fn defaults_match_the_plan() {
        let parsed = SyncthingConfig::from_config(&config(&[])).unwrap();
        assert_eq!(parsed, SyncthingConfig::default());
        assert_eq!(parsed.receive_only_changes, ReceiveOnlyChanges::Dirty);
        assert!(parsed.require_daemon);
        assert!(!parsed.assume_marker_only);
        assert_eq!(parsed.address, None);
        assert_eq!(parsed.api_key, None);
        assert_eq!(parsed.timeout_ms, None);
        assert_eq!(parsed.config_dir, None);
    }

    #[test]
    fn parses_every_documented_key() {
        let parsed = SyncthingConfig::from_config(&config(&[
            ("address", "http://127.0.0.1:9000"),
            ("api-key", "secret"),
            ("require-daemon", "false"),
            ("assume-marker-only", "true"),
            ("receive-only-changes", "ahead"),
            ("timeout-ms", "2500"),
            ("config-dir", "/tmp/stconf"),
        ]))
        .unwrap();
        assert_eq!(parsed.address.as_deref(), Some("http://127.0.0.1:9000"));
        assert_eq!(parsed.api_key.as_deref(), Some("secret"));
        assert!(!parsed.require_daemon);
        assert!(parsed.assume_marker_only);
        assert_eq!(parsed.receive_only_changes, ReceiveOnlyChanges::Ahead);
        assert_eq!(parsed.timeout_ms, Some(2500));
        assert_eq!(parsed.config_dir, Some(PathBuf::from("/tmp/stconf")));
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let parsed = SyncthingConfig::from_config(&config(&[("mystery", "42")])).unwrap();
        assert_eq!(parsed, SyncthingConfig::default());
    }

    #[test]
    fn invalid_values_are_errors() {
        for (key, value) in [
            ("require-daemon", "maybe"),
            ("assume-marker-only", "yes"),
            ("receive-only-changes", "sometimes"),
            ("timeout-ms", "soon"),
        ] {
            let err = SyncthingConfig::from_config(&config(&[(key, value)])).unwrap_err();
            assert!(
                err.message().contains(key),
                "error for `{key}` should name the key: {err}"
            );
        }
    }

    #[test]
    fn receive_only_spellings_round_trip() {
        for value in ["dirty", "ahead", "clean"] {
            let parsed =
                SyncthingConfig::from_config(&config(&[("receive-only-changes", value)])).unwrap();
            assert_eq!(parsed.receive_only_changes.as_str(), value);
        }
    }
}
