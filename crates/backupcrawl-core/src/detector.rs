//! The transport-neutral detector façade.
//!
//! This module defines the small record set the walker shares with detectors
//! ([`ScanInfo`], [`Claim`], [`LinkInfo`], [`DetectorError`]) and the
//! [`Detector`] trait itself (section 4 of `docs/rust-rewrite-plan.md`). It is
//! deliberately free of any host dependency so the crawl logic can be tested
//! with a native test double ([`FixtureDetector`] lives in `tests/`).
//!
//! # One record set, two transports
//!
//! The records are defined **here, in core**, and the wasm host re-exports them
//! (`backupcrawl_host::{Claim, ScanInfo, ...}` are aliases for these types).
//! `WasmDetector` then implements [`Detector`] by delegating to its inherent
//! WIT-calling methods; there is no parallel type zoo. The wasmtime/WIT glue
//! stays an implementation detail below the trait.
//!
//! One adaptation is documented in place: the WIT `claim.attrs` is a *list* of
//! key/value pairs, so [`Claim::attrs`] is a `Vec<Attr>`; when a claim becomes a
//! [`Entry`](crate::model::Entry) the pairs are folded into the entry's
//! `BTreeMap` (sorted, later duplicates win), which is what keeps JSON output
//! deterministic.
//!
//! # Symbols
//!
//! The trait mirrors the WIT `detector` interface one-to-one
//! (`init`/`check-dir`/`check-file`/`check-symlink`/`finish`). The optional
//! [`Detector::report`] accessor exposes the host's lifecycle accounting
//! (errors, notes, disable reason) so the walker can build a
//! [`PluginReport`](crate::model::PluginReport) without duplicating the circuit
//! breaker bookkeeping. The walker still carries `check_symlink` but keeps
//! **skipping** symlinks until M2 (section 7.5 / M1.6).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::model::{Entry, Status};

/// A free-form key/value pair attached to a [`Claim`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attr {
    /// Attribute name, e.g. `package`.
    pub key: String,
    /// Attribute value, e.g. `bash`.
    pub value: String,
}

impl Attr {
    /// Creates an attribute pair.
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

/// A detector's verdict for one path.
///
/// `None` returned by the check methods means "not managed by me"; this record
/// is the `Some(claim)` case. It is the core twin of the WIT `types.claim` and
/// of the host's re-export of the same type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// The verdict.
    pub status: Status,
    /// Details shown in `--all` output.
    pub attrs: Vec<Attr>,
    /// Do not descend into this directory (only meaningful for `check_dir`).
    pub prune: bool,
}

impl Claim {
    /// A claim with `status` and no attributes.
    pub fn new(status: Status) -> Self {
        Self {
            status,
            attrs: Vec::new(),
            prune: false,
        }
    }

    /// A `clean` claim.
    pub fn clean() -> Self {
        Self::new(Status::Clean)
    }

    /// A `dirty` claim.
    pub fn dirty() -> Self {
        Self::new(Status::Dirty)
    }

    /// An `ahead` claim (rendered "Unsynced").
    pub fn ahead() -> Self {
        Self::new(Status::Ahead)
    }

    /// An `unknown` claim.
    pub fn unknown() -> Self {
        Self::new(Status::Unknown)
    }

    /// An `ignored` claim.
    pub fn ignored() -> Self {
        Self::new(Status::Ignored)
    }

    /// Appends an attribute.
    #[must_use]
    pub fn attr(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attrs.push(Attr::new(key, value));
        self
    }

    /// Marks the claim as pruning the subtree.
    #[must_use]
    pub fn prune(mut self) -> Self {
        self.prune = true;
        self
    }

    /// Folds the attribute list into the sorted map used by [`Entry`].
    pub fn attr_map(&self) -> BTreeMap<String, String> {
        self.attrs
            .iter()
            .map(|attr| (attr.key.clone(), attr.value.clone()))
            .collect()
    }

    /// Converts the claim into a [`Entry`] for `path`.
    pub fn into_entry(self, path: &Path) -> Entry {
        Entry {
            path: path.to_path_buf(),
            status: self.status,
            attrs: self.attr_map(),
        }
    }
}

/// Context for one scan, handed to [`Detector::init`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanInfo {
    /// Directory the crawl started at.
    pub root: PathBuf,
    /// Name of the user running the crawl.
    pub user: String,
    /// Whether the walker follows symlinks (always `false` in M1).
    pub follow_symlinks: bool,
    /// This detector's configuration table (`[plugins.<name>]`), flattened.
    pub config: Vec<(String, String)>,
}

/// A symlink seen by the walker, handed to [`Detector::check_symlink`].
///
/// The walker does not call this yet (symlinks are skipped until M2), but the
/// record is part of the frozen contract so plugins can implement the hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkInfo {
    /// Path of the symlink itself.
    pub path: PathBuf,
    /// Target the link points at (not necessarily resolved).
    pub target: PathBuf,
    /// Whether the target does not exist.
    pub dangling: bool,
}

/// A detector lifecycle failure.
///
/// [`DetectorError::Disabled`] tells the walker to stop asking the detector for
/// the rest of the scan (init failure or circuit breaker); anything else is a
/// host-side failure the walker treats as "no claim" for that call while it
/// keeps asking later calls. Both carry a human-readable message for the run
/// report.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DetectorError {
    /// The detector is disabled for the rest of the scan.
    #[error("detector `{name}` is disabled: {reason}")]
    Disabled {
        /// Detector name.
        name: String,
        /// Reason for the run report.
        reason: String,
    },
    /// A host-side failure (transport, serialization, ...).
    #[error("{0}")]
    Host(String),
}

/// Lifecycle summary of one detector, used to build a
/// [`PluginReport`](crate::model::PluginReport) and the run report line.
///
/// The defaults are all-empty, so a test double that does not care about
/// accounting can rely on the [`Detector::report`] default implementation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetectorReport {
    /// Detector name.
    pub name: String,
    /// Number of guest calls made (checks plus `init`/`finish`).
    pub calls: u64,
    /// Number of failed calls.
    pub errors: u32,
    /// Disable reason, if the detector was disabled mid-scan.
    pub disabled: Option<String>,
    /// Human-readable notes (init failure, breaker trip, finish error, ...).
    pub notes: Vec<String>,
}

/// Which filesystem objects a detector wants to be asked about.
///
/// Derived from the manifest `[hooks]` table by the caller; the walker only
/// calls `check_dir`/`check_file`/`check_symlink` on detectors that declared the
/// matching hook (section 5.3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hooks {
    /// Whether `check_dir` should be called.
    pub dir: bool,
    /// Whether `check_file` should be called.
    pub file: bool,
    /// Whether `check_symlink` should be called (unused until M2).
    pub symlink: bool,
}

impl Hooks {
    /// A hook set that wants every object kind.
    pub fn all() -> Self {
        Self {
            dir: true,
            file: true,
            symlink: true,
        }
    }
}

/// The core-facing detector façade.
///
/// Implemented by the wasm host's `WasmDetector` and by the native
/// `FixtureDetector` used in tests. `Send` because the walker may hand
/// detectors to worker threads once parallel crawling lands (section 9).
///
/// # Per-worker lifecycle and cache rules (frozen, M2.0)
///
/// M4a runs one detector **instance per worker**, so a plugin must not assume a
/// single instance per process. The contract, which holds for any `--jobs`
/// value:
///
/// * `init`/`finish` bracket **that instance's** lifetime: a scan with N
///   workers calls each plugin's `init` and `finish` N times.
/// * Instances share no mutable state; the only shared channel is the host
///   cache. Global preparation belongs there, and cache-write races between
///   workers must be benign (last writer wins is acceptable). The first
///   worker's `init` is serialized and warms the cache; the rest hit it.
/// * There are no cross-worker ordering guarantees. First-claim-wins is still
///   evaluated per path, and every path is handled by exactly one worker.
/// * Output stays deterministic: per-worker claims are merged and sorted in the
///   host after the walk, so `--jobs 1` and `--jobs N` produce identical output.
///
/// Plugins must not rely on process-global mutable state or on hook order
/// across unrelated paths.
pub trait Detector: Send {
    /// Called once per instance before the scan. Expensive preparation belongs
    /// here; an error disables the detector for this scan.
    fn init(&mut self, info: &ScanInfo) -> Result<(), DetectorError>;

    /// Asks for a verdict on a directory.
    fn check_dir(&mut self, path: &Path) -> Result<Option<Claim>, DetectorError>;

    /// Asks for a verdict on a regular file.
    fn check_file(&mut self, path: &Path) -> Result<Option<Claim>, DetectorError>;

    /// Asks for a verdict on a symlink.
    fn check_symlink(&mut self, link: &LinkInfo) -> Result<Option<Claim>, DetectorError>;

    /// Called once after the scan; may flush diagnostics.
    fn finish(&mut self) -> Result<(), DetectorError>;

    /// Lifecycle accounting for the run report.
    ///
    /// Defaults to an empty report. The wasm host overrides it with its circuit
    /// breaker / error counters; native test doubles usually leave it alone.
    fn report(&self) -> DetectorReport {
        DetectorReport::default()
    }
}

/// A detector plus the metadata the walker needs to ask it.
///
/// The name labels the [`PluginReport`](crate::model::PluginReport); `config` is
/// handed to [`Detector::init`] as [`ScanInfo::config`]; `hooks` gates which
/// check methods are called.
pub struct DetectorSlot {
    /// Report label (manifest `name`).
    pub name: String,
    /// Flattened `[plugins.<name>]` table.
    pub config: Vec<(String, String)>,
    /// Which object kinds the detector declared.
    pub hooks: Hooks,
    /// The detector itself.
    pub detector: Box<dyn Detector>,
}

impl DetectorSlot {
    /// Builds a slot.
    pub fn new(
        name: impl Into<String>,
        config: Vec<(String, String)>,
        hooks: Hooks,
        detector: Box<dyn Detector>,
    ) -> Self {
        Self {
            name: name.into(),
            config,
            hooks,
            detector,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M4a hands one detector slot to each worker, so the trait object must be
    /// `Send`. This is a compile-time pin, not a behavioral test.
    #[test]
    fn detector_slots_are_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Box<dyn Detector>>();
        assert_send::<DetectorSlot>();
    }
}
