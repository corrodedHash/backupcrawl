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
//! The fixtures and harness live in the shared `support` module (also used by
//! the M1.9 `exec` parity tests in `git_plugin_exec.rs`); they use a hermetic
//! `git` environment (no system/global config, fixed author/committer) exactly
//! like the spike.

mod support;

use backupcrawl_core::Status;

use support::{
    Fixture, Harness, add_submodule, attr, dir_claim, file_status, git_expect_failure, git_in,
    with_upstream,
};

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
