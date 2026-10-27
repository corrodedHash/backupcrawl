//! M1.9 parity tests: the `exec` backend must produce the same claims as the
//! default `gix` backend on the same fixtures.
//!
//! Every test builds a fixture once (mirroring `git_plugin.rs`), then observes
//! it with both backends and asserts the observations are identical: the root
//! directory claim (status, attributes, prune), any additional directory probes
//! and the per-file hook statuses. Because `git_plugin.rs` already pins the
//! `gix` output against the documented porcelain-v2 semantics, equality here
//! means the `exec` backend reproduces them too.
//!
//! The fixtures are created with the `git` CLI, so this file requires `git` on
//! `PATH`; each test returns early (as M1.9's acceptance allows) when the probe
//! fails.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use backupcrawl_core::Status;
use backupcrawl_host::{Claim, WasmDetector};

use support::{Fixture, Harness, add_submodule, git_expect_failure, git_in, with_upstream};

/// Whether the `git` program the fixtures and the `exec` backend both need is
/// available. Tests return early when it is not.
fn require_git() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

// ---------------------------------------------------------------------------
// Observation model
// ---------------------------------------------------------------------------

/// A directory probe's outcome, including a full claim when one was made.
#[derive(Debug, PartialEq, Eq)]
enum DirOutcome {
    /// No claim.
    Unclaimed,
    /// A claim: status, sorted attributes and prune flag.
    Claim {
        status: Status,
        attrs: Vec<(String, String)>,
        prune: bool,
    },
}

/// One backend's answers for a fixed set of probes.
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    dirs: Vec<(PathBuf, DirOutcome)>,
    files: Vec<(PathBuf, Option<Status>)>,
}

fn dir_outcome(detector: &mut WasmDetector, path: &Path) -> DirOutcome {
    match detector
        .check_dir(path)
        .unwrap_or_else(|err| panic!("check_dir {}: {err}", path.display()))
    {
        None => DirOutcome::Unclaimed,
        Some(claim) => DirOutcome::Claim {
            status: claim.status,
            attrs: sorted_attrs(&claim),
            prune: claim.prune,
        },
    }
}

fn sorted_attrs(claim: &Claim) -> Vec<(String, String)> {
    let mut attrs: Vec<(String, String)> = claim
        .attrs
        .iter()
        .map(|attr| (attr.key.clone(), attr.value.clone()))
        .collect();
    attrs.sort();
    attrs
}

/// Observes `root` with `backend`. `dir_probes`/`file_probes` are relative to
/// `root`; `.` means the root itself.
fn snapshot(
    harness: &Harness,
    root: &Path,
    backend: &str,
    config: &[(&str, &str)],
    dir_probes: &[&str],
    file_probes: &[&str],
) -> Snapshot {
    let mut effective: Vec<(&str, &str)> = vec![("backend", backend)];
    effective.extend_from_slice(config);
    let mut detector = harness.detector(root, &effective);

    let dirs = dir_probes
        .iter()
        .map(|rel| {
            let path = probe_path(root, rel);
            let outcome = dir_outcome(&mut detector, &path);
            (path, outcome)
        })
        .collect();
    let files = file_probes
        .iter()
        .map(|rel| {
            let path = probe_path(root, rel);
            let status = detector
                .check_file(&path)
                .unwrap_or_else(|err| panic!("check_file {}: {err}", path.display()))
                .map(|claim| claim.status);
            (path, status)
        })
        .collect();

    Snapshot { dirs, files }
}

fn probe_path(root: &Path, rel: &str) -> PathBuf {
    if rel == "." {
        root.to_path_buf()
    } else {
        root.join(rel)
    }
}

/// Asserts the `gix` and `exec` backends agree on `root` over the given probes.
fn assert_parity(
    harness: &Harness,
    root: &Path,
    config: &[(&str, &str)],
    dir_probes: &[&str],
    file_probes: &[&str],
) {
    let gix = snapshot(harness, root, "gix", config, dir_probes, file_probes);
    let exec = snapshot(harness, root, "exec", config, dir_probes, file_probes);
    assert_eq!(
        gix,
        exec,
        "gix and exec disagree on {} (config {config:?})",
        root.display()
    );
}

// ---------------------------------------------------------------------------
// Parity cases (one per M1.8 acceptance case)
// ---------------------------------------------------------------------------

#[test]
fn clean_repository_with_upstream() {
    if !require_git() {
        return;
    }
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "hello\n");
    fx.write("src/main.rs", "fn main() {}\n");
    fx.commit("initial");
    let remote = tempfile::tempdir().unwrap();
    with_upstream(&fx, remote.path());

    let harness = Harness::new();
    assert_parity(
        &harness,
        fx.path(),
        &[],
        &["."],
        &["tracked.txt", "src/main.rs", "missing.txt"],
    );
}

#[test]
fn dirty_modified_and_untracked() {
    if !require_git() {
        return;
    }
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "hello\n");
    fx.commit("initial");
    fx.write("tracked.txt", "changed\n");
    fx.write("new-file.txt", "new\n");
    fx.write("dir/untracked.txt", "nested\n");

    let harness = Harness::new();
    assert_parity(
        &harness,
        fx.path(),
        &[],
        &["."],
        &["tracked.txt", "new-file.txt", "dir/untracked.txt"],
    );
}

#[test]
fn ahead_and_behind() {
    if !require_git() {
        return;
    }
    let harness = Harness::new();

    // Clean, then ahead by one.
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "one\n");
    fx.commit("c1");
    let remote = tempfile::tempdir().unwrap();
    with_upstream(&fx, remote.path());
    assert_parity(&harness, fx.path(), &[], &["."], &["tracked.txt"]);
    fx.write("tracked.txt", "two\n");
    fx.commit("c2");
    assert_parity(&harness, fx.path(), &[], &["."], &[]);

    // Behind by one: an author advances the shared upstream and the clone
    // fetches. (The `git_plugin.rs` golden test builds the same shape.)
    let behind = Fixture::new();
    let upstream = behind.path().join("upstream.git");
    git_in(
        behind.path(),
        &[
            "init",
            "--bare",
            "-q",
            "-b",
            "main",
            upstream.to_str().unwrap(),
        ],
    );
    let author = behind.path().join("author");
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
        behind.path(),
        &["clone", "-q", upstream.to_str().unwrap(), "clone"],
    );
    std::fs::write(author.join("tracked.txt"), "two\n").unwrap();
    git_in(&author, &["add", "-A"]);
    git_in(&author, &["commit", "-qm", "c2"]);
    git_in(&author, &["push", "-q", "origin", "main"]);
    let clone = behind.path().join("clone");
    git_in(&clone, &["fetch", "-q", "origin"]);
    assert_parity(&harness, &clone, &[], &["."], &["tracked.txt"]);
}

#[test]
fn no_upstream_policies() {
    if !require_git() {
        return;
    }
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "one\n");
    fx.commit("c1");

    let harness = Harness::new();
    for config in [
        &[][..],
        &[("no-upstream", "clean")][..],
        &[("no-upstream", "ahead")][..],
    ] {
        assert_parity(&harness, fx.path(), config, &["."], &["tracked.txt"]);
    }
}

#[test]
fn detached_and_unborn() {
    if !require_git() {
        return;
    }
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "one\n");
    fx.commit("c1");
    let harness = Harness::new();
    assert_parity(&harness, fx.path(), &[], &["."], &[]);

    fx.git(&["checkout", "-q", "--detach", "HEAD"]);
    assert_parity(&harness, fx.path(), &[], &["."], &[]);

    // Dirty detached HEAD: dirty wins on both backends.
    fx.write("tracked.txt", "two\n");
    assert_parity(&harness, fx.path(), &[], &["."], &["tracked.txt"]);

    let unborn = Fixture::new();
    unborn.init();
    assert_parity(&harness, unborn.path(), &[], &["."], &[]);
}

#[test]
fn linked_worktree() {
    if !require_git() {
        return;
    }
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

    let harness = Harness::new();
    assert_parity(&harness, &worktree, &[], &["."], &["tracked.txt"]);
    assert_parity(&harness, fx.path(), &[], &["."], &["tracked.txt"]);
}

#[test]
fn bare_repository() {
    if !require_git() {
        return;
    }
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
    assert_parity(&harness, &bare, &[], &["."], &[]);
}

#[test]
fn ignored_matching_and_all() {
    if !require_git() {
        return;
    }
    let fx = ignored_fixture();

    let harness = Harness::new();
    assert_parity(
        &harness,
        fx.path(),
        &[("report-ignored", "true")],
        &[".", "build"],
        &["a.log", "dir/skip.log", "dir/keep.txt"],
    );
    assert_parity(
        &harness,
        fx.path(),
        &[("report-ignored", "true"), ("ignored-mode", "all")],
        &[".", "build"],
        &["build/out1.o", "build/out2.o", "a.log"],
    );
    assert_parity(&harness, fx.path(), &[], &["."], &["a.log", "dir/skip.log"]);
}

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
fn nested_repository() {
    if !require_git() {
        return;
    }
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "v1\n");
    fx.commit("initial");
    fx.write("tracked.txt", "v2\n");

    let inner = fx.path().join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    git_in(&inner, &["init", "-q", "-b", "main"]);
    std::fs::write(inner.join("inner.txt"), "inner\n").unwrap();
    git_in(&inner, &["add", "-A"]);
    git_in(&inner, &["commit", "-qm", "inner c1"]);
    std::fs::write(inner.join("untracked.txt"), "untracked\n").unwrap();

    let harness = Harness::new();
    assert_parity(&harness, fx.path(), &[], &["."], &["tracked.txt"]);
    assert_parity(
        &harness,
        &inner,
        &[],
        &["."],
        &["inner.txt", "untracked.txt"],
    );
}

#[test]
fn ancestor_repository() {
    if !require_git() {
        return;
    }
    let fx = Fixture::new();
    fx.init();
    fx.write("sub/dir/file.txt", "hello\n");
    fx.commit("initial");
    let remote = tempfile::tempdir().unwrap();
    with_upstream(&fx, remote.path());

    let subdir = fx.path().join("sub/dir");
    let harness = Harness::new();
    assert_parity(
        &harness,
        &subdir,
        &[("ancestor-repos", "true")],
        &["."],
        &["file.txt"],
    );
    assert_parity(
        &harness,
        &subdir,
        &[("ancestor-repos", "false")],
        &["."],
        &["file.txt"],
    );
}

#[test]
fn rename_conflict_and_packed_objects() {
    if !require_git() {
        return;
    }
    let harness = Harness::new();

    let renamed = Fixture::new();
    renamed.init();
    renamed.write("old.txt", "content\n");
    renamed.commit("initial");
    renamed.git(&["mv", "old.txt", "new.txt"]);
    assert_parity(
        &harness,
        renamed.path(),
        &[],
        &["."],
        &["new.txt", "old.txt"],
    );

    let conflict = Fixture::new();
    conflict.init();
    conflict.write("conflict.txt", "base\n");
    conflict.commit("base");
    conflict.git(&["checkout", "-q", "-b", "other"]);
    conflict.write("conflict.txt", "other\n");
    conflict.commit("other");
    conflict.git(&["checkout", "-q", "main"]);
    conflict.write("conflict.txt", "main\n");
    conflict.commit("main");
    git_expect_failure(conflict.path(), &["merge", "other"]);
    assert_parity(&harness, conflict.path(), &[], &["."], &["conflict.txt"]);

    let packed = Fixture::new();
    packed.init();
    packed.write("tracked.txt", "one\n");
    packed.write("other.txt", "two\n");
    packed.commit("c1");
    packed.write("tracked.txt", "two\n");
    packed.commit("c2");
    let remote = tempfile::tempdir().unwrap();
    with_upstream(&packed, remote.path());
    packed.git(&["gc", "-q"]);
    packed.git(&["pack-refs", "--all"]);
    assert_parity(&harness, packed.path(), &[], &["."], &["tracked.txt"]);
}

#[test]
fn submodule_worktree() {
    if !require_git() {
        return;
    }
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "one\n");
    fx.commit("initial");
    let (_library, submodule) = add_submodule(&fx);

    let harness = Harness::new();
    assert_parity(&harness, fx.path(), &[], &["."], &["tracked.txt"]);

    // A clean submodule leaves the superproject without an upstream.
    assert_parity(&harness, &submodule, &[], &["."], &["lib.txt"]);
    assert_parity(
        &harness,
        &submodule,
        &[("recurse-submodules", "false")],
        &["."],
        &["lib.txt"],
    );

    // A changed submodule makes the superproject dirty on both backends.
    std::fs::write(submodule.join("untracked.txt"), "new\n").unwrap();
    assert_parity(&harness, fx.path(), &[], &["."], &["tracked.txt"]);
}

#[test]
fn non_repository_paths() {
    if !require_git() {
        return;
    }
    let fx = Fixture::new();
    fx.write("file.txt", "not a repo\n");
    fx.write("dir/nested.txt", "still not a repo\n");

    let harness = Harness::new();
    assert_parity(
        &harness,
        fx.path(),
        &[],
        &[".", "dir"],
        &["file.txt", "dir/nested.txt"],
    );
}

#[test]
fn incomplete_git_directory_is_unclaimed() {
    if !require_git() {
        return;
    }
    let fx = Fixture::new();
    std::fs::create_dir_all(fx.path().join(".git")).unwrap();

    let harness = Harness::new();
    assert_parity(&harness, fx.path(), &[], &["."], &[]);
}
