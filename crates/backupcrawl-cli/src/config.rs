//! Config file loading.
//!
//! Config lives at `~/.config/backupcrawl/config.toml` (XDG aware). Only the
//! `[ignore]` table exists so far; unknown keys are rejected so typos do not
//! get silently ignored.

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
}

/// `[ignore]` table.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IgnoreConfig {
    /// Glob patterns excluded from the crawl, matched against full paths.
    #[serde(default)]
    pub paths: Vec<String>,
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
