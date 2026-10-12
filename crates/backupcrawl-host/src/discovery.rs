//! Plugin discovery.
//!
//! Detectors are discovered from four sources, scanned in this order
//! (§5.2 of `docs/rust-rewrite-plan.md`):
//!
//! 1. explicit `--plugin-dir` directories, in the order they were given;
//! 2. `$XDG_CONFIG_HOME/backupcrawl/plugins/` (the "user" dir);
//! 3. `/usr/lib/backupcrawl/plugins/` (the "system" dir);
//! 4. the bundled registry (see [`crate::bundled`]).
//!
//! The first source that provides a given plugin name wins, so the precedence
//! is explicit/user > system > bundled. Later same-name candidates are skipped
//! and reported as shadowed.
//!
//! A plugin is a directory containing a `plugin.toml` manifest plus its `.wasm`
//! sidecar. The sidecar is resolved as `<name>.wasm` first, then `plugin.wasm`,
//! then the single `*.wasm` file in the directory; anything else is an error.
//!
//! Discovery never aborts on a single bad plugin: unreadable directories become
//! warnings, and manifests that fail to parse/validate (including API mismatches
//! and unknown capabilities) or that lack a sidecar become per-plugin errors in
//! [`DiscoveryOutcome::errors`]. The caller decides whether that is fatal.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::bundled;
use crate::manifest::Manifest;

/// Manifest file name inside a plugin directory.
pub const MANIFEST_FILE: &str = "plugin.toml";

/// Conventional wasm sidecar name.
pub const WASM_FILE: &str = "plugin.wasm";

/// Default system-wide plugin directory.
pub const SYSTEM_PLUGIN_DIR: &str = "/usr/lib/backupcrawl/plugins";

/// The `$XDG_CONFIG_HOME/backupcrawl/plugins/` directory, if a config home is
/// known.
pub fn user_plugin_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("backupcrawl").join("plugins"))
}

/// Where discovery looks for plugins.
///
/// Tests usually construct this directly; [`SearchPaths::from_env`] fills in the
/// standard user/system/bundled locations.
#[derive(Debug, Clone, Default)]
pub struct SearchPaths {
    /// Explicit `--plugin-dir` directories, in precedence order.
    pub explicit: Vec<PathBuf>,
    /// User plugin directory (`$XDG_CONFIG_HOME/backupcrawl/plugins/`).
    pub user: Option<PathBuf>,
    /// System plugin directory (usually [`SYSTEM_PLUGIN_DIR`]).
    pub system: Option<PathBuf>,
    /// Whether the bundled registry is included.
    pub bundled: bool,
}

impl SearchPaths {
    /// Explicit dirs plus the standard user, system and bundled sources.
    pub fn from_env(explicit: Vec<PathBuf>) -> Self {
        Self {
            explicit,
            user: user_plugin_dir(),
            system: Some(PathBuf::from(SYSTEM_PLUGIN_DIR)),
            bundled: true,
        }
    }

    /// Only the given explicit directories; no user/system/bundled sources.
    pub fn explicit_only(explicit: Vec<PathBuf>) -> Self {
        Self {
            explicit,
            ..Self::default()
        }
    }
}

/// Which source a plugin came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginSource {
    /// A directory passed with `--plugin-dir`.
    Explicit,
    /// `$XDG_CONFIG_HOME/backupcrawl/plugins/`.
    User,
    /// `/usr/lib/backupcrawl/plugins/`.
    System,
    /// Embedded in the binary.
    Bundled,
}

impl PluginSource {
    /// Short, stable label used in output.
    pub fn label(self) -> &'static str {
        match self {
            PluginSource::Explicit => "explicit",
            PluginSource::User => "user",
            PluginSource::System => "system",
            PluginSource::Bundled => "bundled",
        }
    }
}

/// A plugin that parsed and validated successfully.
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    /// Resolved name (`manifest.plugin.name`).
    pub name: String,
    /// The parsed and validated manifest.
    pub manifest: Manifest,
    /// Path to `plugin.toml`, or `None` for bundled plugins.
    pub manifest_path: Option<PathBuf>,
    /// Path to the wasm sidecar, or `None` for bundled plugins.
    pub wasm_path: Option<PathBuf>,
    /// Raw wasm component bytes.
    pub wasm_bytes: Vec<u8>,
    /// Which source provided the winning entry.
    pub source: PluginSource,
    /// The plugin directory, or `None` for bundled plugins.
    pub source_dir: Option<PathBuf>,
    /// Non-fatal manifest warnings (unknown keys), stable order.
    pub warnings: Vec<String>,
}

impl DiscoveredPlugin {
    /// The raw manifest bytes (hashed by the trust flow in M1.5).
    pub fn raw_manifest(&self) -> &[u8] {
        self.manifest.raw()
    }

    /// The manifest API value, always compatible for discovered plugins.
    pub fn api(&self) -> &str {
        &self.manifest.plugin.api
    }

    /// The detector version from the manifest.
    pub fn version(&self) -> &str {
        self.manifest.version()
    }
}

/// A plugin that could not be loaded.
#[derive(Debug, Clone)]
pub struct DiscoveryError {
    /// Plugin directory, or `None` for a bundled registration problem.
    pub dir: Option<PathBuf>,
    /// Source the failing plugin came from.
    pub source: PluginSource,
    /// Human-readable reason.
    pub message: String,
}

impl DiscoveryError {
    /// Renders the error with its origin for diagnostics.
    pub fn display(&self) -> String {
        match &self.dir {
            Some(dir) => format!("{}: {}", dir.display(), self.message),
            None => format!("{}: {}", self.source.label(), self.message),
        }
    }
}

/// The result of a discovery pass.
#[derive(Debug, Default, Clone)]
pub struct DiscoveryOutcome {
    /// Plugins in precedence order (the winner first for each name).
    pub plugins: Vec<DiscoveredPlugin>,
    /// Plugins that failed to load, in scan order.
    pub errors: Vec<DiscoveryError>,
    /// Directory-level warnings (missing/unreadable directories, shadowing).
    pub warnings: Vec<String>,
}

impl DiscoveryOutcome {
    /// Looks up a discovered plugin by name.
    pub fn get(&self, name: &str) -> Option<&DiscoveredPlugin> {
        self.plugins.iter().find(|plugin| plugin.name == name)
    }

    /// Names of discovered plugins in precedence order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.plugins.iter().map(|plugin| plugin.name.as_str())
    }
}

/// Scans `search` for plugins, in precedence order.
pub fn discover(search: &SearchPaths) -> DiscoveryOutcome {
    let mut outcome = DiscoveryOutcome::default();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();

    for dir in &search.explicit {
        scan_dir(dir, PluginSource::Explicit, &mut outcome, &mut seen);
    }
    if let Some(dir) = &search.user {
        scan_dir(dir, PluginSource::User, &mut outcome, &mut seen);
    }
    if let Some(dir) = &search.system {
        scan_dir(dir, PluginSource::System, &mut outcome, &mut seen);
    }
    if search.bundled {
        scan_bundled(&mut outcome, &mut seen);
    }

    outcome
}

/// Scans a root plugin directory. Missing roots are skipped; unreadable roots
/// and entries become warnings.
fn scan_dir(
    root: &Path,
    source: PluginSource,
    outcome: &mut DiscoveryOutcome,
    seen: &mut BTreeMap<String, usize>,
) {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            log::debug!("plugin dir {} does not exist", root.display());
            return;
        }
        Err(err) => {
            outcome
                .warnings
                .push(format!("cannot read plugin dir {}: {err}", root.display()));
            return;
        }
    };

    // Collect and sort subdirectories so scan order is deterministic regardless
    // of the filesystem's directory order.
    let mut dirs = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                if entry.file_type().map(|ty| ty.is_dir()).unwrap_or(false) {
                    dirs.push(entry.path());
                }
            }
            Err(err) => outcome
                .warnings
                .push(format!("cannot read an entry of {}: {err}", root.display())),
        }
    }
    dirs.sort();

    for dir in dirs {
        let manifest_path = dir.join(MANIFEST_FILE);
        match fs::metadata(&manifest_path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => continue,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                outcome
                    .warnings
                    .push(format!("cannot access {}: {err}", manifest_path.display()));
                continue;
            }
        }
        load_plugin_dir(&dir, &manifest_path, source, outcome, seen);
    }
}

/// Loads one plugin directory: parse the manifest, resolve the wasm sidecar,
/// then insert unless the name is already taken.
fn load_plugin_dir(
    dir: &Path,
    manifest_path: &Path,
    source: PluginSource,
    outcome: &mut DiscoveryOutcome,
    seen: &mut BTreeMap<String, usize>,
) {
    let bytes = match fs::read(manifest_path) {
        Ok(bytes) => bytes,
        Err(err) => {
            outcome.errors.push(DiscoveryError {
                dir: Some(dir.to_path_buf()),
                source,
                message: format!("cannot read {}: {err}", manifest_path.display()),
            });
            return;
        }
    };

    let manifest = match Manifest::parse(&bytes) {
        Ok(manifest) => manifest,
        Err(err) => {
            outcome.errors.push(DiscoveryError {
                dir: Some(dir.to_path_buf()),
                source,
                message: format!("{err:#}"),
            });
            return;
        }
    };

    let name = manifest.name().to_owned();
    if let Some(&existing) = seen.get(&name) {
        outcome.warnings.push(format!(
            "plugin `{name}` in {} is shadowed by {} ({})",
            dir.display(),
            outcome.plugins[existing].source.label(),
            source_dir_label(&outcome.plugins[existing]),
        ));
        return;
    }

    let wasm_path = match resolve_wasm(dir, &name) {
        Ok(path) => path,
        Err(err) => {
            outcome.errors.push(DiscoveryError {
                dir: Some(dir.to_path_buf()),
                source,
                message: format!("{err:#}"),
            });
            return;
        }
    };
    let wasm_bytes = match fs::read(&wasm_path) {
        Ok(bytes) => bytes,
        Err(err) => {
            outcome.errors.push(DiscoveryError {
                dir: Some(dir.to_path_buf()),
                source,
                message: format!("cannot read wasm {}: {err}", wasm_path.display()),
            });
            return;
        }
    };

    let warnings = manifest.unknown_keys();
    let index = outcome.plugins.len();
    outcome.plugins.push(DiscoveredPlugin {
        name: name.clone(),
        manifest,
        manifest_path: Some(manifest_path.to_path_buf()),
        wasm_path: Some(wasm_path),
        wasm_bytes,
        source,
        source_dir: Some(dir.to_path_buf()),
        warnings,
    });
    seen.insert(name, index);
}

/// Registers the bundled detectors.
fn scan_bundled(outcome: &mut DiscoveryOutcome, seen: &mut BTreeMap<String, usize>) {
    for entry in bundled::all() {
        let manifest = match Manifest::parse(entry.manifest) {
            Ok(manifest) => manifest,
            Err(err) => {
                outcome.errors.push(DiscoveryError {
                    dir: None,
                    source: PluginSource::Bundled,
                    message: format!("bundled plugin `{}`: {err:#}", entry.name),
                });
                continue;
            }
        };

        let name = manifest.name().to_owned();
        if let Some(&existing) = seen.get(&name) {
            outcome.warnings.push(format!(
                "bundled plugin `{name}` is shadowed by {}",
                outcome.plugins[existing].source.label()
            ));
            continue;
        }

        let warnings = manifest.unknown_keys();
        let index = outcome.plugins.len();
        outcome.plugins.push(DiscoveredPlugin {
            name: name.clone(),
            manifest,
            manifest_path: None,
            wasm_path: None,
            wasm_bytes: entry.wasm.to_vec(),
            source: PluginSource::Bundled,
            source_dir: None,
            warnings,
        });
        seen.insert(name, index);
    }
}

/// Resolves the wasm sidecar inside a plugin directory.
fn resolve_wasm(dir: &Path, name: &str) -> Result<PathBuf> {
    let named = dir.join(format!("{name}.wasm"));
    if named.is_file() {
        return Ok(named);
    }
    let conventional = dir.join(WASM_FILE);
    if conventional.is_file() {
        return Ok(conventional);
    }

    let mut candidates: Vec<PathBuf> = fs::read_dir(dir)
        .with_context(|| format!("cannot read plugin dir {}", dir.display()))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "wasm"))
        .collect();
    candidates.sort();

    match candidates.len() {
        0 => bail!("no wasm sidecar found (looked for {name}.wasm and {WASM_FILE})"),
        1 => Ok(candidates.remove(0)),
        _ => bail!(
            "ambiguous wasm sidecar: {}",
            candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Renders a discovered plugin's source directory for shadow warnings.
fn source_dir_label(plugin: &DiscoveredPlugin) -> String {
    match &plugin.source_dir {
        Some(dir) => dir.display().to_string(),
        None => "embedded".to_owned(),
    }
}
