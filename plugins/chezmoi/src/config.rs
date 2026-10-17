//! Chezmoi detector configuration (§7.4 of `docs/rust-rewrite-plan.md`).
//!
//! The `[plugins.chezmoi]` table reaches the plugin as a flat list of string
//! attributes ([`backupcrawl_plugin_sdk::Config`]); this module validates it.
//! Unknown keys are ignored (the optional `[config-schema]` is not enforced in
//! v1), but invalid values for known keys are a hard `init` error so a typo
//! never silently changes behavior.

use std::path::PathBuf;

use backupcrawl_plugin_sdk::{Config, DetectorError};

/// Default program looked up on the host `PATH`.
pub const DEFAULT_PROGRAM: &str = "chezmoi";

/// Which managed entry types are reported.
///
/// The default is all three. A restricted set is passed to `chezmoi managed`
/// as `--include=...`, so the tool itself filters (and a script that ignores
/// the flag is responsible for its own output, exactly like chezmoi).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncludeTypes {
    /// Regular files.
    pub files: bool,
    /// Directories.
    pub dirs: bool,
    /// Symbolic links.
    pub symlinks: bool,
}

impl Default for IncludeTypes {
    fn default() -> Self {
        Self {
            files: true,
            dirs: true,
            symlinks: true,
        }
    }
}

impl IncludeTypes {
    /// Parses the `include-types` config value.
    ///
    /// Accepts a comma-separated list of `files`, `dirs`, `symlinks` (and
    /// `all`); an empty value means all. Unknown types are an error instead of
    /// silently reporting less than requested.
    pub fn parse(value: &str) -> Result<Self, DetectorError> {
        let trimmed = value.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("all") {
            return Ok(Self::default());
        }
        let mut out = Self {
            files: false,
            dirs: false,
            symlinks: false,
        };
        for item in trimmed.split(',') {
            match item.trim() {
                "files" => out.files = true,
                "dirs" => out.dirs = true,
                "symlinks" => out.symlinks = true,
                other => {
                    return Err(DetectorError::new(format!(
                        "chezmoi config `include-types` has unknown type `{other}`; \
                         expected files, dirs, symlinks or all"
                    )));
                }
            }
        }
        if !out.files && !out.dirs && !out.symlinks {
            return Err(DetectorError::new(
                "chezmoi config `include-types` must name at least one type",
            ));
        }
        Ok(out)
    }

    /// Whether every type is included (the default; no `--include` flag).
    pub fn is_all(self) -> bool {
        self.files && self.dirs && self.symlinks
    }

    /// The canonical `--include=...` value, or `None` for all types.
    pub fn chezmoi_include(self) -> Option<String> {
        if self.is_all() {
            return None;
        }
        let mut types = Vec::new();
        if self.files {
            types.push("files");
        }
        if self.dirs {
            types.push("dirs");
        }
        if self.symlinks {
            types.push("symlinks");
        }
        Some(types.join(","))
    }

    /// A stable spelling for the cache key.
    pub fn cache_key(self) -> String {
        self.chezmoi_include().unwrap_or_else(|| "all".to_owned())
    }
}

/// Validated chezmoi detector configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChezmoiConfig {
    /// Overrides the config file / default source root.
    pub source_dir: Option<PathBuf>,
    /// Overrides the config file / default destination root.
    pub dest_dir: Option<PathBuf>,
    /// Managed entry types to consider.
    pub include_types: IncludeTypes,
    /// Program to execute (`chezmoi-path`, a test hook).
    pub program: String,
    /// Per-`exec` budget in milliseconds.
    pub timeout_ms: Option<u32>,
}

impl Default for ChezmoiConfig {
    fn default() -> Self {
        Self {
            source_dir: None,
            dest_dir: None,
            include_types: IncludeTypes::default(),
            program: DEFAULT_PROGRAM.to_owned(),
            timeout_ms: None,
        }
    }
}

impl ChezmoiConfig {
    /// Parses and validates the `[plugins.chezmoi]` table.
    pub fn from_config(config: &Config) -> Result<Self, DetectorError> {
        let mut out = Self::default();
        for attr in config.iter() {
            match attr.key.as_str() {
                "source-dir" => out.source_dir = Some(PathBuf::from(&attr.value)),
                "dest-dir" => out.dest_dir = Some(PathBuf::from(&attr.value)),
                "include-types" => out.include_types = IncludeTypes::parse(&attr.value)?,
                "chezmoi-path" => {
                    if attr.value.is_empty() {
                        return Err(DetectorError::new(
                            "chezmoi config `chezmoi-path` must not be empty",
                        ));
                    }
                    out.program = attr.value.clone();
                }
                "timeout-ms" => out.timeout_ms = Some(parse_timeout(&attr.value)?),
                // Unknown config keys are ignored; the manifest's optional
                // `[config-schema]` is not enforced in v1.
                _ => {}
            }
        }
        Ok(out)
    }
}

fn parse_timeout(value: &str) -> Result<u32, DetectorError> {
    value.parse().map_err(|_| {
        DetectorError::new(format!(
            "chezmoi config `timeout-ms = \"{value}\"` is invalid; \
             expected a non-negative integer"
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
        let parsed = ChezmoiConfig::from_config(&config(&[])).unwrap();
        assert_eq!(parsed, ChezmoiConfig::default());
        assert_eq!(parsed.program, "chezmoi");
        assert_eq!(parsed.source_dir, None);
        assert_eq!(parsed.dest_dir, None);
        assert_eq!(parsed.timeout_ms, None);
        assert!(parsed.include_types.is_all());
    }

    #[test]
    fn parses_every_documented_key() {
        let parsed = ChezmoiConfig::from_config(&config(&[
            ("source-dir", "/srv/dotfiles"),
            ("dest-dir", "/home/tester"),
            ("include-types", "files,symlinks"),
            ("chezmoi-path", "/opt/chezmoi"),
            ("timeout-ms", "2500"),
        ]))
        .unwrap();
        assert_eq!(parsed.source_dir, Some(PathBuf::from("/srv/dotfiles")));
        assert_eq!(parsed.dest_dir, Some(PathBuf::from("/home/tester")));
        assert_eq!(
            parsed.include_types,
            IncludeTypes {
                files: true,
                dirs: false,
                symlinks: true
            }
        );
        assert_eq!(parsed.program, "/opt/chezmoi");
        assert_eq!(parsed.timeout_ms, Some(2500));
    }

    #[test]
    fn include_types_accepts_all_and_empty() {
        assert_eq!(IncludeTypes::parse("all").unwrap(), IncludeTypes::default());
        assert_eq!(IncludeTypes::parse("").unwrap(), IncludeTypes::default());
        assert_eq!(
            IncludeTypes::parse(" dirs , files ")
                .unwrap()
                .chezmoi_include(),
            Some("files,dirs".to_owned())
        );
    }

    #[test]
    fn include_types_rejects_unknown_types() {
        let err = ChezmoiConfig::from_config(&config(&[("include-types", "scripts")])).unwrap_err();
        assert!(err.message().contains("scripts"), "{err}");
        assert!(err.message().contains("include-types"), "{err}");
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let parsed = ChezmoiConfig::from_config(&config(&[("mystery", "42")])).unwrap();
        assert_eq!(parsed, ChezmoiConfig::default());
    }

    #[test]
    fn invalid_values_are_errors() {
        for (key, value) in [("timeout-ms", "soon"), ("chezmoi-path", "")] {
            let err = ChezmoiConfig::from_config(&config(&[(key, value)])).unwrap_err();
            assert!(
                err.message().contains(key),
                "error for `{key}` should name the key: {err}"
            );
        }
    }
}
