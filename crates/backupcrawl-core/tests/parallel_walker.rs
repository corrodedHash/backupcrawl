//! Parallel crawler semantics (M4a).
//!
//! These tests drive [`walker::crawl`] with several detector sets. The fixture
//! detector is scriptable (directory/file/symlink claims, an optional disabled
//! directory) and reports lifecycle calls through a shared log, so the tests can
//! assert worker counts and that `init` never overlaps.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use backupcrawl_core::detector::{
    Claim, Detector, DetectorError, DetectorSlot, Hooks, LinkInfo, ScanInfo,
};
use backupcrawl_core::ignore::IgnoreMatcher;
use backupcrawl_core::progress::NoProgress;
use backupcrawl_core::walker;

fn matcher(patterns: &[&str]) -> IgnoreMatcher {
    IgnoreMatcher::new(patterns.iter().copied()).unwrap()
}

fn touch(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"x").unwrap();
}

/// Shared log of lifecycle and check calls.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn push(&self, entry: impl Into<String>) {
        self.0.lock().unwrap().push(entry.into());
    }

    fn entries(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }

    fn count(&self, entry: &str) -> usize {
        self.entries().iter().filter(|item| *item == entry).count()
    }
}

/// Records how many `init` calls overlap. The walker serializes `init`, so
/// `max` must stay at one even with several workers.
#[derive(Debug, Default)]
struct InitGuard {
    active: AtomicUsize,
    max: AtomicUsize,
    calls: AtomicUsize,
}

impl InitGuard {
    fn enter(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max.fetch_max(active, Ordering::SeqCst);
    }

    fn leave(&self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Everything a fixture instance should do, shared by all workers.
#[derive(Clone, Default)]
struct Script {
    dir_claims: BTreeMap<PathBuf, Claim>,
    file_claims: BTreeMap<PathBuf, Claim>,
    symlink_claims: BTreeMap<PathBuf, Claim>,
    /// A directory whose `check_dir` disables this instance.
    poison_dir: Option<PathBuf>,
    /// How long `init` sleeps (makes overlap observable).
    init_delay: Option<Duration>,
    init_guard: Option<Arc<InitGuard>>,
    log: Option<Log>,
}

impl Script {
    #[must_use]
    fn claim_dir(mut self, path: impl Into<PathBuf>, claim: Claim) -> Self {
        self.dir_claims.insert(path.into(), claim);
        self
    }

    #[must_use]
    fn claim_file(mut self, path: impl Into<PathBuf>, claim: Claim) -> Self {
        self.file_claims.insert(path.into(), claim);
        self
    }

    #[must_use]
    fn poison(mut self, path: impl Into<PathBuf>) -> Self {
        self.poison_dir = Some(path.into());
        self
    }

    #[must_use]
    fn log(mut self, log: Log) -> Self {
        self.log = Some(log);
        self
    }

    #[must_use]
    fn serialize_init(mut self, guard: Arc<InitGuard>, delay: Duration) -> Self {
        self.init_guard = Some(guard);
        self.init_delay = Some(delay);
        self
    }

    /// Builds one detector set per worker, each with an independent instance.
    fn sets(&self, workers: usize, name: &str) -> Vec<Vec<DetectorSlot>> {
        (0..workers)
            .map(|_| {
                vec![DetectorSlot::new(
                    name,
                    Vec::new(),
                    Hooks::all(),
                    Box::new(FixtureDetector {
                        name: name.to_owned(),
                        script: self.clone(),
                    }),
                )]
            })
            .collect()
    }
}

struct FixtureDetector {
    name: String,
    script: Script,
}

impl FixtureDetector {
    fn log(&self, entry: impl Into<String>) {
        if let Some(log) = &self.script.log {
            log.push(entry);
        }
    }

    fn claim(&self, table: &BTreeMap<PathBuf, Claim>, path: &Path) -> Option<Claim> {
        table.get(path).cloned()
    }
}

impl Detector for FixtureDetector {
    fn init(&mut self, _info: &ScanInfo) -> Result<(), DetectorError> {
        self.log(format!("init:{}", self.name));
        if let Some(guard) = &self.script.init_guard {
            guard.enter();
            if let Some(delay) = self.script.init_delay {
                std::thread::sleep(delay);
            }
            guard.leave();
        }
        Ok(())
    }

    fn check_dir(&mut self, path: &Path) -> Result<Option<Claim>, DetectorError> {
        self.log(format!("dir:{}:{}", self.name, path.display()));
        if self.script.poison_dir.as_deref() == Some(path) {
            return Err(DetectorError::Disabled {
                name: self.name.clone(),
                reason: format!("poisoned at {}", path.display()),
            });
        }
        Ok(self.claim(&self.script.dir_claims, path))
    }

    fn check_file(&mut self, path: &Path) -> Result<Option<Claim>, DetectorError> {
        self.log(format!("file:{}:{}", self.name, path.display()));
        Ok(self.claim(&self.script.file_claims, path))
    }

    fn check_symlink(&mut self, link: &LinkInfo) -> Result<Option<Claim>, DetectorError> {
        self.log(format!("symlink:{}:{}", self.name, link.path.display()));
        Ok(self.claim(&self.script.symlink_claims, &link.path))
    }

    fn finish(&mut self) -> Result<(), DetectorError> {
        self.log(format!("finish:{}", self.name));
        Ok(())
    }
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) {
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(target, link).unwrap();
}

/// Whether the process bypasses file permissions (running as root).
#[cfg(unix)]
fn bypasses_permissions(root: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let probe = root.join(".permission-probe");
    std::fs::write(&probe, b"x").unwrap();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o000)).unwrap();
    let bypass = std::fs::File::open(&probe).is_ok();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::remove_file(&probe).unwrap();
    bypass
}

/// Builds the deterministic fixture tree and returns whether denied paths are
/// observable (false when running as root).
#[cfg(unix)]
fn determinism_tree(root: &Path) -> bool {
    touch(&root.join("top.txt"));
    touch(&root.join("notes.txt.bak"));
    touch(&root.join("plain.txt"));
    touch(&root.join("repo/.git/HEAD"));
    touch(&root.join("repo/file.txt"));
    touch(&root.join("pkg/owned.conf"));
    touch(&root.join("pkg/loose.conf"));
    touch(&root.join("loose/a.txt"));
    touch(&root.join("loose/deep/b.txt"));
    touch(&root.join("farm/.dotbak"));
    touch(&root.join("farm/real.txt"));
    symlink(&root.join("farm/real.txt"), &root.join("link.txt"));
    symlink(Path::new("missing.txt"), &root.join("broken.txt"));

    let bypass = bypasses_permissions(root);
    if !bypass {
        use std::os::unix::fs::PermissionsExt;

        touch(&root.join("secret.txt"));
        touch(&root.join("locked/inner.txt"));
        std::fs::set_permissions(
            root.join("secret.txt"),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        std::fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
    }
    !bypass
}

#[cfg(unix)]
fn restore_permissions(root: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let _ = std::fs::set_permissions(
        root.join("secret.txt"),
        std::fs::Permissions::from_mode(0o644),
    );
    let _ = std::fs::set_permissions(root.join("locked"), std::fs::Permissions::from_mode(0o755));
}

/// Script that exercises prune, non-prune claims and file claims.
#[cfg(unix)]
fn determinism_script(root: &Path) -> Script {
    Script::default()
        .claim_dir(root.join("repo"), Claim::clean().prune())
        .claim_dir(root.join("farm"), Claim::clean().prune())
        .claim_dir(root.join("pkg"), Claim::clean().attr("package", "demo"))
        .claim_file(root.join("pkg/owned.conf"), Claim::dirty())
        .claim_file(root.join("notes.txt.bak"), Claim::dirty())
}

/// The same fixture at several job counts must serialize identically.
#[cfg(unix)]
#[test]
fn output_is_deterministic_across_job_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let denied_observable = determinism_tree(root);
    let script = determinism_script(root);

    let mut serialized = Vec::new();
    let mut first = None;
    for jobs in [1usize, 2, 4] {
        let sets = script.sets(jobs, "matrix");
        let outcome = walker::crawl(root, &matcher(&[]), sets, &NoProgress);
        assert_eq!(outcome.reports.len(), 1);
        if jobs == 1 {
            first = Some(outcome.result.clone());
        }
        serialized.push(serde_json::to_string_pretty(&outcome.result).unwrap());
    }
    restore_permissions(root);

    assert_eq!(serialized[0], serialized[1], "jobs 1 vs 2 differ");
    assert_eq!(serialized[0], serialized[2], "jobs 1 vs 4 differ");

    let result = first.unwrap();
    // Loose collapsing: the fully loose subtree is one entry; claimed subtrees
    // keep their loose files individually.
    assert!(result.loose_paths.contains(&root.join("loose")));
    assert!(result.loose_paths.contains(&root.join("pkg/loose.conf")));
    assert!(result.loose_paths.contains(&root.join("top.txt")));
    assert!(
        !result
            .loose_paths
            .iter()
            .any(|path| path.ends_with("repo/file.txt"))
    );

    // Claims from all workers merged and sorted by path.
    let entries = &result.plugins[0].entries;
    let paths: Vec<&PathBuf> = entries.iter().map(|entry| &entry.path).collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted, "entries are sorted by path");
    assert!(entries.iter().any(|entry| entry.path == root.join("repo")));
    assert!(
        entries
            .iter()
            .any(|entry| entry.path == root.join("pkg/owned.conf"))
    );

    // Symlinks merge, sorted by path; the farm link is clean via its target.
    assert_eq!(result.symlinks.len(), 2);
    assert_eq!(result.symlinks[0].path, root.join("broken.txt"));
    assert!(result.symlinks[0].dangling);
    assert_eq!(result.symlinks[1].path, root.join("link.txt"));
    assert_eq!(
        result.symlinks[1].via,
        Some(root.join("farm/real.txt")),
        "a clean pruning directory claim covers its subtree"
    );

    if denied_observable {
        assert!(result.denied_paths.contains(&root.join("secret.txt")));
        assert!(result.denied_paths.contains(&root.join("locked")));
        let mut denied = result.denied_paths.clone();
        denied.sort();
        assert_eq!(result.denied_paths, denied, "denied paths are sorted");
    }
}

#[test]
fn each_worker_runs_init_and_finish_once_per_instance() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(&root.join("a.txt"));
    touch(&root.join("sub/b.txt"));

    let log = Log::default();
    let script = Script::default().log(log.clone());
    let workers = 3;

    let outcome = walker::crawl(
        root,
        &matcher(&[]),
        script.sets(workers, "counted"),
        &NoProgress,
    );
    assert_eq!(outcome.reports.len(), 1);
    assert_eq!(log.count("init:counted"), workers);
    assert_eq!(log.count("finish:counted"), workers);
}

#[test]
fn init_is_serialized_across_workers() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(&root.join("a.txt"));
    touch(&root.join("b/c.txt"));

    let guard = Arc::new(InitGuard::default());
    let log = Log::default();
    let script = Script::default()
        .log(log.clone())
        .serialize_init(Arc::clone(&guard), Duration::from_millis(40));
    let workers = 4;

    walker::crawl(
        root,
        &matcher(&[]),
        script.sets(workers, "serial"),
        &NoProgress,
    );

    assert_eq!(guard.calls.load(Ordering::SeqCst), workers);
    assert_eq!(
        guard.max.load(Ordering::SeqCst),
        1,
        "init calls must not overlap"
    );
    assert_eq!(log.count("init:serial"), workers);
    assert_eq!(log.count("finish:serial"), workers);
}

#[test]
fn a_detector_disabled_in_one_worker_merges_into_the_report() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // Several siblings so more than one worker stays busy after the poison.
    touch(&root.join("poison/inside.txt"));
    for name in ["a", "b", "c", "d", "e"] {
        touch(&root.join(name).join("file.txt"));
    }

    let guard = Arc::new(InitGuard::default());
    let log = Log::default();
    let mut script = Script::default()
        .log(log.clone())
        .poison(root.join("poison"))
        .serialize_init(Arc::clone(&guard), Duration::from_millis(5));
    for name in ["a", "b", "c", "d", "e"] {
        script = script.claim_file(root.join(name).join("file.txt"), Claim::dirty());
    }
    let workers = 2;

    let outcome = walker::crawl(
        root,
        &matcher(&[]),
        script.sets(workers, "poisoned"),
        &NoProgress,
    );

    assert_eq!(outcome.reports.len(), 1);
    let disabled = outcome.reports[0]
        .disabled
        .as_deref()
        .expect("one worker's instance was disabled");
    assert!(disabled.contains("poison"), "got: {disabled}");
    // The other worker kept running and recording claims.
    assert!(
        !outcome.result.plugins[0].entries.is_empty(),
        "non-disabled workers still contribute claims"
    );
}
