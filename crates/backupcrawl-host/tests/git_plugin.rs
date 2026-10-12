//! M1.8 golden tests: the bundled `git` detector component against fixture
//! repositories created with the `git` CLI, driven through the real host
//! [`Runtime`] and [`WasmDetector`].
//!
//! This ports the M1.7 feasibility spike's fixture suite
//! (`spikes/gix-feasibility/runner/tests/fixtures.rs`) from "component returns
//! a JSON summary" to "detector returns claims", covering the §12 M1.8
//! acceptance cases: clean, dirty, ahead, behind, no-upstream, detached,
//! unborn, linked worktree, bare, ignored (matching/all), nested, ancestor,
//! staged rename, merge conflict, packed objects and non-repositories.
//!
//! The fixtures use a hermetic `git` environment (no system/global config,
//! fixed author/committer) exactly like the spike.

use std::path::{Path, PathBuf};
use std::process::Command;

use backupcrawl_core::Status;
use backupcrawl_host::manifest::Manifest;
use backupcrawl_host::{CapabilityPolicy, Claim, Limits, Runtime, ScanInfo, WasmDetector, bundled};

/// Name of the bundled detector under test.
const PLUGIN: &str = "git";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// A runtime plus the detector instances instantiated from the bundled git
/// component.
struct Harness {
    runtime: Runtime,
    /// Kept alive so the per-plugin cache directory outlives the detectors.
    _cache: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let cache = tempfile::tempdir().expect("cache tempdir");
        let runtime = Runtime::with_cache_root(Limits::default(), cache.path()).expect("runtime");
        Self {
            runtime,
            _cache: cache,
        }
    }

    /// Instantiates and initializes the bundled git detector with `config`.
    fn detector(&self, root: &Path, config: &[(&str, &str)]) -> WasmDetector {
        let plugin = bundled::all()
            .iter()
            .find(|plugin| plugin.name == PLUGIN)
            .expect("git must be in the bundled registry");
        let manifest = Manifest::parse(plugin.manifest).expect("bundled git manifest");
        let policy = CapabilityPolicy::from_manifest(&manifest);
        let mut detector = self
            .runtime
            .instantiate(plugin.wasm, PLUGIN, policy)
            .expect("instantiate git component");
        detector
            .init(&scan_info(root, config))
            .unwrap_or_else(|err| panic!("init git detector: {err}"));
        detector
    }
}

fn scan_info(root: &Path, config: &[(&str, &str)]) -> ScanInfo {
    ScanInfo {
        root: root.to_path_buf(),
        user: "tester".to_owned(),
        follow_symlinks: false,
        config: config
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect(),
    }
}

/// Asserts a directory is claimed and returns the claim.
fn dir_claim(detector: &mut WasmDetector, path: &Path) -> Claim {
    detector
        .check_dir(path)
        .unwrap_or_else(|err| panic!("check_dir {}: {err}", path.display()))
        .unwrap_or_else(|| panic!("expected a claim for {}", path.display()))
}

/// Looks up one claim attribute.
fn attr<'a>(claim: &'a Claim, key: &str) -> Option<&'a str> {
    claim
        .attrs
        .iter()
        .find(|attr| attr.key == key)
        .map(|attr| attr.value.as_str())
}

/// The status of a file claim, or `None` when unclaimed.
fn file_status(detector: &mut WasmDetector, path: &Path) -> Option<Status> {
    detector
        .check_file(path)
        .unwrap_or_else(|err| panic!("check_file {}: {err}", path.display()))
        .map(|claim| claim.status)
}

// ---------------------------------------------------------------------------
// Fixtures (ported from the M1.7 spike)
// ---------------------------------------------------------------------------

/// A git fixture in a temporary directory, kept alive by the struct.
struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("fixture tempdir"),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Writes a file relative to the fixture root, creating parents.
    fn write(&self, rel: &str, contents: &str) {
        let path = self.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture directory");
        }
        std::fs::write(&path, contents).expect("write fixture file");
    }

    fn git(&self, args: &[&str]) -> String {
        git_in(self.path(), args)
    }

    /// `git init` with `main` as the initial branch.
    fn init(&self) {
        self.git(&["init", "-q", "-b", "main"]);
    }

    /// Stages everything and commits it.
    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-qm", message]);
    }
}

/// Runs `git` in `cwd` with a hermetic environment and returns stdout.
fn git_in(cwd: &Path, args: &[&str]) -> String {
    let output = git_command(cwd, args).expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed:\n{}",
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Runs `git` in `cwd` expecting failure (a conflicting merge) and returns stdout.
fn git_expect_failure(cwd: &Path, args: &[&str]) -> String {
    let output = git_command(cwd, args).expect("spawn git");
    assert!(
        !output.status.success(),
        "git {args:?} in {} unexpectedly succeeded",
        cwd.display()
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn git_command(cwd: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "spike")
        .env("GIT_AUTHOR_EMAIL", "spike@example.com")
        .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00+00:00")
        .env("GIT_COMMITTER_NAME", "spike")
        .env("GIT_COMMITTER_EMAIL", "spike@example.com")
        .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00+00:00")
        .output()
}

/// Creates a bare remote under `remote_root` and pushes `main` to it, leaving
/// the repository at `fx` clean with an upstream.
///
/// `remote_root` must be **outside** the fixture worktree: a bare repository
/// inside it would show up as an untracked entry.
fn with_upstream(fx: &Fixture, remote_root: &Path) -> PathBuf {
    let remote = remote_root.join("remote.git");
    git_in(
        fx.path(),
        &[
            "init",
            "--bare",
            "-q",
            "-b",
            "main",
            remote.to_str().unwrap(),
        ],
    );
    fx.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    fx.git(&["push", "-q", "-u", "origin", "main"]);
    remote
}

// ---------------------------------------------------------------------------
// Managed detection
// ---------------------------------------------------------------------------

#[test]
fn clean_repository_with_upstream_is_clean() {
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "hello\n");
    fx.write("src/main.rs", "fn main() {}\n");
    fx.commit("initial");
    let remote = tempfile::tempdir().unwrap();
    with_upstream(&fx, remote.path());

    let harness = Harness::new();
    let mut detector = harness.detector(fx.path(), &[]);
    let claim = dir_claim(&mut detector, fx.path());

    assert_eq!(claim.status, Status::Clean);
    assert_eq!(attr(&claim, "kind"), Some("common"));
    assert_eq!(attr(&claim, "branch"), Some("main"));
    assert_eq!(attr(&claim, "upstream"), Some("origin/main"));
    assert_eq!(attr(&claim, "ahead"), Some("0"));
    assert_eq!(attr(&claim, "behind"), None);
    assert!(claim.prune, "the default config prunes the repository");
    assert_eq!(detector.report().errors, 0);
}

#[test]
fn dirty_modified_and_untracked_is_dirty() {
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "hello\n");
    fx.commit("initial");
    fx.write("tracked.txt", "changed\n");
    fx.write("new-file.txt", "new\n");

    let harness = Harness::new();
    let mut detector = harness.detector(fx.path(), &[]);
    let claim = dir_claim(&mut detector, fx.path());

    assert_eq!(claim.status, Status::Dirty);
    // The file hook is served from the per-repository cache.
    assert_eq!(
        file_status(&mut detector, &fx.path().join("tracked.txt")),
        Some(Status::Dirty)
    );
    assert_eq!(
        file_status(&mut detector, &fx.path().join("new-file.txt")),
        Some(Status::Dirty)
    );
    assert_eq!(detector.report().errors, 0);
}

#[test]
fn ahead_of_upstream_is_ahead() {
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "one\n");
    fx.commit("c1");
    let remote = tempfile::tempdir().unwrap();
    with_upstream(&fx, remote.path());
    fx.write("tracked.txt", "two\n");
    fx.commit("c2");

    let harness = Harness::new();
    let mut detector = harness.detector(fx.path(), &[]);
    let claim = dir_claim(&mut detector, fx.path());

    assert_eq!(claim.status, Status::Ahead);
    assert_eq!(attr(&claim, "upstream"), Some("origin/main"));
    assert_eq!(attr(&claim, "ahead"), Some("1"));
    assert_eq!(attr(&claim, "behind"), None);
}

#[test]
fn behind_upstream_is_clean_with_a_behind_attribute() {
    let fx = Fixture::new();
    let upstream = fx.path().join("upstream.git");
    git_in(
        fx.path(),
        &[
            "init",
            "--bare",
            "-q",
            "-b",
            "main",
            upstream.to_str().unwrap(),
        ],
    );

    let author = fx.path().join("author");
    std::fs::create_dir_all(&author).unwrap();
    git_in(&author, &["init", "-q", "-b", "main"]);
    std::fs::write(author.join("tracked.txt"), "one\n").unwrap();
    git_in(&author, &["add", "-A"]);
    git_in(&author, &["commit", "-qm", "c1"]);
    git_in(
        &author,
        &["remote", "add", "origin", upstream.to_str().unwrap()],
    );
    git_in(&author, &["push", "-q", "-u", "origin", "main"]);

    git_in(
        fx.path(),
        &["clone", "-q", upstream.to_str().unwrap(), "clone"],
    );
    std::fs::write(author.join("tracked.txt"), "two\n").unwrap();
    git_in(&author, &["add", "-A"]);
    git_in(&author, &["commit", "-qm", "c2"]);
    git_in(&author, &["push", "-q", "origin", "main"]);

    let clone = fx.path().join("clone");
    git_in(&clone, &["fetch", "-q", "origin"]);

    let harness = Harness::new();
    let mut detector = harness.detector(&clone, &[]);
    let claim = dir_claim(&mut detector, &clone);

    // Behind is not a backup problem: `clean`, with `behind` noted (§7.1).
    assert_eq!(claim.status, Status::Clean);
    assert_eq!(attr(&claim, "ahead"), Some("0"));
    assert_eq!(attr(&claim, "behind"), Some("1"));
}

#[test]
fn no_upstream_defaults_to_unknown() {
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "one\n");
    fx.commit("c1");

    let harness = Harness::new();
    let mut detector = harness.detector(fx.path(), &[]);
    let claim = dir_claim(&mut detector, fx.path());

    assert_eq!(claim.status, Status::Unknown);
    assert_eq!(attr(&claim, "reason"), Some("no-upstream"));
    assert_eq!(attr(&claim, "branch"), Some("main"));
}

#[test]
fn no_upstream_policy_is_configurable() {
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "one\n");
    fx.commit("c1");

    let harness = Harness::new();
    let mut clean = harness.detector(fx.path(), &[("no-upstream", "clean")]);
    assert_eq!(dir_claim(&mut clean, fx.path()).status, Status::Clean);

    let mut ahead = harness.detector(fx.path(), &[("no-upstream", "ahead")]);
    assert_eq!(dir_claim(&mut ahead, fx.path()).status, Status::Ahead);
}

#[test]
fn detached_head_is_unknown_but_dirty_wins() {
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "one\n");
    fx.commit("c1");
    fx.git(&["checkout", "-q", "--detach", "HEAD"]);

    let harness = Harness::new();
    let mut detector = harness.detector(fx.path(), &[]);
    let claim = dir_claim(&mut detector, fx.path());
    assert_eq!(claim.status, Status::Unknown);
    assert_eq!(attr(&claim, "head"), Some("detached"));
    assert_eq!(attr(&claim, "reason"), Some("detached-head"));

    fx.write("tracked.txt", "two\n");
    // A fresh instance is a fresh scan; the modified file must win over the
    // detached HEAD.
    let mut detector = harness.detector(fx.path(), &[]);
    let dirty = dir_claim(&mut detector, fx.path());
    assert_eq!(dirty.status, Status::Dirty);
}

#[test]
fn unborn_branch_is_unknown_then_dirty() {
    let fx = Fixture::new();
    fx.init();

    let harness = Harness::new();
    let mut detector = harness.detector(fx.path(), &[]);
    let claim = dir_claim(&mut detector, fx.path());
    assert_eq!(claim.status, Status::Unknown);
    assert_eq!(attr(&claim, "head"), Some("unborn"));
    assert_eq!(attr(&claim, "reason"), Some("unborn"));

    fx.write("new-file.txt", "new\n");
    let mut detector = harness.detector(fx.path(), &[]);
    let dirty = dir_claim(&mut detector, fx.path());
    assert_eq!(dirty.status, Status::Dirty);
}

#[test]
fn linked_worktree_reports_its_own_repository() {
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "one\n");
    fx.commit("c1");
    let worktree = fx.path().join("wt");
    fx.git(&[
        "worktree",
        "add",
        "-q",
        worktree.to_str().unwrap(),
        "-b",
        "feature",
    ]);
    assert!(
        worktree.join(".git").is_file(),
        ".git must be a file in a linked worktree"
    );

    let harness = Harness::new();
    let mut detector = harness.detector(&worktree, &[]);
    let claim = dir_claim(&mut detector, &worktree);

    assert_eq!(attr(&claim, "kind"), Some("linked-worktree"));
    assert_eq!(attr(&claim, "branch"), Some("feature"));

    // The main checkout still reports as a common repository.
    let mut main = harness.detector(fx.path(), &[]);
    assert_eq!(
        attr(&dir_claim(&mut main, fx.path()), "kind"),
        Some("common")
    );
}

#[test]
fn bare_repository_is_clean_with_bare_attribute_and_prunes() {
    let fx = Fixture::new();
    let bare = fx.path().join("bare.git");
    git_in(
        fx.path(),
        &["init", "--bare", "-q", "-b", "main", bare.to_str().unwrap()],
    );

    let author = fx.path().join("author");
    std::fs::create_dir_all(&author).unwrap();
    git_in(&author, &["init", "-q", "-b", "main"]);
    std::fs::write(author.join("tracked.txt"), "one\n").unwrap();
    git_in(&author, &["add", "-A"]);
    git_in(&author, &["commit", "-qm", "c1"]);
    git_in(
        &author,
        &["remote", "add", "origin", bare.to_str().unwrap()],
    );
    git_in(&author, &["push", "-q", "-u", "origin", "main"]);

    let harness = Harness::new();
    let mut detector = harness.detector(&bare, &[]);
    let claim = dir_claim(&mut detector, &bare);

    assert_eq!(claim.status, Status::Clean);
    assert_eq!(attr(&claim, "bare"), Some("true"));
    assert!(claim.prune, "bare repository contents are backup data");
}

#[test]
fn git_metadata_directory_is_claimed_and_pruned() {
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "one\n");
    fx.commit("c1");

    let harness = Harness::new();
    let mut detector = harness.detector(fx.path(), &[("report-ignored", "true")]);
    // The repository itself does not prune when ignored files are reported, so
    // the walker descends and reaches `.git`.
    assert!(!dir_claim(&mut detector, fx.path()).prune);

    let claim = dir_claim(&mut detector, &fx.path().join(".git"));
    assert_eq!(claim.status, Status::Clean);
    assert_eq!(attr(&claim, "kind"), Some("git-dir"));
    assert!(claim.prune);
}

// ---------------------------------------------------------------------------
// Ignored files
// ---------------------------------------------------------------------------

/// A repository with a matching ignored file, an ignored directory and an
/// untracked file next to an ignored one.
fn ignored_fixture() -> Fixture {
    let fx = Fixture::new();
    fx.init();
    fx.write(".gitignore", "*.log\nbuild/\n");
    fx.write("tracked.txt", "tracked\n");
    fx.commit("initial");
    fx.write("a.log", "ignored\n");
    fx.write("build/out1.o", "ignored\n");
    fx.write("build/out2.o", "ignored\n");
    fx.write("dir/keep.txt", "untracked\n");
    fx.write("dir/skip.log", "ignored\n");
    fx
}

#[test]
fn ignored_matching_reports_matching_entries() {
    let fx = ignored_fixture();
    let harness = Harness::new();
    let mut detector = harness.detector(fx.path(), &[("report-ignored", "true")]);
    // The repository is dirty (there is an untracked file) but the ignored
    // files themselves must not make it dirty.
    assert_eq!(dir_claim(&mut detector, fx.path()).status, Status::Dirty);

    assert_eq!(
        file_status(&mut detector, &fx.path().join("a.log")),
        Some(Status::Ignored)
    );
    assert_eq!(
        file_status(&mut detector, &fx.path().join("dir/skip.log")),
        Some(Status::Ignored)
    );
    // The ignored directory is one `ignored` entry and prunes.
    let build = dir_claim(&mut detector, &fx.path().join("build"));
    assert_eq!(build.status, Status::Ignored);
    assert!(build.prune);
    // A plain untracked file is dirty, not ignored.
    assert_eq!(
        file_status(&mut detector, &fx.path().join("dir/keep.txt")),
        Some(Status::Dirty)
    );
}

#[test]
fn ignored_all_reports_files_inside_ignored_directories() {
    let fx = ignored_fixture();
    let harness = Harness::new();
    let mut detector = harness.detector(
        fx.path(),
        &[("report-ignored", "true"), ("ignored-mode", "all")],
    );
    assert_eq!(dir_claim(&mut detector, fx.path()).status, Status::Dirty);

    // With `all`, ignored directories are not claimed and pruned: the walker
    // descends and the file hook reports their contents as ignored.
    assert!(
        detector
            .check_dir(&fx.path().join("build"))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        file_status(&mut detector, &fx.path().join("build/out1.o")),
        Some(Status::Ignored)
    );
    assert_eq!(
        file_status(&mut detector, &fx.path().join("build/out2.o")),
        Some(Status::Ignored)
    );
}

#[test]
fn report_ignored_off_keeps_the_repository_as_one_clean_entry() {
    let fx = ignored_fixture();
    let harness = Harness::new();
    let mut detector = harness.detector(fx.path(), &[]);
    let claim = dir_claim(&mut detector, fx.path());
    assert_eq!(claim.status, Status::Dirty);
    assert!(claim.prune, "without report-ignored the repository prunes");
}

// ---------------------------------------------------------------------------
// Nested and ancestor repositories
// ---------------------------------------------------------------------------

#[test]
fn nested_repository_is_reported_on_its_own() {
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "v1\n");
    fx.commit("initial");
    fx.write("tracked.txt", "v2\n"); // dirty parent

    let inner = fx.path().join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    git_in(&inner, &["init", "-q", "-b", "main"]);
    std::fs::write(inner.join("inner.txt"), "inner\n").unwrap();
    git_in(&inner, &["add", "-A"]);
    git_in(&inner, &["commit", "-qm", "inner c1"]);
    std::fs::write(inner.join("untracked.txt"), "untracked\n").unwrap();

    let harness = Harness::new();
    let mut detector = harness.detector(fx.path(), &[]);
    assert_eq!(dir_claim(&mut detector, fx.path()).status, Status::Dirty);
    assert_eq!(dir_claim(&mut detector, &inner).status, Status::Dirty);
    assert_eq!(
        file_status(&mut detector, &inner.join("untracked.txt")),
        Some(Status::Dirty)
    );
}

#[test]
fn crawl_root_inside_a_repository_is_claimed_by_ancestor_discovery() {
    let fx = Fixture::new();
    fx.init();
    fx.write("sub/dir/file.txt", "hello\n");
    fx.commit("initial");
    let remote = tempfile::tempdir().unwrap();
    with_upstream(&fx, remote.path());

    let subdir = fx.path().join("sub/dir");
    let harness = Harness::new();
    let mut detector = harness.detector(&subdir, &[("ancestor-repos", "true")]);
    let claim = dir_claim(&mut detector, &subdir);

    assert_eq!(claim.status, Status::Clean);
    assert_eq!(attr(&claim, "kind"), Some("common"));
    assert!(claim.prune, "the ambient claim covers the crawl root");
}

#[test]
fn ancestor_discovery_can_be_disabled() {
    let fx = Fixture::new();
    fx.init();
    fx.write("sub/dir/file.txt", "hello\n");
    fx.commit("initial");

    let subdir = fx.path().join("sub/dir");
    let harness = Harness::new();
    let mut detector = harness.detector(&subdir, &[("ancestor-repos", "false")]);
    assert!(
        detector.check_dir(&subdir).unwrap().is_none(),
        "ancestor discovery is off"
    );
    assert!(
        detector
            .check_file(&subdir.join("file.txt"))
            .unwrap()
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// Status edge cases
// ---------------------------------------------------------------------------

#[test]
fn staged_rename_is_dirty() {
    let fx = Fixture::new();
    fx.init();
    fx.write("old.txt", "content\n");
    fx.commit("initial");
    fx.git(&["mv", "old.txt", "new.txt"]);

    let harness = Harness::new();
    let mut detector = harness.detector(fx.path(), &[]);
    assert_eq!(dir_claim(&mut detector, fx.path()).status, Status::Dirty);
}

#[test]
fn merge_conflict_is_dirty() {
    let fx = Fixture::new();
    fx.init();
    fx.write("conflict.txt", "base\n");
    fx.commit("base");
    fx.git(&["checkout", "-q", "-b", "other"]);
    fx.write("conflict.txt", "other\n");
    fx.commit("other");
    fx.git(&["checkout", "-q", "main"]);
    fx.write("conflict.txt", "main\n");
    fx.commit("main");
    git_expect_failure(fx.path(), &["merge", "other"]);

    let harness = Harness::new();
    let mut detector = harness.detector(fx.path(), &[]);
    assert_eq!(dir_claim(&mut detector, fx.path()).status, Status::Dirty);
    assert_eq!(
        file_status(&mut detector, &fx.path().join("conflict.txt")),
        Some(Status::Dirty)
    );
}

#[test]
fn packed_objects_and_refs_are_read() {
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "one\n");
    fx.write("other.txt", "two\n");
    fx.commit("c1");
    fx.write("tracked.txt", "two\n");
    fx.commit("c2");
    let remote = tempfile::tempdir().unwrap();
    with_upstream(&fx, remote.path());
    fx.git(&["gc", "-q"]);
    fx.git(&["pack-refs", "--all"]);
    let packs = std::fs::read_dir(fx.path().join(".git/objects/pack"))
        .unwrap()
        .count();
    assert!(packs > 0, "fixture must contain a pack file");

    let harness = Harness::new();
    let mut detector = harness.detector(fx.path(), &[]);
    assert_eq!(dir_claim(&mut detector, fx.path()).status, Status::Clean);

    // A staged change needs the HEAD tree, i.e. object reads from the pack.
    fx.write("tracked.txt", "three\n");
    fx.git(&["add", "tracked.txt"]);
    // A fresh instance is a fresh scan: status is not persisted across scans.
    let mut detector = harness.detector(fx.path(), &[]);
    assert_eq!(dir_claim(&mut detector, fx.path()).status, Status::Dirty);
}

#[test]
fn non_repository_paths_are_unclaimed() {
    let fx = Fixture::new();
    fx.write("file.txt", "not a repo\n");

    let harness = Harness::new();
    let mut detector = harness.detector(fx.path(), &[]);
    assert!(detector.check_dir(fx.path()).unwrap().is_none());
    assert!(
        detector
            .check_file(&fx.path().join("file.txt"))
            .unwrap()
            .is_none()
    );
    assert_eq!(detector.report().errors, 0);
}

#[test]
fn submodule_worktree_is_reported_and_a_changed_submodule_is_dirty() {
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "one\n");
    fx.commit("initial");
    let (_library, submodule) = add_submodule(&fx);

    assert!(
        submodule.join(".git").is_file(),
        ".git must be a file in a submodule worktree"
    );

    let harness = Harness::new();
    let mut detector = harness.detector(fx.path(), &[]);
    // A clean submodule leaves the superproject with no upstream: `unknown`.
    assert_eq!(dir_claim(&mut detector, fx.path()).status, Status::Unknown);
    assert_eq!(
        attr(&dir_claim(&mut detector, &submodule), "kind"),
        Some("submodule")
    );

    // A changed submodule (new untracked file) makes the superproject dirty.
    std::fs::write(submodule.join("untracked.txt"), "new\n").unwrap();
    let mut detector = harness.detector(fx.path(), &[]);
    assert_eq!(dir_claim(&mut detector, fx.path()).status, Status::Dirty);
}

#[test]
fn submodule_worktrees_can_be_skipped() {
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "one\n");
    fx.commit("initial");
    let (_library, submodule) = add_submodule(&fx);

    let harness = Harness::new();
    let mut detector = harness.detector(fx.path(), &[("recurse-submodules", "false")]);
    assert!(
        detector.check_dir(&submodule).unwrap().is_none(),
        "submodule worktrees are skipped"
    );
}

/// Creates a local library repository outside the worktree and adds it as the
/// `vendor/lib` submodule of `fx`. Returns the library tempdir (keep alive) and
/// the submodule worktree path.
fn add_submodule(fx: &Fixture) -> (tempfile::TempDir, PathBuf) {
    let library_root = tempfile::tempdir().unwrap();
    let library = library_root.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    git_in(&library, &["init", "-q", "-b", "main"]);
    std::fs::write(library.join("lib.txt"), "lib\n").unwrap();
    git_in(&library, &["add", "-A"]);
    git_in(&library, &["commit", "-qm", "lib c1"]);

    fx.git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        "-q",
        library.to_str().unwrap(),
        "vendor/lib",
    ]);
    fx.commit("add submodule");
    (library_root, fx.path().join("vendor/lib"))
}
