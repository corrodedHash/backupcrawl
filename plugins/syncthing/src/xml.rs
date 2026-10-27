//! Syncthing `config.xml` discovery and parsing (§7.3).
//!
//! Newer syncthing releases keep their configuration under
//! `~/.local/state/syncthing/config.xml`; older ones use
//! `~/.config/syncthing/config.xml`, and `$STCONFDIR` overrides both. The file
//! is a plain XML document:
//!
//! ```xml
//! <configuration>
//!   <folder id="docs" path="/home/u/Documents" label="Documents" type="sendreceive"/>
//!   <gui><address>127.0.0.1:8384</address><apikey>...</apikey></gui>
//! </configuration>
//! ```
//!
//! Only `folder` attributes (`id`, `path`, `label`, `type`) and the GUI address
//! and API key are consumed. A missing file is not an error (the plugin simply
//! has no configured folders); malformed XML is an `init` error so the plugin
//! is disabled with a visible note instead of silently claiming nothing.

use std::env;
use std::path::{Component, Path, PathBuf};

use backupcrawl_plugin_sdk::DetectorError;

/// One `<folder>` entry from `config.xml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderEntry {
    /// Folder ID used by the REST API (`?folder=<id>`).
    pub id: String,
    /// Absolute folder root, with `~` expanded.
    pub path: PathBuf,
    /// Optional human-readable label.
    pub label: Option<String>,
    /// Folder type (`sendreceive`, `sendonly`, `receiveonly`, ...).
    pub folder_type: Option<String>,
}

/// The parts of `config.xml` the detector needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigFile {
    /// Configured folders in document order.
    pub folders: Vec<FolderEntry>,
    /// `<gui><address>` (bind address, usually `127.0.0.1:8384`).
    pub gui_address: Option<String>,
    /// `<gui><apikey>` used as `X-API-Key`.
    pub api_key: Option<String>,
}

/// The config directory used by this scan, plus a short source label for logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLocation {
    /// Directory containing `config.xml`.
    pub dir: PathBuf,
    /// `explicit`, `STCONFDIR`, `state` or `config`.
    pub source: &'static str,
}

/// Resolves the config directory in the documented order.
///
/// 1. the `config-dir` test hook (always wins),
/// 2. `$STCONFDIR`,
/// 3. `~/.local/state/syncthing` when it contains `config.xml`,
/// 4. `~/.config/syncthing` when it contains `config.xml`.
///
/// A directory from steps 1–2 is used even when `config.xml` is missing, so an
/// explicitly configured empty directory does not silently fall through to an
/// older config. Returns `None` when nothing is configured.
pub fn resolve_config_dir(
    explicit: Option<&Path>,
    stconfdir: Option<&Path>,
    home: Option<&Path>,
) -> Option<ConfigLocation> {
    if let Some(dir) = explicit {
        return Some(ConfigLocation {
            dir: expand_home(dir, home),
            source: "explicit",
        });
    }
    if let Some(dir) = stconfdir {
        return Some(ConfigLocation {
            dir: expand_home(dir, home),
            source: "STCONFDIR",
        });
    }
    let home = home?;
    let state = home.join(".local/state/syncthing");
    if state.join("config.xml").is_file() {
        return Some(ConfigLocation {
            dir: state,
            source: "state",
        });
    }
    let config = home.join(".config/syncthing");
    if config.join("config.xml").is_file() {
        return Some(ConfigLocation {
            dir: config,
            source: "config",
        });
    }
    None
}

/// Loads and parses `<dir>/config.xml`.
///
/// A missing file yields an empty [`ConfigFile`]; unreadable or malformed
/// content is an error.
pub fn load_config(dir: &Path, home: Option<&Path>) -> Result<ConfigFile, DetectorError> {
    let path = dir.join("config.xml");
    let xml = match std::fs::read_to_string(&path) {
        Ok(xml) => xml,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ConfigFile::default());
        }
        Err(err) => {
            return Err(DetectorError::new(format!(
                "syncthing: cannot read {}: {err}",
                path.display()
            )));
        }
    };
    parse_config(&xml, home, dir).map_err(|err| {
        DetectorError::new(format!("syncthing: cannot parse {}: {err}", path.display()))
    })
}

/// Parses a `config.xml` document.
///
/// Folder paths are made absolute (relative entries are resolved against
/// `home`, falling back to the config directory) and lexically normalized.
pub fn parse_config(
    xml: &str,
    home: Option<&Path>,
    config_dir: &Path,
) -> Result<ConfigFile, String> {
    let document = roxmltree::Document::parse(xml).map_err(|err| err.to_string())?;
    let mut out = ConfigFile::default();

    for node in document
        .descendants()
        .filter(|node| node.is_element() && node.has_tag_name("folder"))
    {
        let Some(id) = node.attribute("id").map(str::trim) else {
            continue;
        };
        let Some(raw_path) = node.attribute("path").map(str::trim) else {
            continue;
        };
        if id.is_empty() || raw_path.is_empty() {
            continue;
        }
        let expanded = expand_home(Path::new(raw_path), home);
        let path = if expanded.is_absolute() {
            expanded
        } else if raw_path.starts_with('~') {
            // A `~` path we cannot expand stays unresolved rather than turning
            // into `<config-dir>/~/...`.
            expanded
        } else {
            home.unwrap_or(config_dir).join(expanded)
        };
        out.folders.push(FolderEntry {
            id: id.to_owned(),
            path: normalize(&path),
            label: non_empty(node.attribute("label")),
            folder_type: non_empty(node.attribute("type")),
        });
    }

    if let Some(gui) = document
        .descendants()
        .find(|node| node.is_element() && node.has_tag_name("gui"))
    {
        out.gui_address = child_text(gui, "address");
        out.api_key = child_text(gui, "apikey");
    }

    Ok(out)
}

fn child_text(node: roxmltree::Node<'_, '_>, name: &str) -> Option<String> {
    node.children()
        .find(|child| child.is_element() && child.has_tag_name(name))
        .and_then(|child| child.text())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Expands a leading `~` (alone or before `/`) against `home`.
pub fn expand_home(path: &Path, home: Option<&Path>) -> PathBuf {
    let text = path.to_string_lossy();
    if text == "~" {
        return home
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.to_path_buf());
    }
    if let Some(rest) = text.strip_prefix("~/")
        && let Some(home) = home
    {
        return home.join(rest);
    }
    path.to_path_buf()
}

/// Lexically normalizes a path (`.` and `..` removed, no filesystem access).
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The invoking user's home directory.
///
/// `$HOME` is the normal source. The Windows-style absence of it (and the wasm
/// guest's empty environment when the host does not pass `HOME`) falls back to
/// the conventional `/home/<user>` or `/root` path from [`ScanInfo::user`].
///
/// [`ScanInfo::user`]: backupcrawl_plugin_sdk::ScanInfo::user
pub fn home_dir(user: &str) -> Option<PathBuf> {
    home_from(user, env::var_os("HOME"))
}

/// The pure core of [`home_dir`], testable without touching the process env.
pub fn home_from(user: &str, env_home: Option<std::ffi::OsString>) -> Option<PathBuf> {
    if let Some(home) = env_home.filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(home));
    }
    let user = user.trim();
    if user.is_empty() {
        return None;
    }
    if user == "root" {
        Some(PathBuf::from("/root"))
    } else {
        Some(PathBuf::from("/home").join(user))
    }
}

/// `$STCONFDIR`, when set and non-empty.
pub fn stconfdir() -> Option<PathBuf> {
    env::var_os("STCONFDIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
        <configuration version="37">
          <folder id="docs" path="/home/u/Documents" label="Documents" type="sendreceive">
            <device id="DEVICE"></device>
          </folder>
          <folder id="pics" path="/srv/pictures" type="receiveonly"></folder>
          <folder id="missing-path"></folder>
          <folder path="/no/id"></folder>
          <gui>
            <address>127.0.0.1:8384</address>
            <apikey>secret-key</apikey>
          </gui>
        </configuration>
    "#;

    fn home() -> PathBuf {
        PathBuf::from("/home/u")
    }

    #[test]
    fn parses_folders_gui_address_and_api_key() {
        let parsed = parse_config(CONFIG, Some(&home()), Path::new("/cfg")).unwrap();
        assert_eq!(parsed.folders.len(), 2);
        assert_eq!(parsed.folders[0].id, "docs");
        assert_eq!(parsed.folders[0].path, PathBuf::from("/home/u/Documents"));
        assert_eq!(parsed.folders[0].label.as_deref(), Some("Documents"));
        assert_eq!(
            parsed.folders[0].folder_type.as_deref(),
            Some("sendreceive")
        );
        assert_eq!(parsed.folders[1].id, "pics");
        assert_eq!(parsed.folders[1].path, PathBuf::from("/srv/pictures"));
        assert_eq!(parsed.folders[1].label, None);
        assert_eq!(
            parsed.folders[1].folder_type.as_deref(),
            Some("receiveonly")
        );
        assert_eq!(parsed.gui_address.as_deref(), Some("127.0.0.1:8384"));
        assert_eq!(parsed.api_key.as_deref(), Some("secret-key"));
    }

    #[test]
    fn expands_tilde_and_relative_folder_paths() {
        let xml = r#"
            <configuration>
              <folder id="a" path="~/Docs"/>
              <folder id="b" path="relative"/>
            </configuration>
        "#;
        let parsed = parse_config(xml, Some(&home()), Path::new("/cfg")).unwrap();
        assert_eq!(parsed.folders[0].path, PathBuf::from("/home/u/Docs"));
        assert_eq!(parsed.folders[1].path, PathBuf::from("/home/u/relative"));

        let parsed = parse_config(xml, None, Path::new("/cfg")).unwrap();
        assert_eq!(parsed.folders[0].path, PathBuf::from("~/Docs"));
        assert_eq!(parsed.folders[1].path, PathBuf::from("/cfg/relative"));
    }

    #[test]
    fn normalizes_dot_and_dotdot_components() {
        assert_eq!(
            normalize(Path::new("/home/u/./a/../Documents/")),
            PathBuf::from("/home/u/Documents")
        );
    }

    #[test]
    fn malformed_xml_is_an_error() {
        let err =
            parse_config("<configuration><folder", Some(&home()), Path::new("/cfg")).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn missing_config_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let parsed = load_config(dir.path(), Some(&home())).unwrap();
        assert_eq!(parsed, ConfigFile::default());
    }

    #[test]
    fn load_config_reads_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.xml"), CONFIG).unwrap();
        let parsed = load_config(dir.path(), Some(&home())).unwrap();
        assert_eq!(parsed.folders.len(), 2);
        assert_eq!(parsed.api_key.as_deref(), Some("secret-key"));
    }

    #[test]
    fn discovery_prefers_explicit_then_stconfdir_then_state_then_config() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let explicit = tmp.path().join("explicit");
        let stconfdir = tmp.path().join("stconfdir");
        let state = home.join(".local/state/syncthing");
        let config = home.join(".config/syncthing");
        for dir in [&explicit, &stconfdir, &state, &config] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("config.xml"), "<configuration/>").unwrap();
        }

        assert_eq!(
            resolve_config_dir(Some(&explicit), Some(&stconfdir), Some(&home)),
            Some(ConfigLocation {
                dir: explicit,
                source: "explicit"
            })
        );
        assert_eq!(
            resolve_config_dir(None, Some(&stconfdir), Some(&home)),
            Some(ConfigLocation {
                dir: stconfdir,
                source: "STCONFDIR"
            })
        );
        assert_eq!(
            resolve_config_dir(None, None, Some(&home)),
            Some(ConfigLocation {
                dir: state.clone(),
                source: "state"
            })
        );

        std::fs::remove_file(state.join("config.xml")).unwrap();
        assert_eq!(
            resolve_config_dir(None, None, Some(&home)),
            Some(ConfigLocation {
                dir: config,
                source: "config"
            })
        );

        std::fs::remove_file(home.join(".config/syncthing/config.xml")).unwrap();
        assert_eq!(resolve_config_dir(None, None, Some(&home)), None);
    }

    #[test]
    fn explicit_dir_wins_even_without_config_xml() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("empty");
        std::fs::create_dir_all(&dir).unwrap();
        let location = resolve_config_dir(Some(&dir), None, None).unwrap();
        assert_eq!(location.dir, dir);
        assert_eq!(location.source, "explicit");
    }

    #[test]
    fn home_falls_back_to_the_user_name_when_home_is_unset() {
        assert_eq!(home_from("root", None), Some(PathBuf::from("/root")));
        assert_eq!(home_from("alice", None), Some(PathBuf::from("/home/alice")));
        assert_eq!(
            home_from("alice", Some("/srv/alice".into())),
            Some(PathBuf::from("/srv/alice"))
        );
        assert_eq!(home_from("", None), None);
    }
}
