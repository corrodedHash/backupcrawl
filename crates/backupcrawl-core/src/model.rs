//! Result model of a crawl.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How a path relates to a backup mechanism.
///
/// The mapping from the three detector questions (managed? current?
/// propagated?) to a status is documented in section 3 of the rewrite plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// A verified copy exists.
    Clean,
    /// Managed by a mechanism, but the current content is not captured.
    Dirty,
    /// Captured locally, not propagated (shown as "Unsynced").
    Ahead,
    /// Managed, but the state could not be determined.
    Unknown,
    /// Deliberately excluded by the mechanism (`.gitignore`, `.stignore`).
    Ignored,
}

/// A single claimed path with its status and optional attributes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Path the claim is about.
    pub path: PathBuf,
    /// Claimed status.
    pub status: Status,
    /// Free-form details for `--all` output (e.g. `package = "bash"`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attrs: BTreeMap<String, String>,
}

/// Result of one detector for the whole scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginReport {
    /// Detector name (manifest `name`).
    pub plugin: String,
    /// Claimed entries.
    pub entries: Vec<Entry>,
    /// Number of failed calls; the detector keeps going until the circuit breaker trips.
    #[serde(default)]
    pub errors: u32,
    /// Human-readable notes (disabled plugin, degraded mode, ...).
    #[serde(default)]
    pub notes: Vec<String>,
}

/// Aggregated result of a crawl.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrawlResult {
    /// Directory the scan started at.
    pub root: PathBuf,
    /// Paths no detector claimed.
    pub loose_paths: Vec<PathBuf>,
    /// Paths that could not be read.
    pub denied_paths: Vec<PathBuf>,
    /// Symlinks no detector claimed, sorted by path (section 7.5).
    #[serde(default)]
    pub symlinks: Vec<SymlinkEntry>,
    /// One report per detector that ran.
    pub plugins: Vec<PluginReport>,
}

/// An unmanaged symlink seen by the walker (section 7.5).
///
/// The walker never follows symlinks (that stays deferred). A link a plugin
/// claimed — for example a package-owned link — is recorded in that plugin's
/// report instead; everything else lands here and is always reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymlinkEntry {
    /// Path of the symlink itself.
    pub path: PathBuf,
    /// Target the link points at, made absolute and lexically normalized
    /// (`..`/`.` resolved without touching the filesystem).
    pub target: PathBuf,
    /// Whether the target does not exist (including broken chains).
    pub dangling: bool,
    /// Set when the link is considered clean because the target is inside a
    /// clean-managed tree (a `clean` pruning directory claim, or an exactly
    /// claimed `clean` path); carries that target path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<PathBuf>,
}
