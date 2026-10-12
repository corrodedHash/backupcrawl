//! Shared helpers for the M1.7 fixture tests: git CLI fixtures, component
//! invocation and a `git status --porcelain=v2` reference parser.

#![allow(dead_code)] // not every test binary uses every helper

use gix_feasibility_runner::{default_component_path, SpikeHost};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use tempfile::TempDir;

/// One shared host (engine + compiled component) for all tests in a binary.
pub fn host() -> &'static SpikeHost {
    static HOST: OnceLock<SpikeHost> = OnceLock::new();
    HOST.get_or_init(|| {
        let path = default_component_path().expect("component path resolution failed");
        SpikeHost::new(&path).unwrap_or_else(|err| {
            panic!(
                "failed to load component {}: {err}\n\
                 build it first with `cargo component build --target wasm32-wasip2` (see build.sh)",
                path.display()
            )
        })
    })
}

/// Call the component on `root` and parse its JSON summary.
pub fn run_json(root: &Path) -> Value {
    let call = host()
        .run_cold(root.to_str().expect("non-utf8 path"))
        .expect("component call failed");
    serde_json::from_str(&call.json).expect("component returned invalid JSON")
}

/// A git fixture in a temporary directory that is kept alive as long as the struct.
pub struct Fixture {
    pub dir: TempDir,
}

impl Fixture {
    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("failed to create tempdir"),
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Write a file relative to the fixture root, creating parent directories.
    pub fn write(&self, rel: &str, contents: &str) {
        let path = self.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("failed to create fixture directory");
        }
        std::fs::write(&path, contents).expect("failed to write fixture file");
    }

    pub fn git(&self, args: &[&str]) -> String {
        git_in(self.path(), args)
    }

    /// `git init` with `main` as the initial branch and isolated config.
    pub fn init(&self) {
        self.git(&["init", "-q", "-b", "main"]);
    }

    /// Stage everything and commit it.
    pub fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-qm", message]);
    }
}

/// Run `git` in `cwd` with a hermetic environment (no system/global config,
/// fixed author/committer) and return stdout. Panics on failure.
pub fn git_in(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
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
        .expect("failed to spawn git");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed:\n{}",
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Run `git` in `cwd` expecting failure (e.g. a merge that conflicts) and
/// return stdout. Panics if the command unexpectedly succeeds.
pub fn git_expect_failure(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
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
        .expect("failed to spawn git");
    assert!(
        !output.status.success(),
        "git {args:?} in {} unexpectedly succeeded",
        cwd.display()
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The reference `git status --porcelain=v2 --branch --ignored=matching` counts.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Porcelain {
    pub branch_oid: Option<String>,
    pub branch_head: Option<String>,
    pub branch_upstream: Option<String>,
    pub ahead: i64,
    pub behind: i64,
    pub staged: u64,
    pub unstaged: u64,
    pub renamed: u64,
    pub conflicted: u64,
    pub untracked: u64,
    pub ignored: u64,
}

impl Porcelain {
    pub fn dirty(&self) -> bool {
        self.staged + self.unstaged + self.renamed + self.conflicted + self.untracked > 0
    }
}

/// Parse the porcelain v2 reference output. Must be called on a repository with
/// a worktree (bare repositories have no `status`).
pub fn porcelain(root: &Path) -> Porcelain {
    let out = git_in(
        root,
        &["status", "--porcelain=v2", "--branch", "--ignored=matching"],
    );
    let mut p = Porcelain::default();
    for line in out.lines() {
        if let Some(value) = line.strip_prefix("# branch.oid ") {
            p.branch_oid = (value != "(initial)").then(|| value.to_owned());
        } else if let Some(value) = line.strip_prefix("# branch.head ") {
            p.branch_head = (value != "(detached)").then(|| value.to_owned());
        } else if let Some(value) = line.strip_prefix("# branch.upstream ") {
            p.branch_upstream = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("# branch.ab ") {
            // "+A -B"
            let mut parts = value.split_whitespace();
            p.ahead = parts
                .next()
                .and_then(|v| v.strip_prefix('+'))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            p.behind = parts
                .next()
                .and_then(|v| v.strip_prefix('-'))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("1 ") {
            let xy = &rest[..2];
            if &xy[0..1] != "." {
                p.staged += 1;
            }
            if &xy[1..2] != "." {
                p.unstaged += 1;
            }
        } else if line.starts_with("2 ") {
            p.renamed += 1;
        } else if line.starts_with("u ") {
            p.conflicted += 1;
        } else if line.starts_with("? ") {
            p.untracked += 1;
        } else if line.starts_with("! ") {
            p.ignored += 1;
        }
    }
    p
}

/// Compare the component summary with the porcelain v2 reference for the
/// fields whose semantics are expected to be identical. Count mismatches
/// caused by different collapsing modes are asserted separately in the tests.
pub fn assert_porcelain_parity(root: &Path, json: &Value) {
    let p = porcelain(root);
    assert_eq!(json["error"], Value::Null, "unexpected error: {json}");
    assert_eq!(
        json["status"]["staged"].as_u64().unwrap(),
        p.staged,
        "staged mismatch: {json}"
    );
    assert_eq!(
        json["status"]["unstaged"].as_u64().unwrap(),
        p.unstaged,
        "unstaged mismatch: {json}"
    );
    assert_eq!(
        json["status"]["renamed"].as_u64().unwrap(),
        p.renamed,
        "renamed mismatch: {json}"
    );
    assert_eq!(
        json["status"]["conflicted"].as_u64().unwrap(),
        p.conflicted,
        "conflicted mismatch: {json}"
    );
    assert_eq!(
        json["dirty"].as_bool().unwrap(),
        p.dirty(),
        "dirty mismatch: {json}"
    );
    assert_eq!(
        json["head"]["oid"].as_str().map(str::to_owned),
        p.branch_oid,
        "head oid mismatch: {json}"
    );
    assert_eq!(
        json["head"]["name"].as_str().map(str::to_owned),
        p.branch_head,
        "head name mismatch: {json}"
    );
}

/// Path of the wasm component used by the runner (for diagnostics in tests).
pub fn component_path() -> PathBuf {
    default_component_path().expect("component path resolution failed")
}
