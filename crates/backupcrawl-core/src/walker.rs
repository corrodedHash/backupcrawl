//! Depth-first crawler with detector integration.
//!
//! The traversal mirrors the Python implementation: per directory, ignored
//! entries are skipped, unreadable files/directories are collected as denied,
//! sockets/FIFOs/devices are ignored, and directories, regular files and
//! symlinks are offered to the detectors.
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
//! # Symlinks (section 7.5)
//!
//! Symlinks are never followed. Each link is turned into a
//! [`LinkInfo`] and offered to detectors that declared the `symlink` hook; a
//! claim is recorded in that detector's panel like any other. Unclaimed links
//! are collected in [`CrawlResult::symlinks`] and are never dropped. After the
//! scan the **target-in-managed-tree rule** runs: a link whose target is inside
//! a `clean` *pruning* directory claim, or is itself an exactly claimed `clean`
//! path, is marked clean with `via = Some(target)`. Non-pruning directory claims
//! are deliberately not extended to their subtree.
//!
//! `init` runs once before the scan and `finish` once after; per-detector
//! results become [`PluginReport`]s with entries sorted by path.

use std::path::{Component, Path, PathBuf};

use log::{debug, warn};

use crate::detector::{Claim, DetectorError, DetectorReport, DetectorSlot, LinkInfo, ScanInfo};
use crate::ignore::IgnoreMatcher;
use crate::model::{CrawlResult, Entry, PluginReport, Status, SymlinkEntry};

/// Scans `root` depth-first, asking `detectors` in order, and returns the
/// aggregated result.
pub fn scan(root: &Path, ignore: &IgnoreMatcher, detectors: &mut [DetectorSlot]) -> CrawlResult {
    let mut run = Run::new(root, detectors);
    run.init_all();
    let dir = run.crawl(root, ignore);
    run.finish_all();

    let DirResult {
        loose_paths,
        denied_paths,
        mut symlinks,
        ..
    } = dir;
    run.mark_via(&mut symlinks);
    symlinks.sort_by(|left, right| left.path.cmp(&right.path));

    CrawlResult {
        root: root.to_path_buf(),
        loose_paths,
        denied_paths,
        symlinks,
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
    /// Unmanaged symlinks of this subtree (never dropped).
    symlinks: Vec<SymlinkEntry>,
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
    /// Paths claimed `clean` by a *pruning* directory claim; the
    /// target-in-managed-tree rule extends only over these.
    clean_prune_dirs: Vec<PathBuf>,
}

impl<'a> Run<'a> {
    fn new(root: &Path, detectors: &'a mut [DetectorSlot]) -> Self {
        let count = detectors.len();
        Self {
            root: root.to_path_buf(),
            detectors,
            active: vec![true; count],
            entries: vec![Vec::new(); count],
            clean_prune_dirs: Vec::new(),
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
            if claim.status == Status::Clean && claim.prune {
                self.clean_prune_dirs.push(dir.to_path_buf());
            }
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
        let mut symlinks = Vec::new();
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
                symlinks.push(path);
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
        symlinks.sort();

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

        // Symlinks: offered to the symlink hooks, never followed. Claimed links
        // stay in the plugin panel; the rest are reported in the symlink panel.
        for path in symlinks {
            let link = match link_info(&path) {
                Ok(link) => link,
                Err(err) => {
                    if err.kind() == std::io::ErrorKind::PermissionDenied {
                        out.denied_paths.push(path);
                    } else {
                        warn!("cannot inspect symlink {}: {err}", path.display());
                    }
                    continue;
                }
            };
            match self.ask_symlink(&link) {
                Some((index, claim)) => {
                    self.record(index, &path, &claim);
                    out.claimed = true;
                }
                None => out.symlinks.push(SymlinkEntry {
                    path: link.path,
                    target: link.target,
                    dangling: link.dangling,
                    via: None,
                }),
            }
        }

        for sub in dirs {
            let child = self.crawl(&sub, ignore);
            out.denied_paths.extend(child.denied_paths);
            out.symlinks.extend(child.symlinks);
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

    /// Asks the symlink hooks in order; first claim wins.
    fn ask_symlink(&mut self, link: &LinkInfo) -> Option<(usize, Claim)> {
        for index in 0..self.detectors.len() {
            if !self.is_asked(index, Hook::Symlink) {
                continue;
            }
            match self.detectors[index].detector.check_symlink(link) {
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
            Hook::Symlink => hooks.symlink,
        }
    }

    /// Applies the target-in-managed-tree rule (section 7.5).
    ///
    /// A link is marked clean when its target is exactly a claimed `clean` path
    /// or lies inside a `clean` **pruning** directory claim. Non-pruning claims
    /// are deliberately not extended to their subtree.
    fn mark_via(&self, links: &mut [SymlinkEntry]) {
        for link in links {
            let target = &link.target;
            let exactly_claimed = self
                .entries
                .iter()
                .flatten()
                .any(|entry| entry.status == Status::Clean && entry.path == *target);
            let inside_pruned = self
                .clean_prune_dirs
                .iter()
                .any(|dir| target.starts_with(dir));
            if exactly_claimed || inside_pruned {
                link.via = Some(target.clone());
            }
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
    Symlink,
}

/// Builds the [`LinkInfo`] for `path`, resolving the target to an absolute,
/// lexically normalized path without following symlink chains.
///
/// Lexical normalization (rather than `canonicalize`) keeps the target
/// comparable with the walker's own paths; it also works for dangling targets,
/// where canonicalization fails.
fn link_info(path: &Path) -> std::io::Result<LinkInfo> {
    let raw = std::fs::read_link(path)?;
    let joined = if raw.is_absolute() {
        raw
    } else {
        path.parent().unwrap_or_else(|| Path::new("/")).join(&raw)
    };
    let target = lexical_normalize(&joined);
    let dangling = !target.exists();
    Ok(LinkInfo {
        path: path.to_path_buf(),
        target,
        dangling,
    })
}

/// Lexically normalizes a path (`.`/`..` components, no filesystem access).
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(Path::new("/"));
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
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
