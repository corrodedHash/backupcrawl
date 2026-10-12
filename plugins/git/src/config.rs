//! Git detector configuration (§7.1 of `docs/rust-rewrite-plan.md`).
//!
//! The `[plugins.git]` table reaches the plugin as a flat list of string
//! attributes ([`backupcrawl_plugin_sdk::Config`]); this module turns it into a
//! validated [`GitConfig`]. Unknown keys are ignored (the optional
//! `[config-schema]` is not enforced in v1), but invalid values for known keys
//! are a hard `init` error so a typo never silently changes behavior.

use backupcrawl_plugin_sdk::{Config, DetectorError};

/// Which implementation computes repository status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Pure Rust, in-process (no `exec` capability).
    Gix,
    /// `git status --porcelain=v2` via the host `exec` import (M1.9).
    Exec,
}

/// What to report for a repository whose HEAD has no upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoUpstream {
    /// Report `unknown` (the default; fixes the Python false positive).
    Unknown,
    /// Report `ahead` (the old, wrong-ish behavior).
    Ahead,
    /// Report `clean`.
    Clean,
}

/// How much of `.gitignore`d tree to surface when `report-ignored` is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgnoredMode {
    /// Only entries that match an ignore pattern (`build/`, `a.log`).
    Matching,
    /// Also files inside ignored directories.
    All,
}

/// Validated git detector configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitConfig {
    /// Status backend.
    pub backend: Backend,
    /// Verdict for a branch without an upstream.
    pub no_upstream: NoUpstream,
    /// Whether to collect and report ignored files.
    pub report_ignored: bool,
    /// Granularity of ignored reporting.
    pub ignored_mode: IgnoredMode,
    /// Whether to discover and claim the repository enclosing the crawl root.
    pub ancestor_repos: bool,
    /// Whether submodule worktrees are claimed on their own.
    pub recurse_submodules: bool,
    /// Parsed per-repository timeout budget (reserved; see module docs).
    pub timeout_ms: Option<u64>,
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            backend: Backend::Gix,
            no_upstream: NoUpstream::Unknown,
            report_ignored: false,
            ignored_mode: IgnoredMode::Matching,
            ancestor_repos: true,
            recurse_submodules: true,
            timeout_ms: None,
        }
    }
}

impl GitConfig {
    /// Parses and validates the `[plugins.git]` table.
    pub fn from_config(config: &Config) -> Result<Self, DetectorError> {
        let mut out = Self::default();
        for attr in config.iter() {
            match attr.key.as_str() {
                "backend" => out.backend = parse_backend(&attr.value)?,
                "no-upstream" => out.no_upstream = parse_no_upstream(&attr.value)?,
                "report-ignored" => out.report_ignored = parse_bool("report-ignored", &attr.value)?,
                "ignored-mode" => out.ignored_mode = parse_ignored_mode(&attr.value)?,
                "ancestor-repos" => out.ancestor_repos = parse_bool("ancestor-repos", &attr.value)?,
                "recurse-submodules" => {
                    out.recurse_submodules = parse_bool("recurse-submodules", &attr.value)?
                }
                "timeout-ms" => out.timeout_ms = Some(parse_u64("timeout-ms", &attr.value)?),
                // Unknown config keys are ignored; the manifest's optional
                // `[config-schema]` is not enforced in v1.
                _ => {}
            }
        }
        Ok(out)
    }
}

fn parse_backend(value: &str) -> Result<Backend, DetectorError> {
    match value {
        "gix" => Ok(Backend::Gix),
        "exec" => Ok(Backend::Exec),
        other => Err(DetectorError::new(format!(
            "git config `backend = \"{other}\"` is invalid; expected \"gix\" or \"exec\""
        ))),
    }
}

fn parse_no_upstream(value: &str) -> Result<NoUpstream, DetectorError> {
    match value {
        "unknown" => Ok(NoUpstream::Unknown),
        "ahead" => Ok(NoUpstream::Ahead),
        "clean" => Ok(NoUpstream::Clean),
        other => Err(DetectorError::new(format!(
            "git config `no-upstream = \"{other}\"` is invalid; \
             expected \"unknown\", \"ahead\" or \"clean\""
        ))),
    }
}

fn parse_ignored_mode(value: &str) -> Result<IgnoredMode, DetectorError> {
    match value {
        "matching" => Ok(IgnoredMode::Matching),
        "all" => Ok(IgnoredMode::All),
        other => Err(DetectorError::new(format!(
            "git config `ignored-mode = \"{other}\"` is invalid; \
             expected \"matching\" or \"all\""
        ))),
    }
}

fn parse_bool(key: &str, value: &str) -> Result<bool, DetectorError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(DetectorError::new(format!(
            "git config `{key} = \"{other}\"` is invalid; expected a boolean"
        ))),
    }
}

fn parse_u64(key: &str, value: &str) -> Result<u64, DetectorError> {
    value.parse().map_err(|_| {
        DetectorError::new(format!(
            "git config `{key} = \"{value}\"` is invalid; expected a non-negative integer"
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
        let parsed = GitConfig::from_config(&config(&[])).unwrap();
        assert_eq!(parsed, GitConfig::default());
        assert_eq!(parsed.backend, Backend::Gix);
        assert_eq!(parsed.no_upstream, NoUpstream::Unknown);
        assert!(!parsed.report_ignored);
        assert_eq!(parsed.ignored_mode, IgnoredMode::Matching);
        assert!(parsed.ancestor_repos);
        assert!(parsed.recurse_submodules);
        assert_eq!(parsed.timeout_ms, None);
    }

    #[test]
    fn parses_every_documented_key() {
        let parsed = GitConfig::from_config(&config(&[
            ("backend", "gix"),
            ("no-upstream", "clean"),
            ("report-ignored", "true"),
            ("ignored-mode", "all"),
            ("ancestor-repos", "false"),
            ("recurse-submodules", "false"),
            ("timeout-ms", "1500"),
        ]))
        .unwrap();

        assert_eq!(parsed.backend, Backend::Gix);
        assert_eq!(parsed.no_upstream, NoUpstream::Clean);
        assert!(parsed.report_ignored);
        assert_eq!(parsed.ignored_mode, IgnoredMode::All);
        assert!(!parsed.ancestor_repos);
        assert!(!parsed.recurse_submodules);
        assert_eq!(parsed.timeout_ms, Some(1500));
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let parsed = GitConfig::from_config(&config(&[("mystery", "42")])).unwrap();
        assert_eq!(parsed, GitConfig::default());
    }

    #[test]
    fn invalid_values_are_errors() {
        for (key, value) in [
            ("backend", "libgit2"),
            ("no-upstream", "maybe"),
            ("ignored-mode", "sometimes"),
            ("report-ignored", "yes"),
            ("timeout-ms", "soon"),
        ] {
            let err = GitConfig::from_config(&config(&[(key, value)])).unwrap_err();
            assert!(
                err.message().contains(key),
                "error for `{key}` should name the key: {err}"
            );
        }
    }
}
