//! Depth-first crawler with detector integration.
//!
//! The traversal mirrors the Python implementation: per directory, ignored
//! entries and symlinks are skipped, unreadable files/directories are collected
//! as denied, sockets/FIFOs/devices are ignored, and directories and regular
//! files are offered to the detectors.
//!
//! # Merge rules (section 6 of the rewrite plan)
//!
//! * Detectors are asked in the order the caller supplies them; the **first
//!   `Some(claim)` wins**. A detector that errors is skipped for that path (the
//!   call counts as "no claim") and is dropped from the rest of the scan if the
//!   error says so ([`DetectorError::Disabled`]).
//! * A directory claim with `prune = true` records the claim and does **not**
//!   descend; `prune = false` records the claim and descends normally, so a
//!   claimed package tree can still expose modified files and a git hook can
//!   report `.gitignore`d files.
//! * Files no detector claims stay loose.
//! * Loose-path collapsing is preserved: a directory whose subtree contains no
//!   claimed paths is reported as the directory itself instead of listing all of
//!   its files (this is what keeps `/usr`-style output readable). A subtree with
//!   at least one claim keeps its loose paths individually.
//!
//! `init` runs once before the scan and `finish` once after; per-detector
//! results become [`PluginReport`]s with entries sorted by path.
//!
//! Symlinks are still skipped entirely (the trait carries `check_symlink`, but
//! reporting/traversal is M2; section 7.5).

use std::path::{Path, PathBuf};

use log::{debug, warn};

use crate::detector::{Claim, DetectorError, DetectorReport, DetectorSlot, ScanInfo};
use crate::ignore::IgnoreMatcher;
use crate::model::{CrawlResult, Entry, PluginReport};

/// Scans `root` depth-first, asking `detectors` in order, and returns the
/// aggregated result.
pub fn scan(root: &Path, ignore: &IgnoreMatcher, detectors: &mut [DetectorSlot]) -> CrawlResult {
    let mut run = Run::new(root, detectors);
    run.init_all();
    let dir = run.crawl(root, ignore);
    run.finish_all();

    CrawlResult {
        root: root.to_path_buf(),
        loose_paths: dir.loose_paths,
        denied_paths: dir.denied_paths,
        plugins: run.reports(),
    }
}

/// Result of crawling a single directory, before merging into its parent.
#[derive(Debug, Default)]
struct DirResult {
    /// Loose paths of this subtree, already collapsed where possible.
    loose_paths: Vec<PathBuf>,
    /// Unreadable paths of this subtree.
    denied_paths: Vec<PathBuf>,
    /// Whether any detector claimed a path in this subtree (including the
    /// directory itself).
    claimed: bool,
}

/// Per-scan driver: owns the detector list, the active flags and the collected
/// claims.
struct Run<'a> {
    root: PathBuf,
    detectors: &'a mut [DetectorSlot],
    /// Whether each detector is still asked (cleared on `Disabled`).
    active: Vec<bool>,
    /// Claimed entries per detector, in traversal order.
    entries: Vec<Vec<Entry>>,
}

impl<'a> Run<'a> {
    fn new(root: &Path, detectors: &'a mut [DetectorSlot]) -> Self {
        let count = detectors.len();
        Self {
            root: root.to_path_buf(),
            detectors,
            active: vec![true; count],
            entries: vec![Vec::new(); count],
        }
    }

    /// Calls `init` once per detector. An error disables the detector with a
    /// visible warning (section 5.2 step 5).
    fn init_all(&mut self) {
        let user = current_user();
        for index in 0..self.detectors.len() {
            let info = ScanInfo {
                root: self.root.clone(),
                user: user.clone(),
                follow_symlinks: false,
                config: self.detectors[index].config.clone(),
            };
            if let Err(err) = self.detectors[index].detector.init(&info) {
                warn!(
                    "detector `{}` disabled at init: {err}",
                    self.detectors[index].name
                );
                self.active[index] = false;
            }
        }
    }

    /// Calls `finish` once per active detector; errors are logged and surfaced
    /// through [`Detector::report`].
    fn finish_all(&mut self) {
        for index in 0..self.detectors.len() {
            if !self.active[index] {
                continue;
            }
            if let Err(err) = self.detectors[index].detector.finish() {
                warn!(
                    "detector `{}` failed to finish: {err}",
                    self.detectors[index].name
                );
            }
        }
    }

    /// Builds one [`PluginReport`] per detector, with entries sorted by path.
    fn reports(&mut self) -> Vec<PluginReport> {
        let mut reports = Vec::with_capacity(self.detectors.len());
        for index in 0..self.detectors.len() {
            let mut entries = std::mem::take(&mut self.entries[index]);
            entries.sort_by(|left, right| left.path.cmp(&right.path));
            let DetectorReport { errors, notes, .. } = self.detectors[index].detector.report();
            reports.push(PluginReport {
                plugin: self.detectors[index].name.clone(),
                entries,
                errors,
                notes,
            });
        }
        reports
    }

    fn crawl(&mut self, dir: &Path, ignore: &IgnoreMatcher) -> DirResult {
        debug!("Entering {}", dir.display());
        let mut out = DirResult::default();

        // The directory itself is offered to the directory hooks first.
        if let Some((index, claim)) = self.ask_dir(dir) {
            self.record(index, dir, &claim);
            out.claimed = true;
            if claim.prune {
                // Recorded, not descended (git repos, syncthing folder roots).
                return out;
            }
        }

        let reader = match std::fs::read_dir(dir) {
            Ok(reader) => reader,
            Err(err) => {
                if err.kind() == std::io::ErrorKind::PermissionDenied {
                    out.denied_paths.push(dir.to_path_buf());
                } else {
                    warn!("cannot read {}: {err}", dir.display());
                }
                return out;
            }
        };

        let mut files = Vec::new();
        let mut dirs = Vec::new();
        for entry in reader {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    warn!("cannot read an entry in {}: {err}", dir.display());
                    continue;
                }
            };
            let path = entry.path();
            if ignore.is_ignored(&path) {
                continue;
            }
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(err) => {
                    warn!("cannot stat {}: {err}", path.display());
                    continue;
                }
            };
            if file_type.is_symlink() {
                // Symlinks are reported from M2 on; for now they are skipped.
                continue;
            } else if file_type.is_file() {
                if is_readable(&path, false) {
                    files.push(path);
                } else {
                    out.denied_paths.push(path);
                }
            } else if file_type.is_dir() {
                if is_readable(&path, true) {
                    dirs.push(path);
                } else {
                    out.denied_paths.push(path);
                }
            }
            // Sockets, FIFOs and device nodes are ignored, as in the Python tool.
        }

        files.sort();
        dirs.sort();

        // File detectors, first claim wins; unclaimed files stay loose.
        for file in files {
            match self.ask_file(&file) {
                Some((index, claim)) => {
                    self.record(index, &file, &claim);
                    out.claimed = true;
                }
                None => out.loose_paths.push(file),
            }
        }

        for sub in dirs {
            let child = self.crawl(&sub, ignore);
            out.denied_paths.extend(child.denied_paths);
            out.claimed |= child.claimed;
            if child.claimed {
                out.loose_paths.extend(child.loose_paths);
            } else if !child.loose_paths.is_empty() {
                // Fully loose subtree: report the directory, not its contents.
                out.loose_paths.push(sub);
            }
        }

        out
    }

    /// Asks the directory hooks in order; first claim wins.
    fn ask_dir(&mut self, path: &Path) -> Option<(usize, Claim)> {
        for index in 0..self.detectors.len() {
            if !self.is_asked(index, Hook::Dir) {
                continue;
            }
            match self.detectors[index].detector.check_dir(path) {
                Ok(Some(claim)) => return Some((index, claim)),
                Ok(None) => {}
                Err(err) => self.handle_error(index, err),
            }
        }
        None
    }

    /// Asks the file hooks in order; first claim wins.
    fn ask_file(&mut self, path: &Path) -> Option<(usize, Claim)> {
        for index in 0..self.detectors.len() {
            if !self.is_asked(index, Hook::File) {
                continue;
            }
            match self.detectors[index].detector.check_file(path) {
                Ok(Some(claim)) => return Some((index, claim)),
                Ok(None) => {}
                Err(err) => self.handle_error(index, err),
            }
        }
        None
    }

    /// Whether detector `index` is active and declared `hook`.
    fn is_asked(&self, index: usize, hook: Hook) -> bool {
        if !self.active[index] {
            return false;
        }
        let hooks = self.detectors[index].hooks;
        match hook {
            Hook::Dir => hooks.dir,
            Hook::File => hooks.file,
        }
    }

    /// Applies a failed call: `Disabled` removes the detector from the rest of
    /// the scan; any other error is treated as "no claim" for this path.
    fn handle_error(&mut self, index: usize, err: DetectorError) {
        match err {
            DetectorError::Disabled { name, reason } => {
                warn!("detector `{name}` disabled: {reason}");
                self.active[index] = false;
            }
            DetectorError::Host(message) => {
                warn!("detector `{}`: {message}", self.detectors[index].name);
            }
        }
    }

    fn record(&mut self, index: usize, path: &Path, claim: &Claim) {
        self.entries[index].push(claim.clone().into_entry(path));
    }
}

/// The check hook a call belongs to.
#[derive(Clone, Copy)]
enum Hook {
    Dir,
    File,
}

/// Best-effort current user for [`ScanInfo::user`].
fn current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_default()
}

/// Checks read access the way the Python tool did (`os.access`): files need
/// `R_OK`, directories need `R_OK | X_OK` to be traversable.
#[cfg(unix)]
fn is_readable(path: &Path, need_execute: bool) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let mode = if need_execute {
        libc::R_OK | libc::X_OK
    } else {
        libc::R_OK
    };
    match CString::new(path.as_os_str().as_bytes()) {
        // SAFETY: access(2) only reads the path and the process's credentials.
        Ok(cpath) => unsafe { libc::access(cpath.as_ptr(), mode) == 0 },
        // A path with an interior NUL cannot exist.
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_readable(path: &Path, _need_execute: bool) -> bool {
    // Best effort on non-Unix platforms; the tool targets Unix.
    path.exists()
}
