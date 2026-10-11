//! Fixture tests for the M1.7 gix component: create repositories with the git
//! CLI and assert the component's JSON summary, comparing against the
//! porcelain v2 reference of `docs/rust-rewrite-plan.md` §7.1.

mod common;

use common::{Fixture, assert_porcelain_parity, git_in, porcelain, run_json};
use serde_json::Value;
use std::path::Path;

/// The component must report paths as seen by the guest; the preopen is `/`,
/// so host and guest paths are identical up to symlink resolution.
fn assert_same_path(actual: &str, expected: &Path) {
    let actual = std::fs::canonicalize(actual).expect("component path must exist");
    let expected = std::fs::canonicalize(expected).expect("fixture path must exist");
    assert_eq!(actual, expected);
}

#[test]
fn clean_repo() {
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "hello\n");
    fx.write("src/main.rs", "fn main() {}\n");
    fx.commit("initial");

    let json = run_json(fx.path());
    assert_porcelain_parity(fx.path(), &json);
    assert_eq!(json["kind"], "common");
    assert_eq!(json["bare"], false);
    assert_eq!(json["head"]["kind"], "branch");
    assert_eq!(json["head"]["name"], "main");
    assert_eq!(json["head"]["oid"].as_str().unwrap().len(), 40);
    assert_eq!(json["upstream"]["name"], Value::Null);
    assert_eq!(json["upstream"]["ahead"], Value::Null);
    assert_eq!(json["upstream"]["behind"], Value::Null);
    assert_eq!(json["status"]["untracked"], 0);
    assert_eq!(json["status"]["ignored"], 0);
    assert_eq!(json["dirty"], false);
    assert_same_path(json["repo"].as_str().unwrap(), fx.path());
}

#[test]
fn dirty_modified_and_untracked() {
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "hello\n");
    fx.commit("initial");
    fx.write("tracked.txt", "changed\n");
    fx.write("new-file.txt", "new\n");

    let json = run_json(fx.path());
    assert_porcelain_parity(fx.path(), &json);
    assert_eq!(json["status"]["unstaged"], 1);
    assert_eq!(json["status"]["staged"], 0);
    assert_eq!(json["status"]["untracked"], 1);
    assert_eq!(json["dirty"], true);
}

#[test]
fn ahead_of_upstream() {
    let fx = Fixture::new();
    let remote = fx.path().join("remote.git");
    git_in(fx.path(), &["init", "--bare", "-q", "-b", "main", remote.to_str().unwrap()]);
    fx.init();
    fx.write("tracked.txt", "one\n");
    fx.commit("c1");
    fx.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    fx.git(&["push", "-q", "-u", "origin", "main"]);
    fx.write("tracked.txt", "two\n");
    fx.commit("c2");

    let json = run_json(fx.path());
    assert_porcelain_parity(fx.path(), &json);
    assert_eq!(json["upstream"]["name"], "origin/main");
    assert_eq!(json["upstream"]["ahead"], 1);
    assert_eq!(json["upstream"]["behind"], 0);
    assert_eq!(json["dirty"], false);
}

#[test]
fn behind_upstream() {
    let fx = Fixture::new();
    let upstream = fx.path().join("upstream.git");
    git_in(fx.path(), &["init", "--bare", "-q", "-b", "main", upstream.to_str().unwrap()]);

    let author = fx.path().join("author");
    std::fs::create_dir_all(&author).unwrap();
    git_in(&author, &["init", "-q", "-b", "main"]);
    std::fs::write(author.join("tracked.txt"), "one\n").unwrap();
    git_in(&author, &["add", "-A"]);
    git_in(&author, &["commit", "-qm", "c1"]);
    git_in(&author, &["remote", "add", "origin", upstream.to_str().unwrap()]);
    git_in(&author, &["push", "-q", "-u", "origin", "main"]);

    git_in(fx.path(), &["clone", "-q", upstream.to_str().unwrap(), "clone"]);

    // Move the upstream ahead of the clone: the author commits and pushes, the
    // clone fetches so its remote-tracking ref advances while its local branch
    // stays behind.
    std::fs::write(author.join("tracked.txt"), "two\n").unwrap();
    git_in(&author, &["add", "-A"]);
    git_in(&author, &["commit", "-qm", "c2"]);
    git_in(&author, &["push", "-q", "origin", "main"]);

    let clone = fx.path().join("clone");
    git_in(&clone, &["fetch", "-q", "origin"]);
    let json = run_json(&clone);
    assert_porcelain_parity(&clone, &json);
    assert_eq!(json["upstream"]["name"], "origin/main");
    assert_eq!(json["upstream"]["ahead"], 0);
    assert_eq!(json["upstream"]["behind"], 1);
    assert_eq!(json["dirty"], false);
}

#[test]
fn no_upstream() {
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "one\n");
    fx.commit("c1");

    let json = run_json(fx.path());
    assert_porcelain_parity(fx.path(), &json);
    assert_eq!(json["head"]["kind"], "branch");
    assert_eq!(json["upstream"]["name"], Value::Null);
    assert_eq!(json["upstream"]["ahead"], Value::Null);
    assert_eq!(json["upstream"]["behind"], Value::Null);
}

#[test]
fn detached_head() {
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "one\n");
    fx.commit("c1");
    fx.git(&["checkout", "-q", "--detach", "HEAD"]);

    let json = run_json(fx.path());
    assert_porcelain_parity(fx.path(), &json);
    assert_eq!(json["head"]["kind"], "detached");
    assert_eq!(json["head"]["name"], Value::Null);
    assert_eq!(json["head"]["oid"].as_str().unwrap().len(), 40);
    assert_eq!(json["upstream"]["name"], Value::Null);
}

#[test]
fn unborn_branch() {
    let fx = Fixture::new();
    fx.init();

    let json = run_json(fx.path());
    assert_porcelain_parity(fx.path(), &json);
    assert_eq!(json["head"]["kind"], "unborn");
    assert_eq!(json["head"]["name"], "main");
    assert_eq!(json["head"]["oid"], Value::Null);
    assert_eq!(json["dirty"], false);

    // An unborn branch with an untracked file is dirty but has no upstream.
    fx.write("new-file.txt", "new\n");
    let json = run_json(fx.path());
    assert_porcelain_parity(fx.path(), &json);
    assert_eq!(json["status"]["untracked"], 1);
    assert_eq!(json["dirty"], true);
    assert_eq!(json["upstream"]["name"], Value::Null);
}

#[test]
fn linked_worktree() {
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "one\n");
    fx.commit("c1");
    let worktree = fx.path().join("wt");
    fx.git(&["worktree", "add", "-q", worktree.to_str().unwrap(), "-b", "feature"]);
    assert!(worktree.join(".git").is_file(), ".git must be a file in a linked worktree");

    let json = run_json(&worktree);
    assert_porcelain_parity(&worktree, &json);
    assert_eq!(json["kind"], "linked-worktree");
    assert_eq!(json["head"]["kind"], "branch");
    assert_eq!(json["head"]["name"], "feature");
    assert_same_path(json["repo"].as_str().unwrap(), &worktree);

    // The main checkout still reports as a common repository.
    let main = run_json(fx.path());
    assert_eq!(main["kind"], "common");
    assert_eq!(main["head"]["name"], "main");
}

#[test]
fn bare_repo() {
    let fx = Fixture::new();
    let bare = fx.path().join("bare.git");
    git_in(fx.path(), &["init", "--bare", "-q", "-b", "main", bare.to_str().unwrap()]);

    let author = fx.path().join("author");
    std::fs::create_dir_all(&author).unwrap();
    git_in(&author, &["init", "-q", "-b", "main"]);
    std::fs::write(author.join("tracked.txt"), "one\n").unwrap();
    git_in(&author, &["add", "-A"]);
    git_in(&author, &["commit", "-qm", "c1"]);
    git_in(&author, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git_in(&author, &["push", "-q", "-u", "origin", "main"]);

    let json = run_json(&bare);
    // No porcelain parity: `git status` does not work in a bare repository.
    assert_eq!(json["error"], Value::Null);
    assert_eq!(json["bare"], true);
    assert_eq!(json["repo"], Value::Null);
    assert_eq!(json["head"]["kind"], "branch");
    assert_eq!(json["head"]["name"], "main");
    assert_eq!(json["head"]["oid"].as_str().unwrap().len(), 40);
    assert_eq!(json["status"]["staged"], 0);
    assert_eq!(json["status"]["unstaged"], 0);
    assert_eq!(json["status"]["untracked"], 0);
    assert_eq!(json["status"]["ignored"], 0);
    assert_eq!(json["dirty"], false);
}

#[test]
fn ignored_files_matching() {
    let fx = Fixture::new();
    fx.init();
    fx.write(".gitignore", "*.log\nbuild/\n");
    fx.write("tracked.txt", "tracked\n");
    fx.commit("initial");
    // Ignored files and directories plus one untracked file in a directory that
    // also contains an ignored file.
    fx.write("a.log", "ignored\n");
    fx.write("build/out1.o", "ignored\n");
    fx.write("build/out2.o", "ignored\n");
    fx.write("dir/keep.txt", "untracked\n");
    fx.write("dir/skip.log", "ignored\n");

    let json = run_json(fx.path());
    let reference = porcelain(fx.path());
    // Reference records: `! a.log`, `! build/`, `! dir/skip.log`, `? dir/keep.txt`.
    assert_eq!(reference.ignored, 3);
    assert_eq!(reference.untracked, 1);
    assert_eq!(json["status"]["untracked"], 1, "untracked files are not collapsed: {json}");
    assert_eq!(
        json["status"]["ignored"], reference.ignored,
        "ignored entries must match `--ignored=matching` collapsing: {json}"
    );
    assert_eq!(json["dirty"], true, "ignored files alone must not be dirty");
    assert_porcelain_parity(fx.path(), &json);
}

#[test]
fn nested_repository_in_dirty_parent() {
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

    // The nested repository is reported on its own, not as part of the parent.
    let inner_json = run_json(&inner);
    assert_porcelain_parity(&inner, &inner_json);
    assert_same_path(inner_json["repo"].as_str().unwrap(), &inner);
    assert_eq!(inner_json["status"]["untracked"], 1);
    assert_eq!(inner_json["dirty"], true);

    // From the parent, the nested repository is a single untracked entry; the
    // dirwalk must not recurse into it.
    let outer_json = run_json(fx.path());
    assert_porcelain_parity(fx.path(), &outer_json);
    assert_eq!(outer_json["status"]["unstaged"], 1);
    assert_eq!(outer_json["status"]["untracked"], 1, "nested repo as one entry: {outer_json}");
    assert_same_path(outer_json["repo"].as_str().unwrap(), fx.path());
}

#[test]
fn subdirectory_discovers_ancestor() {
    let fx = Fixture::new();
    fx.init();
    fx.write("sub/dir/file.txt", "hello\n");
    fx.commit("initial");

    let json = run_json(&fx.path().join("sub/dir"));
    assert_eq!(json["error"], Value::Null);
    assert_same_path(json["repo"].as_str().unwrap(), fx.path());
    assert_eq!(json["head"]["name"], "main");
    assert_eq!(json["dirty"], false);
}

#[test]
fn staged_rename() {
    let fx = Fixture::new();
    fx.init();
    fx.write("old.txt", "content\n");
    fx.commit("initial");
    fx.git(&["mv", "old.txt", "new.txt"]);

    let json = run_json(fx.path());
    assert_porcelain_parity(fx.path(), &json);
    assert_eq!(json["status"]["renamed"], 1, "staged rename as `2` record: {json}");
    assert_eq!(json["status"]["staged"], 0);
    assert_eq!(json["status"]["unstaged"], 0);
    assert_eq!(json["dirty"], true);
}

#[test]
fn packed_repository() {
    // Real repositories have packed objects and packed refs; gix reaches them
    // through gix-pack/gix-ref, which is the path most affected by the wasm
    // mmap situation.
    let fx = Fixture::new();
    fx.init();
    fx.write("tracked.txt", "one\n");
    fx.write("other.txt", "two\n");
    fx.commit("c1");
    fx.write("tracked.txt", "two\n");
    fx.commit("c2");
    fx.git(&["gc", "-q"]);
    fx.git(&["pack-refs", "--all"]);
    let packs = std::fs::read_dir(fx.path().join(".git/objects/pack"))
        .unwrap()
        .count();
    assert!(packs > 0, "fixture must contain a pack file");

    let json = run_json(fx.path());
    assert_porcelain_parity(fx.path(), &json);
    assert_eq!(json["dirty"], false);

    // A staged change needs the HEAD tree, i.e. object reads from the pack.
    fx.write("tracked.txt", "three\n");
    fx.git(&["add", "tracked.txt"]);
    let json = run_json(fx.path());
    assert_porcelain_parity(fx.path(), &json);
    assert_eq!(json["status"]["staged"], 1);
    assert_eq!(json["dirty"], true);
}

#[test]
fn merge_conflict() {
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
    common::git_expect_failure(fx.path(), &["merge", "other"]);

    let json = run_json(fx.path());
    assert_eq!(json["status"]["conflicted"], 1, "one `u` record per conflicted path: {json}");
    assert_eq!(json["dirty"], true);
}

#[test]
fn not_a_repository() {
    let fx = Fixture::new();
    fx.write("file.txt", "not a repo\n");

    let json = run_json(fx.path());
    let error = json["error"].as_str().expect("error must be reported");
    assert!(error.contains("not a git repository"), "unexpected error: {error}");
    assert_eq!(json["repo"], Value::Null);
    assert_eq!(json["head"]["kind"], Value::Null);
    assert_eq!(json["dirty"], false);
}
