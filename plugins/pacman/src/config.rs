//! Pacman detector configuration (§7.2 of `docs/rust-rewrite-plan.md`).
//!
//! The `[plugins.pacman]` table reaches the plugin as a flat list of string
//! attributes ([`backupcrawl_plugin_sdk::Config`]); this module validates it.
//! Unknown keys are ignored (the optional `[config-schema]` is not enforced in
//! v1), but invalid values for known keys are a hard `init` error so a typo
//! never silently changes behavior.

use std::path::PathBuf;

use backupcrawl_plugin_sdk::{Config, DetectorError};

/// Default pacman database root.
pub const DEFAULT_DB_PATH: &str = "/var/lib/pacman";

/// Which implementation answers the integrity question.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Parse the on-disk database in-process (default).
    Native,
    /// Run `pacman -Qkk` through the host `exec` import (deferred).
    Pacman,
}

impl Backend {
    /// The config spelling, used in diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Pacman => "pacman",
        }
    }
}

/// How much of each owned path the detector verifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckLevel {
    /// Compare type, size, mtime, mode, uid, gid and symlink target.
    Metadata,
    /// Also hash file contents against the mtree md5/sha256 digests.
    Checksum,
}

impl CheckLevel {
    /// The config spelling, used in diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Checksum => "checksum",
        }
    }
}

/// Validated pacman detector configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacmanConfig {
    /// Database root containing `local/`.
    pub db_path: PathBuf,
    /// Verification depth.
    pub check_level: CheckLevel,
    /// Integrity backend.
    pub backend: Backend,
    /// Reserved per-operation budget in milliseconds (the native backend reads
    /// the database synchronously; the host's fuel/epoch limits are the real
    /// protection until the `pacman` backend lands).
    pub timeout_ms: Option<u64>,
}

impl Default for PacmanConfig {
    fn default() -> Self {
        Self {
            db_path: PathBuf::from(DEFAULT_DB_PATH),
            check_level: CheckLevel::Metadata,
            backend: Backend::Native,
            timeout_ms: None,
        }
    }
}

impl PacmanConfig {
    /// Parses and validates the `[plugins.pacman]` table.
    pub fn from_config(config: &Config) -> Result<Self, DetectorError> {
        let mut out = Self::default();
        for attr in config.iter() {
            match attr.key.as_str() {
                "db-path" => out.db_path = PathBuf::from(&attr.value),
                "check-level" => out.check_level = parse_check_level(&attr.value)?,
                "backend" => out.backend = parse_backend(&attr.value)?,
                "timeout-ms" => out.timeout_ms = Some(parse_u64("timeout-ms", &attr.value)?),
                // Unknown config keys are ignored; the manifest's optional
                // `[config-schema]` is not enforced in v1.
                _ => {}
            }
        }
        if out.backend == Backend::Pacman {
            return Err(DetectorError::new(
                "pacman config `backend = \"pacman\"` is not implemented yet; \
                 the `pacman -Qkk` fallback is deferred, use \"native\"",
            ));
        }
        Ok(out)
    }
}

fn parse_check_level(value: &str) -> Result<CheckLevel, DetectorError> {
    match value {
        "metadata" => Ok(CheckLevel::Metadata),
        "checksum" => Ok(CheckLevel::Checksum),
        other => Err(DetectorError::new(format!(
            "pacman config `check-level = \"{other}\"` is invalid; \
             expected \"metadata\" or \"checksum\""
        ))),
    }
}

fn parse_backend(value: &str) -> Result<Backend, DetectorError> {
    match value {
        "native" => Ok(Backend::Native),
        "pacman" => Ok(Backend::Pacman),
        other => Err(DetectorError::new(format!(
            "pacman config `backend = \"{other}\"` is invalid; \
             expected \"native\" or \"pacman\""
        ))),
    }
}

fn parse_u64(key: &str, value: &str) -> Result<u64, DetectorError> {
    value.parse().map_err(|_| {
        DetectorError::new(format!(
            "pacman config `{key} = \"{value}\"` is invalid; expected a non-negative integer"
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
        let parsed = PacmanConfig::from_config(&config(&[])).unwrap();
        assert_eq!(parsed, PacmanConfig::default());
        assert_eq!(parsed.db_path, PathBuf::from("/var/lib/pacman"));
        assert_eq!(parsed.check_level, CheckLevel::Metadata);
        assert_eq!(parsed.backend, Backend::Native);
        assert_eq!(parsed.timeout_ms, None);
    }

    #[test]
    fn parses_every_documented_key() {
        let parsed = PacmanConfig::from_config(&config(&[
            ("db-path", "/srv/pacman"),
            ("check-level", "checksum"),
            ("backend", "native"),
            ("timeout-ms", "2500"),
        ]))
        .unwrap();
        assert_eq!(parsed.db_path, PathBuf::from("/srv/pacman"));
        assert_eq!(parsed.check_level, CheckLevel::Checksum);
        assert_eq!(parsed.backend, Backend::Native);
        assert_eq!(parsed.timeout_ms, Some(2500));
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let parsed = PacmanConfig::from_config(&config(&[("mystery", "42")])).unwrap();
        assert_eq!(parsed, PacmanConfig::default());
    }

    #[test]
    fn invalid_values_are_errors() {
        for (key, value) in [
            ("check-level", "paranoid"),
            ("backend", "libalpm"),
            ("timeout-ms", "soon"),
        ] {
            let err = PacmanConfig::from_config(&config(&[(key, value)])).unwrap_err();
            assert!(
                err.message().contains(key),
                "error for `{key}` should name the key: {err}"
            );
        }
    }

    #[test]
    fn pacman_backend_is_a_clear_not_implemented_error() {
        let err = PacmanConfig::from_config(&config(&[("backend", "pacman")])).unwrap_err();
        assert!(err.message().contains("not implemented yet"), "{err}");
        assert!(err.message().contains("native"), "{err}");
    }
}
