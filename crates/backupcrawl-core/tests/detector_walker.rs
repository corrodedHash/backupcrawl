//! Walker + detector integration semantics (M1.6 acceptance).
//!
//! Uses a native [`FixtureDetector`] test double so first-claim-wins, pruning,
//! claim-then-descend and loose-path interaction can be asserted without a wasm
//! toolchain. The fixture records every call through a shared [`Log`] so the
//! tests can prove which detector was asked (and which was not).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use backupcrawl_core::detector::{
    Claim, Detector, DetectorError, DetectorSlot, Hooks, LinkInfo, ScanInfo,
};
use backupcrawl_core::ignore::IgnoreMatcher;
use backupcrawl_core::walker;

fn matcher(patterns: &[&str]) -> IgnoreMatcher {
    IgnoreMatcher::new(patterns.iter().copied()).unwrap()
}

fn touch(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"x").unwrap();
}

/// A shared, ordered record of fixture calls.
#[derive(Clone, Default)]
struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn push(&self, entry: impl Into<String>) {
        self.0.lock().unwrap().push(entry.into());
    }

    fn entries(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }

    fn contains(&self, entry: &str) -> bool {
        self.0.lock().unwrap().iter().any(|item| item == entry)
    }
}

/// A scripted detector: claims the paths it was given, logs every call.
struct FixtureDetector {
    name: String,
    log: Log,
    dir_claims: BTreeMap<PathBuf, Claim>,
    file_claims: BTreeMap<PathBuf, Claim>,
    /// If set, `init` returns this error.
    init_error: Option<DetectorError>,
    /// If set, the matching check returns this error.
    check_error: Option<DetectorError>,
    /// If set, `check_file` returns this error (dirs unaffected).
    file_error: Option<DetectorError>,
}

impl FixtureDetector {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            log: Log::default(),
            dir_claims: BTreeMap::new(),
            file_claims: BTreeMap::new(),
            init_error: None,
            check_error: None,
            file_error: None,
        }
    }

    fn log(&self) -> Log {
        self.log.clone()
    }

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
    fn with_init_error(mut self, error: DetectorError) -> Self {
        self.init_error = Some(error);
        self
    }

    #[must_use]
    fn with_check_error(mut self, error: DetectorError) -> Self {
        self.check_error = Some(error);
        self
    }

    #[must_use]
    fn with_file_error(mut self, error: DetectorError) -> Self {
        self.file_error = Some(error);
        self
    }

    /// Boxes the detector into a walker slot, cloning the log out first.
    fn slot(self) -> (DetectorSlot, Log) {
        let log = self.log();
        let slot = DetectorSlot::new(self.name.clone(), Vec::new(), Hooks::all(), Box::new(self));
        (slot, log)
    }
}

impl Detector for FixtureDetector {
    fn init(&mut self, _info: &ScanInfo) -> Result<(), DetectorError> {
        self.log.push(format!("init:{}", self.name));
        match &self.init_error {
            Some(DetectorError::Disabled { name, reason }) => Err(DetectorError::Disabled {
                name: name.clone(),
                reason: reason.clone(),
            }),
            Some(DetectorError::Host(message)) => Err(DetectorError::Host(message.clone())),
            None => Ok(()),
        }
    }

    fn check_dir(&mut self, path: &Path) -> Result<Option<Claim>, DetectorError> {
        self.log
            .push(format!("dir:{}:{}", self.name, path.display()));
        self.check(path, true)
    }

    fn check_file(&mut self, path: &Path) -> Result<Option<Claim>, DetectorError> {
        self.log
            .push(format!("file:{}:{}", self.name, path.display()));
        self.check(path, false)
    }

    fn check_symlink(&mut self, link: &LinkInfo) -> Result<Option<Claim>, DetectorError> {
        self.log
            .push(format!("symlink:{}:{}", self.name, link.path.display()));
        Ok(None)
    }

    fn finish(&mut self) -> Result<(), DetectorError> {
        self.log.push(format!("finish:{}", self.name));
        Ok(())
    }
}

impl FixtureDetector {
    fn check(&self, path: &Path, dir: bool) -> Result<Option<Claim>, DetectorError> {
        let error = if dir {
            self.check_error.as_ref()
        } else {
            self.file_error.as_ref().or(self.check_error.as_ref())
        };
        if let Some(error) = error {
            return Err(match error {
                DetectorError::Disabled { name, reason } => DetectorError::Disabled {
                    name: name.clone(),
                    reason: reason.clone(),
                },
                DetectorError::Host(message) => DetectorError::Host(message.clone()),
            });
        }
        let table = if dir {
            &self.dir_claims
        } else {
            &self.file_claims
        };
        Ok(table.get(path).cloned())
    }
}

/// The entries of plugin report `index` as `(file name, status)` pairs.
fn entries(result: &backupcrawl_core::CrawlResult, index: usize) -> Vec<(String, &'static str)> {
    result.plugins[index]
        .entries
        .iter()
        .map(|entry| {
            let name = entry
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let label = match entry.status {
                backupcrawl_core::Status::Clean => "clean",
                backupcrawl_core::Status::Dirty => "dirty",
                backupcrawl_core::Status::Ahead => "ahead",
                backupcrawl_core::Status::Unknown => "unknown",
                backupcrawl_core::Status::Ignored => "ignored",
            };
            (name, label)
        })
        .collect()
}

#[test]
fn first_claim_wins_and_stops_asking_later_detectors() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(&root.join("a.txt"));
    touch(&root.join("b.txt"));

    let first = FixtureDetector::new("first").claim_file(root.join("a.txt"), Claim::dirty());
    let second = FixtureDetector::new("second")
        .claim_file(root.join("a.txt"), Claim::clean())
        .claim_file(root.join("b.txt"), Claim::clean());
    let (first_slot, _first_log) = first.slot();
    let (second_slot, second_log) = second.slot();

    let mut detectors = vec![first_slot, second_slot];
    let result = walker::scan(root, &matcher(&[]), &mut detectors);

    assert_eq!(entries(&result, 0), vec![("a.txt".to_owned(), "dirty")]);
    assert_eq!(entries(&result, 1), vec![("b.txt".to_owned(), "clean")]);
    assert!(result.loose_paths.is_empty());
    // The second detector must never have been asked about the claimed file.
    assert!(!second_log.contains(&format!("file:second:{}", root.join("a.txt").display())));
    assert!(second_log.contains(&format!("file:second:{}", root.join("b.txt").display())));
}

#[test]
fn prune_records_the_claim_without_descending() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(&root.join("repo/.git/HEAD"));
    touch(&root.join("repo/file.txt"));

    let detector = FixtureDetector::new("vcs").claim_dir(root.join("repo"), Claim::clean().prune());
    let (slot, log) = detector.slot();

    let mut detectors = vec![slot];
    let result = walker::scan(root, &matcher(&[]), &mut detectors);

    assert_eq!(entries(&result, 0), vec![("repo".to_owned(), "clean")]);
    // Nothing under the pruned directory was visited.
    assert!(!log.contains(&format!(
        "file:vcs:{}",
        root.join("repo/file.txt").display()
    )));
    assert!(!log.contains(&format!("dir:vcs:{}", root.join("repo/.git").display())));
    // A fully claimed tree has no loose and no denied paths.
    assert!(result.loose_paths.is_empty());
    assert!(result.denied_paths.is_empty());
}

#[test]
fn prune_preserves_the_claim_attrs() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("repo")).unwrap();

    let detector = FixtureDetector::new("vcs").claim_dir(
        root.join("repo"),
        Claim::clean().attr("bare", "true").prune(),
    );
    let (slot, _log) = detector.slot();

    let mut detectors = vec![slot];
    let result = walker::scan(root, &matcher(&[]), &mut detectors);

    let entry = &result.plugins[0].entries[0];
    assert_eq!(entry.attrs.get("bare").map(String::as_str), Some("true"));
}

#[test]
fn a_non_pruning_dir_claim_still_descends() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(&root.join("pkg/dirty.conf"));
    touch(&root.join("pkg/other.conf"));

    let detector = FixtureDetector::new("pacman")
        .claim_dir(root.join("pkg"), Claim::clean().attr("package", "bash"))
        .claim_file(root.join("pkg/dirty.conf"), Claim::dirty());
    let (slot, _log) = detector.slot();

    let mut detectors = vec![slot];
    let result = walker::scan(root, &matcher(&[]), &mut detectors);

    assert_eq!(
        entries(&result, 0),
        vec![
            ("pkg".to_owned(), "clean"),
            ("dirty.conf".to_owned(), "dirty")
        ]
    );
    // The unclaimed file inside the claimed directory stays loose and is kept
    // individually (the subtree has a claim, so it is not collapsed to `pkg`).
    assert_eq!(result.loose_paths, vec![root.join("pkg/other.conf")]);
}

#[test]
fn loose_paths_collapse_but_raw_claim_subtrees_do_not() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(&root.join("top.txt"));
    touch(&root.join("loose/a.txt"));
    touch(&root.join("loose/b.txt"));
    touch(&root.join("claimed/inner.txt"));

    let detector = FixtureDetector::new("marker").claim_dir(root.join("claimed"), Claim::clean());
    let (slot, _log) = detector.slot();

    let mut detectors = vec![slot];
    let result = walker::scan(root, &matcher(&[]), &mut detectors);

    // `top.txt` and `claimed/inner.txt` are individual; the fully loose
    // `loose/` subtree collapses to the directory itself.
    assert_eq!(
        result.loose_paths,
        vec![
            root.join("top.txt"),
            root.join("claimed/inner.txt"),
            root.join("loose")
        ]
    );
    assert_eq!(entries(&result, 0), vec![("claimed".to_owned(), "clean")]);
}

#[test]
fn detector_errors_are_no_claim_and_other_detectors_still_get_a_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(&root.join("a.txt"));

    let broken =
        FixtureDetector::new("broken").with_check_error(DetectorError::Host("boom".to_owned()));
    let fallback = FixtureDetector::new("fallback").claim_file(root.join("a.txt"), Claim::ahead());
    let (broken_slot, _broken_log) = broken.slot();
    let (fallback_slot, _fallback_log) = fallback.slot();

    let mut detectors = vec![broken_slot, fallback_slot];
    let result = walker::scan(root, &matcher(&[]), &mut detectors);

    assert!(result.plugins[0].entries.is_empty());
    assert_eq!(entries(&result, 1), vec![("a.txt".to_owned(), "ahead")]);
    assert!(result.loose_paths.is_empty());
}

#[test]
fn a_disabled_detector_is_not_asked_again() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(&root.join("a.txt"));

    let disabled = FixtureDetector::new("dead").with_file_error(DetectorError::Disabled {
        name: "dead".to_owned(),
        reason: "went away".to_owned(),
    });
    let (slot, log) = disabled.slot();

    let mut detectors = vec![slot];
    let result = walker::scan(root, &matcher(&[]), &mut detectors);

    // One failed call, then the walker stops asking; the file stays loose.
    assert_eq!(
        log.entries()
            .iter()
            .filter(|e| e.starts_with("file:"))
            .count(),
        1
    );
    assert_eq!(result.loose_paths, vec![root.join("a.txt")]);
}

#[test]
fn init_runs_once_before_and_finish_once_after_the_scan() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(&root.join("a.txt"));

    let detector = FixtureDetector::new("ordered");
    let (slot, log) = detector.slot();

    let mut detectors = vec![slot];
    walker::scan(root, &matcher(&[]), &mut detectors);

    let entries = log.entries();
    assert_eq!(entries.first().map(String::as_str), Some("init:ordered"));
    assert_eq!(entries.last().map(String::as_str), Some("finish:ordered"));
    // The root itself and every directory are offered to the dir hook.
    assert!(entries.contains(&format!("dir:ordered:{}", root.display())));
}

#[test]
fn an_init_failure_disables_the_detector_for_the_scan() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    touch(&root.join("a.txt"));

    let detector = FixtureDetector::new("dead").with_init_error(DetectorError::Disabled {
        name: "dead".to_owned(),
        reason: "cannot prepare".to_owned(),
    });
    let (slot, log) = detector.slot();

    let mut detectors = vec![slot];
    let result = walker::scan(root, &matcher(&[]), &mut detectors);

    assert!(!log.entries().iter().any(|e| e.starts_with("file:")));
    assert!(!log.entries().iter().any(|e| e.starts_with("finish:")));
    assert_eq!(result.loose_paths, vec![root.join("a.txt")]);
}
