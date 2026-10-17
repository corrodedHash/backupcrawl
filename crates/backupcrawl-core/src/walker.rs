//! Parallel directory crawler with detector integration.
//!
//! The traversal mirrors the Python implementation: per directory, ignored
//! entries are skipped, unreadable files/directories are collected as denied,
//! sockets/FIFOs/devices are ignored, and directories, regular files and
//! symlinks are offered to the detectors.
//!
//! # Work distribution
//!
//! A crawl takes **one detector set per worker** ([`crawl`], `Vec<Vec<DetectorSlot>>`).
//! Directories are processed from a shared work queue: a worker pops a
//! directory, asks its own detector instances, and enqueues the readable child
//! directories (pruned directories enqueue nothing). Every path is handled by
//! exactly one worker, so first-claim-wins is evaluated exactly as in the
//! single-threaded walker.
//!
//! Each worker calls `init` once before its loop and `finish` once after. The
//! `init` calls are serialized across workers (one global mutex), so the first
//! worker warms the host cache and the rest hit it — the M4a single-flight
//! guard. A worker whose `init` (or a later check) disables a detector stops
//! asking that instance; the merged run report says the plugin is disabled.
//!
//! # Determinism
//!
//! Per-directory results are collected into a map while the worker threads run.
//! After they join, the directory tree is rebuilt and loose-path collapsing is
//! computed bottom-up exactly like the old recursion. Claims are merged and
//! sorted by path, denied paths and symlinks are sorted, and per-worker reports
//! are merged with summed errors and a stable union of notes. Scheduling never
//! influences output, so `--jobs 1` and `--jobs N` are byte-identical.
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
//!   at least one claim keeps its loose paths individually. The crawl root is
//!   never collapsed to itself.
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

use std::collections::{HashMap, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread;

use log::{debug, warn};

use crate::detector::{Claim, DetectorError, DetectorReport, DetectorSlot, LinkInfo, ScanInfo};
use crate::ignore::IgnoreMatcher;
use crate::model::{CrawlResult, Entry, PluginReport, Status, SymlinkEntry};
use crate::progress::{NoProgress, ProgressSink, ProgressSnapshot};

/// Scans `root` with a single worker and returns the aggregated result.
///
/// This is the convenience entry point for callers that do not need
/// parallelism (and for the core tests); it delegates to the same engine as
/// [`crawl`] with one detector set.
pub fn scan(root: &Path, ignore: &IgnoreMatcher, detectors: &mut [DetectorSlot]) -> CrawlResult {
    run_inline(root, ignore, detectors).result
}

/// Result of a crawl plus the merged lifecycle report of every detector.
///
/// `reports[i]` summarizes detector slot `i` across **all** workers: errors are
/// summed, notes are a stable union, and a detector disabled by any instance is
/// reported as disabled. The CLI uses this for the `plugins:` run line;
/// [`scan`] drops it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrawlOutcome {
    /// The aggregated crawl result.
    pub result: CrawlResult,
    /// One merged report per detector slot, in detector order.
    pub reports: Vec<DetectorReport>,
}

/// Crawls `root` with one detector set per worker, asking the detectors in
/// order.
///
/// `worker_sets` must contain one set per worker, each listing the same
/// detectors in the same order. Pass a single set for a serial crawl; [`scan`]
/// wraps that case. `progress` receives a snapshot after each directory.
pub fn crawl(
    root: &Path,
    ignore: &IgnoreMatcher,
    mut worker_sets: Vec<Vec<DetectorSlot>>,
    progress: &dyn ProgressSink,
) -> CrawlOutcome {
    if worker_sets.is_empty() {
        // No worker sets means "no detectors"; still walk the tree.
        worker_sets.push(Vec::new());
    }
    if worker_sets.len() == 1 {
        let mut set = worker_sets.pop().expect("one worker set");
        return run_inline_with(root, ignore, &mut set, progress);
    }
    run_parallel(root, ignore, worker_sets, progress)
}

/// Runs a crawl on the current thread with one detector set.
fn run_inline(root: &Path, ignore: &IgnoreMatcher, detectors: &mut [DetectorSlot]) -> CrawlOutcome {
    run_inline_with(root, ignore, detectors, &NoProgress)
}

fn run_inline_with(
    root: &Path,
    ignore: &IgnoreMatcher,
    detectors: &mut [DetectorSlot],
    progress: &dyn ProgressSink,
) -> CrawlOutcome {
    let names: Vec<String> = detectors.iter().map(|slot| slot.name.clone()).collect();
    let counters = Counters::default();
    let init_lock = Mutex::new(());
    let queue = TaskQueue::new(root);
    let mut worker = Worker::new(detectors);
    worker.init_all(root, &init_lock);
    drain(&mut worker, &queue, ignore, &counters, progress);
    worker.finish_all();
    assemble(root, names, vec![worker.into_outcome()])
}

/// Runs a crawl with one scoped thread per detector set.
fn run_parallel(
    root: &Path,
    ignore: &IgnoreMatcher,
    worker_sets: Vec<Vec<DetectorSlot>>,
    progress: &dyn ProgressSink,
) -> CrawlOutcome {
    let names: Vec<String> = worker_sets[0]
        .iter()
        .map(|slot| slot.name.clone())
        .collect();
    let counters = Counters::default();
    let init_lock = Mutex::new(());
    let queue = TaskQueue::new(root);

    let outcomes = thread::scope(|scope| {
        let handles: Vec<_> = worker_sets
            .into_iter()
            .map(|mut set| {
                let queue = &queue;
                let init_lock = &init_lock;
                let counters = &counters;
                scope.spawn(move || {
                    let mut worker = Worker::new(&mut set);
                    worker.init_all(root, init_lock);
                    drain(&mut worker, queue, ignore, counters, progress);
                    worker.finish_all();
                    worker.into_outcome()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("crawl worker panicked"))
            .collect()
    });

    assemble(root, names, outcomes)
}

/// Drains the shared queue until no directory is in flight anymore.
fn drain(
    worker: &mut Worker<'_>,
    queue: &TaskQueue,
    ignore: &IgnoreMatcher,
    counters: &Counters,
    progress: &dyn ProgressSink,
) {
    while let Some(dir) = queue.pop() {
        counters.directories_opened.fetch_add(1, Ordering::Relaxed);
        let outcome = worker.process(&dir, ignore, counters);
        let children = outcome.children.clone();
        worker.dirs.push((dir, outcome));
        counters.directories_closed.fetch_add(1, Ordering::Relaxed);
        queue.done(children);
        progress.update(counters.snapshot());
    }
}

/// Per-scan counters published through [`ProgressSink`].
#[derive(Debug, Default)]
struct Counters {
    directories_opened: AtomicU64,
    directories_closed: AtomicU64,
    files_seen: AtomicU64,
    claims: AtomicU64,
    denied: AtomicU64,
}

impl Counters {
    fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            directories_opened: self.directories_opened.load(Ordering::Relaxed),
            directories_closed: self.directories_closed.load(Ordering::Relaxed),
            files_seen: self.files_seen.load(Ordering::Relaxed),
            claims: self.claims.load(Ordering::Relaxed),
            denied: self.denied.load(Ordering::Relaxed),
        }
    }
}

/// Shared FIFO of directories plus an in-flight counter.
///
/// `pop` blocks while the queue is empty but directories are still being
/// processed. `done` retires the current directory and publishes its children
/// atomically, so a worker can never observe "empty and idle" while children
/// are about to appear.
struct TaskQueue {
    state: Mutex<QueueState>,
    cond: Condvar,
}

#[derive(Debug)]
struct QueueState {
    deque: VecDeque<PathBuf>,
    outstanding: usize,
}

impl TaskQueue {
    fn new(root: &Path) -> Self {
        let mut deque = VecDeque::new();
        deque.push_back(root.to_path_buf());
        Self {
            state: Mutex::new(QueueState {
                deque,
                outstanding: 1,
            }),
            cond: Condvar::new(),
        }
    }

    /// Pops the next directory, or `None` once every directory is done.
    fn pop(&self) -> Option<PathBuf> {
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        loop {
            if let Some(path) = state.deque.pop_front() {
                return Some(path);
            }
            if state.outstanding == 0 {
                return None;
            }
            state = self.cond.wait(state).unwrap_or_else(|err| err.into_inner());
        }
    }

    /// Retires the directory this worker just processed and enqueues `children`.
    fn done(&self, children: Vec<PathBuf>) {
        let mut state = self.state.lock().unwrap_or_else(|err| err.into_inner());
        debug_assert!(state.outstanding > 0);
        state.outstanding -= 1;
        for child in children {
            state.deque.push_back(child);
            state.outstanding += 1;
        }
        drop(state);
        self.cond.notify_all();
    }
}

/// Result of processing a single directory, before the tree-wide merge.
#[derive(Debug, Default)]
struct DirOutcome {
    /// Whether a detector claimed this directory or any path directly inside it.
    claimed: bool,
    /// Loose paths of this directory (its own files, not collapsed yet).
    loose: Vec<PathBuf>,
    /// Unreadable paths directly in this directory.
    denied: Vec<PathBuf>,
    /// Unmanaged symlinks directly in this directory.
    symlinks: Vec<SymlinkEntry>,
    /// Readable child directories, sorted; the queue descends into them.
    children: Vec<PathBuf>,
}

/// State owned by one worker thread.
struct Worker<'a> {
    detectors: &'a mut [DetectorSlot],
    /// Whether each detector is still asked (cleared on `Disabled`).
    active: Vec<bool>,
    /// Disable reason surfaced by the walker, per detector.
    disabled: Vec<Option<String>>,
    /// Claimed entries per detector.
    entries: Vec<Vec<Entry>>,
    /// Paths claimed `clean` by a *pruning* directory claim; the
    /// target-in-managed-tree rule extends only over these.
    clean_prune_dirs: Vec<PathBuf>,
    /// Per-directory results, merged after all workers finish.
    dirs: Vec<(PathBuf, DirOutcome)>,
}

impl<'a> Worker<'a> {
    fn new(detectors: &'a mut [DetectorSlot]) -> Self {
        let count = detectors.len();
        Self {
            detectors,
            active: vec![true; count],
            disabled: vec![None; count],
            entries: vec![Vec::new(); count],
            clean_prune_dirs: Vec::new(),
            dirs: Vec::new(),
        }
    }

    /// Calls `init` once per detector. The call is serialized by `init_lock`
    /// across workers so the first one warms the host cache. An error disables
    /// the detector for this worker (section 5.2 step 5).
    fn init_all(&mut self, root: &Path, init_lock: &Mutex<()>) {
        let user = current_user();
        for index in 0..self.detectors.len() {
            let info = ScanInfo {
                root: root.to_path_buf(),
                user: user.clone(),
                follow_symlinks: false,
                config: self.detectors[index].config.clone(),
            };
            // The guard is intentionally held for exactly one `init` call: the
            // first init of a plugin warms the host cache, the next workers'
            // inits then hit it.
            let result = {
                let _guard = init_lock.lock().unwrap_or_else(|err| err.into_inner());
                self.detectors[index].detector.init(&info)
            };
            if let Err(err) = result {
                warn!(
                    "detector `{}` disabled at init: {err}",
                    self.detectors[index].name
                );
                self.active[index] = false;
                self.disabled[index] = Some(match err {
                    DetectorError::Disabled { reason, .. } => reason,
                    DetectorError::Host(message) => message,
                });
            }
        }
    }

    /// Calls `finish` once per active detector; errors are logged and surfaced
    /// through [`Detector::report`](crate::detector::Detector::report).
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

    /// Consumes the worker into the data merged after the walk.
    fn into_outcome(mut self) -> WorkerOutcome {
        let reports = self
            .detectors
            .iter()
            .map(|slot| slot.detector.report())
            .collect();
        WorkerOutcome {
            dirs: std::mem::take(&mut self.dirs),
            entries: std::mem::take(&mut self.entries),
            clean_prune_dirs: std::mem::take(&mut self.clean_prune_dirs),
            disabled: std::mem::take(&mut self.disabled),
            reports,
        }
    }

    /// Processes one directory and returns its direct result (children are
    /// enqueued by the caller).
    fn process(&mut self, dir: &Path, ignore: &IgnoreMatcher, counters: &Counters) -> DirOutcome {
        debug!("Entering {}", dir.display());
        let mut out = DirOutcome::default();

        // The directory itself is offered to the directory hooks first.
        if let Some((index, claim)) = self.ask_dir(dir) {
            if claim.status == Status::Clean && claim.prune {
                self.clean_prune_dirs.push(dir.to_path_buf());
            }
            self.record(index, dir, &claim, counters);
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
                    out.denied.push(dir.to_path_buf());
                    counters.denied.fetch_add(1, Ordering::Relaxed);
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
                counters.files_seen.fetch_add(1, Ordering::Relaxed);
                if is_readable(&path, false) {
                    files.push(path);
                } else {
                    out.denied.push(path);
                    counters.denied.fetch_add(1, Ordering::Relaxed);
                }
            } else if file_type.is_dir() {
                if is_readable(&path, true) {
                    dirs.push(path);
                } else {
                    out.denied.push(path);
                    counters.denied.fetch_add(1, Ordering::Relaxed);
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
                    self.record(index, &file, &claim, counters);
                    out.claimed = true;
                }
                None => out.loose.push(file),
            }
        }

        // Symlinks: offered to the symlink hooks, never followed. Claimed links
        // stay in the plugin panel; the rest are reported in the symlink panel.
        for path in symlinks {
            let link = match link_info(&path) {
                Ok(link) => link,
                Err(err) => {
                    if err.kind() == std::io::ErrorKind::PermissionDenied {
                        out.denied.push(path);
                        counters.denied.fetch_add(1, Ordering::Relaxed);
                    } else {
                        warn!("cannot inspect symlink {}: {err}", path.display());
                    }
                    continue;
                }
            };
            match self.ask_symlink(&link) {
                Some((index, claim)) => {
                    self.record(index, &path, &claim, counters);
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

        out.children = dirs;
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

    /// Applies a failed call: `Disabled` removes the detector from the rest of
    /// this worker's scan; any other error is treated as "no claim" for this path.
    fn handle_error(&mut self, index: usize, err: DetectorError) {
        match err {
            DetectorError::Disabled { name, reason } => {
                warn!("detector `{name}` disabled: {reason}");
                self.active[index] = false;
                if self.disabled[index].is_none() {
                    self.disabled[index] = Some(reason);
                }
            }
            DetectorError::Host(message) => {
                warn!("detector `{}`: {message}", self.detectors[index].name);
            }
        }
    }

    fn record(&mut self, index: usize, path: &Path, claim: &Claim, counters: &Counters) {
        counters.claims.fetch_add(1, Ordering::Relaxed);
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

/// Data a worker hands to the merge step.
struct WorkerOutcome {
    dirs: Vec<(PathBuf, DirOutcome)>,
    entries: Vec<Vec<Entry>>,
    clean_prune_dirs: Vec<PathBuf>,
    disabled: Vec<Option<String>>,
    reports: Vec<DetectorReport>,
}

/// Merges per-worker results into the deterministic crawl result and reports.
fn assemble(root: &Path, names: Vec<String>, outcomes: Vec<WorkerOutcome>) -> CrawlOutcome {
    let count = names.len();

    // Claims: concatenate per detector across workers, then sort by path.
    let mut entries: Vec<Vec<Entry>> = vec![Vec::new(); count];
    // Clean pruning directory claims, deduplicated (order does not matter for
    // the `starts_with` checks, but keep it stable anyway).
    let mut clean_prune_dirs: Vec<PathBuf> = Vec::new();
    for outcome in &outcomes {
        for (index, list) in outcome.entries.iter().enumerate() {
            if let Some(dst) = entries.get_mut(index) {
                dst.extend(list.iter().cloned());
            }
        }
        clean_prune_dirs.extend(outcome.clean_prune_dirs.iter().cloned());
    }
    for list in &mut entries {
        list.sort_by(|left, right| left.path.cmp(&right.path));
    }
    clean_prune_dirs.sort();
    clean_prune_dirs.dedup();

    // Merged lifecycle reports: errors summed, notes a stable union, disabled if
    // any instance was disabled.
    let mut reports = Vec::with_capacity(count);
    for (index, name) in names.iter().enumerate() {
        let mut errors = 0u32;
        let mut calls = 0u64;
        let mut disabled = None;
        let mut notes: Vec<String> = Vec::new();
        for outcome in &outcomes {
            if let Some(report) = outcome.reports.get(index) {
                errors = errors.saturating_add(report.errors);
                calls = calls.saturating_add(report.calls);
                if disabled.is_none() {
                    disabled = report.disabled.clone();
                }
                for note in &report.notes {
                    if !notes.iter().any(|existing| existing == note) {
                        notes.push(note.clone());
                    }
                }
            }
            if disabled.is_none() {
                disabled = outcome
                    .disabled
                    .get(index)
                    .and_then(|reason| reason.clone());
            }
        }
        reports.push(DetectorReport {
            name: name.clone(),
            calls,
            errors,
            disabled,
            notes,
        });
    }
    // Notes are a de-duplicated union. Sort them so the output does not depend
    // on which worker happened to observe a note first.
    for report in &mut reports {
        report.notes.sort();
    }

    // Rebuild the directory map and collect the paths that always merge
    // globally.
    let mut dirs: HashMap<PathBuf, DirOutcome> = HashMap::new();
    let mut denied_paths = Vec::new();
    let mut symlinks = Vec::new();
    for outcome in outcomes {
        for (path, dir) in outcome.dirs {
            denied_paths.extend(dir.denied.iter().cloned());
            symlinks.extend(dir.symlinks.iter().cloned());
            dirs.insert(path, dir);
        }
    }
    denied_paths.sort();
    symlinks.sort_by(|left, right| left.path.cmp(&right.path));

    // Bottom-up loose collapse, identical to the old recursion.
    let loose_paths = collapse(root, &dirs).0;

    // Target-in-managed-tree rule (section 7.5). Runs after all claims exist.
    for link in &mut symlinks {
        let target = &link.target;
        let exactly_claimed = entries
            .iter()
            .flatten()
            .any(|entry| entry.status == Status::Clean && entry.path == *target);
        let inside_pruned = clean_prune_dirs.iter().any(|dir| target.starts_with(dir));
        if exactly_claimed || inside_pruned {
            link.via = Some(target.clone());
        }
    }

    let plugins = entries
        .into_iter()
        .enumerate()
        .map(|(index, entries)| PluginReport {
            plugin: names[index].clone(),
            entries,
            errors: reports[index].errors,
            notes: reports[index].notes.clone(),
        })
        .collect();

    CrawlOutcome {
        result: CrawlResult {
            root: root.to_path_buf(),
            loose_paths,
            denied_paths,
            symlinks,
            plugins,
        },
        reports,
    }
}

/// Collapses loose paths bottom-up and returns `(loose, claimed)` for `path`.
///
/// Mirrors the serial recursion exactly: a subtree with a claim keeps its loose
/// paths individually; a subtree without one collapses to its top directory.
fn collapse(path: &Path, dirs: &HashMap<PathBuf, DirOutcome>) -> (Vec<PathBuf>, bool) {
    let Some(dir) = dirs.get(path) else {
        return (Vec::new(), false);
    };
    let mut loose = dir.loose.clone();
    let mut claimed = dir.claimed;
    for child in &dir.children {
        let (child_loose, child_claimed) = collapse(child, dirs);
        claimed |= child_claimed;
        if child_claimed {
            loose.extend(child_loose);
        } else if !child_loose.is_empty() {
            // Fully loose subtree: report the directory, not its contents.
            loose.push(child.clone());
        }
    }
    (loose, claimed)
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
