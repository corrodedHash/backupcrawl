//! Source/destination directory resolution for the chezmoi detector.
//!
//! Chezmoi answers the managed/current questions relative to two roots: the
//! **source** (the dotfiles repository, `~/.local/share/chezmoi` by default)
//! and the **destination** (`$HOME`). `init` resolves them in this order
//! (§7.4):
//!
//! 1. the plugin config `source-dir` / `dest-dir`,
//! 2. `~/.config/chezmoi/chezmoi.toml` (`sourceDir` / `destDir`),
//! 3. the defaults above.
//!
//! The config file is parsed directly (the `toml` crate) instead of asking
//! `chezmoi dump-config`. Values support a leading `~`, and relative values
//! resolve against the home directory. Template expressions in the config file
//! are not evaluated; a value that is not a plain string is ignored.
//!
//! # Finding `$HOME` inside the sandbox
//!
//! WASI starts with an empty environment, so `$HOME` is usually unavailable to
//! the guest. The host does grant the manifest's `fs-read = ["~"]` root, but it
//! does not tell the plugin where that is. `detect_home` therefore tries, in
//! order: the process environment (native tests and a host that provides it),
//! the standard per-user locations (`/home/<user>`, `/Users/<user>`, `/root`),
//! the crawl root when it is a home directory, and finally a crawl root that
//! contains a chezmoi config/source. A non-standard home with a crawl root
//! elsewhere needs `source-dir`/`dest-dir` (or the config file location) set
//! explicitly; this is documented as a v1 limitation.

use std::path::{Component, Path, PathBuf};

use backupcrawl_plugin_sdk::DetectorError;
use serde::Deserialize;

use crate::config::ChezmoiConfig;

/// Chezmoi's per-user config file, relative to `$HOME`.
pub const CONFIG_RELATIVE: &str = ".config/chezmoi/chezmoi.toml";

/// Default source state root, relative to `$HOME`.
pub const DEFAULT_SOURCE_RELATIVE: &str = ".local/share/chezmoi";

/// The resolved roots used by the detector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    /// Source state (dotfiles repository).
    pub source: PathBuf,
    /// Destination root (`$HOME` by default).
    pub dest: PathBuf,
    /// Home directory, when it could be determined. Passed to `chezmoi` as
    /// `HOME` so the child finds its config just like the parent process would.
    pub home: Option<PathBuf>,
}

/// Resolves the roots, detecting `$HOME` from the crawl root and user.
pub fn resolve(config: &ChezmoiConfig, root: &Path, user: &str) -> Result<Dirs, DetectorError> {
    let home = detect_home(root, user);
    resolve_with_home(config, home.as_deref())
}

/// Resolves the roots against an explicit home directory (test seam).
pub(crate) fn resolve_with_home(
    config: &ChezmoiConfig,
    home: Option<&Path>,
) -> Result<Dirs, DetectorError> {
    // Only read the config file when it can still matter.
    let file = if config.source_dir.is_none() || config.dest_dir.is_none() {
        home.and_then(read_config_file)
    } else {
        None
    };

    let source = choose(
        config.source_dir.clone(),
        file.as_ref().and_then(|file| file.source_dir.clone()),
        DEFAULT_SOURCE_RELATIVE,
        home,
        "source-dir",
    )?;
    let dest = choose(
        config.dest_dir.clone(),
        file.as_ref().and_then(|file| file.dest_dir.clone()),
        ".",
        home,
        "dest-dir",
    )?;

    Ok(Dirs {
        source,
        dest,
        home: home.map(Path::to_path_buf),
    })
}

/// The raw `chezmoi.toml` keys the detector understands.
#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    #[serde(rename = "sourceDir")]
    source_dir: Option<String>,
    #[serde(rename = "destDir")]
    dest_dir: Option<String>,
}

fn read_config_file(home: &Path) -> Option<FileConfig> {
    let text = std::fs::read_to_string(home.join(CONFIG_RELATIVE)).ok()?;
    // A template config (`chezmoi.toml.tmpl`) or a malformed file falls back to
    // the defaults instead of failing the scan.
    toml::from_str(&text).ok()
}

fn choose(
    explicit: Option<PathBuf>,
    from_file: Option<String>,
    default_relative: &str,
    home: Option<&Path>,
    key: &str,
) -> Result<PathBuf, DetectorError> {
    let value = explicit
        .or_else(|| from_file.map(PathBuf::from))
        .or_else(|| home.map(|home| home.join(default_relative)));
    let value = value.ok_or_else(|| {
        DetectorError::new(format!(
            "chezmoi: cannot determine the {key} directory because $HOME is unavailable; \
             set [plugins.chezmoi] source-dir and dest-dir explicitly"
        ))
    })?;
    absolutize(value, home).ok_or_else(|| {
        DetectorError::new(format!(
            "chezmoi: {key} path is relative and no home directory is known; \
             use an absolute path"
        ))
    })
}

/// Expands a leading `~` and makes relative paths absolute against `home`.
fn absolutize(path: PathBuf, home: Option<&Path>) -> Option<PathBuf> {
    let path = expand_tilde(path, home);
    if path.is_absolute() {
        Some(normalize(path))
    } else {
        home.map(|home| normalize(home.join(path)))
    }
}

fn expand_tilde(path: PathBuf, home: Option<&Path>) -> PathBuf {
    let mut components = path.components();
    let Some(Component::Normal(first)) = components.next() else {
        return path;
    };
    let first = first.to_string_lossy();
    if first != "~" {
        return path;
    }
    let Some(home) = home else {
        return path;
    };
    let rest: PathBuf = components.collect();
    if rest.as_os_str().is_empty() {
        home.to_path_buf()
    } else {
        home.join(rest)
    }
}

/// Lexically normalizes `.` and `..` without touching the filesystem.
fn normalize(path: PathBuf) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(component.as_os_str());
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

/// Best-effort `$HOME` detection (see the module docs).
pub(crate) fn detect_home(root: &Path, user: &str) -> Option<PathBuf> {
    if let Some(value) = std::env::var_os("HOME") {
        let home = PathBuf::from(value);
        if home.is_absolute() {
            return Some(home);
        }
    }

    if !user.is_empty() {
        for candidate in [
            PathBuf::from(format!("/home/{user}")),
            PathBuf::from(format!("/Users/{user}")),
        ] {
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
        if user == "root" && Path::new("/root").is_dir() {
            return Some(PathBuf::from("/root"));
        }

        // The crawl root itself is the home directory.
        let parent = root.parent();
        if parent == Some(Path::new("/home"))
            || parent == Some(Path::new("/Users"))
            || root == Path::new("/root")
        {
            return Some(root.to_path_buf());
        }
    }

    // A non-standard home: when crawling `$HOME` directly, the crawl root
    // contains the chezmoi config or source state.
    if root.is_dir()
        && (root.join(CONFIG_RELATIVE).is_file() || root.join(DEFAULT_SOURCE_RELATIVE).is_dir())
    {
        return Some(root.to_path_buf());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(source: Option<&str>, dest: Option<&str>) -> ChezmoiConfig {
        ChezmoiConfig {
            source_dir: source.map(PathBuf::from),
            dest_dir: dest.map(PathBuf::from),
            ..ChezmoiConfig::default()
        }
    }

    #[test]
    fn defaults_are_under_home() {
        let home = tempfile::tempdir().unwrap();
        let dirs = resolve_with_home(&ChezmoiConfig::default(), Some(home.path())).unwrap();

        assert_eq!(dirs.source, home.path().join(".local/share/chezmoi"));
        assert_eq!(dirs.dest, home.path());
        assert_eq!(dirs.home.as_deref(), Some(home.path()));
    }

    #[test]
    fn config_file_source_and_dest_are_used() {
        let home = tempfile::tempdir().unwrap();
        let config_dir = home.path().join(".config/chezmoi");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("chezmoi.toml"),
            "sourceDir = \"/srv/dotfiles\"\ndestDir = \"/home/someone\"\n",
        )
        .unwrap();

        let dirs = resolve_with_home(&ChezmoiConfig::default(), Some(home.path())).unwrap();
        assert_eq!(dirs.source, PathBuf::from("/srv/dotfiles"));
        assert_eq!(dirs.dest, PathBuf::from("/home/someone"));
    }

    #[test]
    fn config_file_tilde_values_expand_against_home() {
        let home = tempfile::tempdir().unwrap();
        let config_dir = home.path().join(".config/chezmoi");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("chezmoi.toml"),
            "sourceDir = \"~/dotfiles\"\ndestDir = \"~/target\"\n",
        )
        .unwrap();

        let dirs = resolve_with_home(&ChezmoiConfig::default(), Some(home.path())).unwrap();
        assert_eq!(dirs.source, home.path().join("dotfiles"));
        assert_eq!(dirs.dest, home.path().join("target"));
    }

    #[test]
    fn explicit_config_overrides_the_file() {
        let home = tempfile::tempdir().unwrap();
        let config_dir = home.path().join(".config/chezmoi");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("chezmoi.toml"),
            "sourceDir = \"/from-file\"\ndestDir = \"/from-file\"\n",
        )
        .unwrap();

        let dirs = resolve_with_home(
            &config_with(Some("/explicit/src"), Some("/explicit/dest")),
            Some(home.path()),
        )
        .unwrap();
        assert_eq!(dirs.source, PathBuf::from("/explicit/src"));
        assert_eq!(dirs.dest, PathBuf::from("/explicit/dest"));
    }

    #[test]
    fn relative_paths_resolve_against_home() {
        let home = tempfile::tempdir().unwrap();
        let dirs = resolve_with_home(&config_with(Some("dotfiles"), Some(".")), Some(home.path()))
            .unwrap();
        assert_eq!(dirs.source, home.path().join("dotfiles"));
        assert_eq!(dirs.dest, home.path());
    }

    #[test]
    fn missing_home_and_no_overrides_is_an_error() {
        let err = resolve_with_home(&ChezmoiConfig::default(), None).unwrap_err();
        assert!(err.message().contains("$HOME"), "{err}");
        assert!(err.message().contains("source-dir"), "{err}");
    }

    #[test]
    fn missing_config_file_uses_defaults() {
        let home = tempfile::tempdir().unwrap();
        let dirs = resolve_with_home(&ChezmoiConfig::default(), Some(home.path())).unwrap();
        assert_eq!(dirs.source, home.path().join(".local/share/chezmoi"));
        assert_eq!(dirs.dest, home.path());
    }

    #[test]
    fn malformed_config_file_uses_defaults() {
        let home = tempfile::tempdir().unwrap();
        let config_dir = home.path().join(".config/chezmoi");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join("chezmoi.toml"), "not = [ toml").unwrap();

        let dirs = resolve_with_home(&ChezmoiConfig::default(), Some(home.path())).unwrap();
        assert_eq!(dirs.source, home.path().join(".local/share/chezmoi"));
    }
}
