//! `plugin.toml` manifest parsing and validation.
//!
//! The manifest schema is §5.3 of `docs/rust-rewrite-plan.md`: a `[plugin]`
//! table (name, version, api, description, license), a `[hooks]` table (which
//! filesystem objects the plugin wants to see plus a priority), a
//! `[capabilities]` table (`exec`, `fs-read`, `http`, `cache`) and an optional
//! `[config-schema]` table.
//!
//! # Unknown keys vs unknown capabilities
//!
//! The plan (§5.2, §12 M1.3) distinguishes unknown *manifest* keys from unknown
//! *capabilities*:
//!
//! * An unknown key in `[plugin]`, `[hooks]` or at the top level is almost
//!   always a typo (for example `files = true` instead of `file = true`). The
//!   field is collected verbatim and surfaced by [`Manifest::unknown_keys`] as
//!   a **warning**, so a mis-spelled hook is visible instead of silently
//!   disabling a hook.
//! * An unknown key inside `[capabilities]` is a **load error**: every entry in
//!   that table describes something the plugin *requires*. If the host does not
//!   know how to grant it, the plugin cannot be loaded safely. The known set is
//!   exactly `exec`, `fs-read`, `http` and `cache`.
//!
//! API compatibility is checked against the supported major: the host contract
//! is `backupcrawl:detector@1.0.0`, so `api = "1"` (or any `1.x`) is accepted
//! and anything else is a load error.
//!
//! The raw bytes are retained verbatim because the trust flow (M1.5) hashes
//! them; this module never hashes or trusts anything itself.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// Default `[hooks].priority` when the key is absent; lower is asked earlier.
pub const DEFAULT_PRIORITY: u32 = 100;

/// The manifest API major this host understands.
pub const SUPPORTED_API_MAJOR: u64 = 1;

/// The contract the supported API major corresponds to.
pub const SUPPORTED_CONTRACT: &str = "backupcrawl:detector@1.0.0";

/// Capability names the host knows about; anything else in `[capabilities]` is
/// a load error.
pub const KNOWN_CAPABILITIES: &[&str] = &["exec", "fs-read", "http", "cache"];

fn default_priority() -> u32 {
    DEFAULT_PRIORITY
}

/// A parsed and validated `plugin.toml`.
///
/// Construction goes through [`Manifest::parse`], which also validates API
/// compatibility and capabilities. Unknown non-capability keys are kept in the
/// `extra` maps of each table and reported by [`Manifest::unknown_keys`].
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    /// The mandatory `[plugin]` table.
    pub plugin: PluginMeta,

    /// The `[hooks]` table; defaults to no hooks and priority 100.
    #[serde(default)]
    pub hooks: Hooks,

    /// The `[capabilities]` table; defaults to no capabilities.
    #[serde(default)]
    pub capabilities: Capabilities,

    /// The optional `[config-schema]` table, kept as an opaque TOML table.
    #[serde(default, rename = "config-schema")]
    pub config_schema: Option<toml::Table>,

    /// Unknown top-level keys, captured instead of rejected.
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,

    /// The raw manifest bytes, kept for the M1.5 trust hash.
    #[serde(skip)]
    raw: Vec<u8>,
}

/// The `[plugin]` table.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginMeta {
    /// Detector name; unique across sources and used by `[plugins.git]` config.
    pub name: String,

    /// Detector version, informational.
    pub version: String,

    /// Manifest API version, for example `"1"`.
    pub api: String,

    /// Human-readable description.
    #[serde(default)]
    pub description: Option<String>,

    /// SPDX license identifier or similar.
    #[serde(default)]
    pub license: Option<String>,

    /// Unknown keys inside `[plugin]` (warnings, not errors).
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

/// The `[hooks]` table.
#[derive(Debug, Clone, Deserialize)]
pub struct Hooks {
    /// Whether the plugin wants `check_dir` calls.
    #[serde(default)]
    pub dir: bool,

    /// Whether the plugin wants `check_file` calls.
    #[serde(default)]
    pub file: bool,

    /// Whether the plugin wants `check_symlink` calls.
    #[serde(default)]
    pub symlink: bool,

    /// Ordering hint: lower is asked earlier.
    #[serde(default = "default_priority")]
    pub priority: u32,

    /// Unknown keys inside `[hooks]` (warnings, not errors).
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

impl Default for Hooks {
    fn default() -> Self {
        Self {
            dir: false,
            file: false,
            symlink: false,
            priority: DEFAULT_PRIORITY,
            extra: BTreeMap::new(),
        }
    }
}

/// The `[capabilities]` table.
///
/// Unknown keys here are a load error (see the module docs); they are captured
/// in [`Capabilities::extra`] so [`Manifest::validate`] can name them.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Capabilities {
    /// Allowlisted programs the plugin may `exec`.
    #[serde(default)]
    pub exec: Vec<String>,

    /// Read-only filesystem roots the plugin needs (`~` + shell globs; M2.0).
    ///
    /// Authoritative since M2.0: the host preopens the crawl root plus every
    /// resolved spec. An unresolved spec is a warning, not a load error.
    #[serde(default, rename = "fs-read")]
    pub fs_read: Vec<String>,

    /// Allowlisted HTTP origins.
    #[serde(default)]
    pub http: Vec<String>,

    /// Whether the plugin uses the persistent key/value cache.
    #[serde(default)]
    pub cache: bool,

    /// Unknown capability names (load errors, not warnings).
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

impl Manifest {
    /// Parses and validates manifest bytes.
    ///
    /// The bytes are kept verbatim (see [`Manifest::raw`]). Validation covers
    /// API compatibility and unknown capabilities; unknown non-capability keys
    /// are only warnings and are available via [`Manifest::unknown_keys`].
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(bytes).context("plugin.toml is not valid UTF-8")?;
        let mut manifest: Manifest =
            toml::from_str(text).context("invalid plugin.toml manifest")?;
        manifest.raw = bytes.to_vec();
        manifest.validate()?;
        Ok(manifest)
    }

    /// The raw manifest bytes exactly as read from disk.
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    /// The detector name from `[plugin].name`.
    pub fn name(&self) -> &str {
        &self.plugin.name
    }

    /// The detector version from `[plugin].version`.
    pub fn version(&self) -> &str {
        &self.plugin.version
    }

    /// Validates API compatibility and that every declared capability is known.
    pub fn validate(&self) -> Result<()> {
        let api = self.plugin.api.trim();
        match api_major(api) {
            Some(major) if major == SUPPORTED_API_MAJOR => {}
            Some(major) => bail!(
                "plugin `{}` requires API {major} (`{api}`), but this host only \
                 supports major {SUPPORTED_API_MAJOR} ({SUPPORTED_CONTRACT})",
                self.plugin.name
            ),
            None => bail!(
                "plugin `{}` has an invalid api value `{api}`; expected a version \
                 like \"{SUPPORTED_API_MAJOR}\"",
                self.plugin.name
            ),
        }

        if !self.capabilities.extra.is_empty() {
            let unknown: Vec<&str> = self.capabilities.extra.keys().map(String::as_str).collect();
            bail!(
                "unknown required capability for plugin `{}`: {} (known: {})",
                self.plugin.name,
                unknown.join(", "),
                KNOWN_CAPABILITIES.join(", ")
            );
        }

        Ok(())
    }

    /// Unknown keys outside `[capabilities]`, as human-readable warnings.
    ///
    /// Ordering is stable: `[plugin]`, then `[hooks]`, then top-level keys, each
    /// in lexicographic order (the maps are [`BTreeMap`]s).
    pub fn unknown_keys(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        for key in self.plugin.extra.keys() {
            warnings.push(format!("unknown manifest key `plugin.{key}`"));
        }
        for key in self.hooks.extra.keys() {
            warnings.push(format!("unknown manifest key `hooks.{key}`"));
        }
        for key in self.extra.keys() {
            warnings.push(format!("unknown top-level manifest key `{key}`"));
        }
        warnings
    }
}

/// Parses the leading major component of an `api` string (`"1"` / `"1.0.0"`).
fn api_major(api: &str) -> Option<u64> {
    api.split('.').next()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal(api: &str) -> String {
        format!(
            r#"
            [plugin]
            name = "git"
            version = "0.1.0"
            api = "{api}"
            "#
        )
    }

    #[test]
    fn parses_full_manifest() {
        let manifest = Manifest::parse(
            br#"
            [plugin]
            name = "git"
            version = "0.1.0"
            api = "1"
            description = "Git repositories and their sync state"
            license = "MIT"

            [hooks]
            dir = true
            file = true
            symlink = true
            priority = 10

            [capabilities]
            exec = []
            fs-read = ["~/.config/git"]
            http = ["http://127.0.0.1:8384"]
            cache = true

            [config-schema]
            type = "object"
            "#,
        )
        .expect("valid manifest");

        assert_eq!(manifest.name(), "git");
        assert_eq!(manifest.version(), "0.1.0");
        assert_eq!(
            manifest.plugin.description.as_deref(),
            Some("Git repositories and their sync state")
        );
        assert_eq!(manifest.plugin.license.as_deref(), Some("MIT"));
        assert!(manifest.hooks.dir && manifest.hooks.file && manifest.hooks.symlink);
        assert_eq!(manifest.hooks.priority, 10);
        assert_eq!(manifest.capabilities.fs_read, ["~/.config/git"]);
        assert_eq!(manifest.capabilities.http, ["http://127.0.0.1:8384"]);
        assert!(manifest.capabilities.cache);
        assert!(manifest.config_schema.is_some());
        assert!(manifest.unknown_keys().is_empty());
        assert!(!manifest.raw().is_empty());
    }

    #[test]
    fn hooks_default_to_priority_100_and_no_hooks() {
        let manifest = Manifest::parse(minimal("1").as_bytes()).unwrap();

        assert!(!manifest.hooks.dir);
        assert!(!manifest.hooks.file);
        assert!(!manifest.hooks.symlink);
        assert_eq!(manifest.hooks.priority, DEFAULT_PRIORITY);
    }

    #[test]
    fn hooks_table_without_priority_keeps_default() {
        let mut toml = minimal("1");
        toml.push_str("[hooks]\nfile = true\n");
        let manifest = Manifest::parse(toml.as_bytes()).unwrap();

        assert!(manifest.hooks.file);
        assert_eq!(manifest.hooks.priority, DEFAULT_PRIORITY);
    }

    #[test]
    fn accepts_api_1_minor_versions() {
        for api in ["1", "1.0", "1.0.0"] {
            let manifest = Manifest::parse(minimal(api).as_bytes())
                .unwrap_or_else(|err| panic!("api {api} must be accepted: {err:#}"));
            assert_eq!(manifest.plugin.api, api);
        }
    }

    #[test]
    fn rejects_api_mismatch() {
        let err = Manifest::parse(minimal("2").as_bytes()).expect_err("api 2 must fail");

        let message = format!("{err:#}");
        assert!(message.contains("requires API 2"), "got: {message}");
        assert!(message.contains("supports major 1"), "got: {message}");
    }

    #[test]
    fn rejects_invalid_api_value() {
        let err = Manifest::parse(minimal("latest").as_bytes()).expect_err("non-numeric api");

        assert!(
            format!("{err:#}").contains("invalid api value"),
            "got: {err:#}"
        );
    }

    #[test]
    fn collects_unknown_keys_as_warnings() {
        let manifest = Manifest::parse(
            br#"
            notes = "top-level typo"

            [plugin]
            name = "git"
            version = "0.1.0"
            api = "1"
            homepage = "https://example.com"

            [hooks]
            files = true
            "#,
        )
        .expect("unknown keys are warnings, not errors");

        let warnings = manifest.unknown_keys();
        assert!(warnings.contains(&"unknown manifest key `plugin.homepage`".to_string()));
        assert!(warnings.contains(&"unknown manifest key `hooks.files`".to_string()));
        assert!(warnings.contains(&"unknown top-level manifest key `notes`".to_string()));
    }

    #[test]
    fn unknown_capability_is_a_load_error() {
        let mut toml = minimal("1");
        toml.push_str("[capabilities]\nsudo = true\n");
        let err = Manifest::parse(toml.as_bytes()).expect_err("unknown capability must fail");

        let message = format!("{err:#}");
        assert!(
            message.contains("unknown required capability for plugin `git`: sudo"),
            "got: {message}"
        );
    }

    #[test]
    fn reject_flags_misspelled_known_capability() {
        // `exec` misspelled as `execs` must not silently grant nothing.
        let mut toml = minimal("1");
        toml.push_str("[capabilities]\nexecs = [\"git\"]\n");
        let err = Manifest::parse(toml.as_bytes()).expect_err("misspelled capability must fail");

        assert!(
            format!("{err:#}").contains("unknown required capability for plugin `git`: execs"),
            "got: {err:#}"
        );
    }
}
