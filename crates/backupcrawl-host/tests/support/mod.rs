//! Shared harness and git-CLI fixtures for the bundled `git` detector tests.
//!
//! Used by both `git_plugin.rs` (the `gix` golden tests, M1.8) and
//! `git_plugin_exec.rs` (the `exec` parity tests, M1.9). Fixtures are created
//! with the `git` CLI under a hermetic environment (no system/global config,
//! fixed author/committer) exactly like the M1.7 spike.

#![allow(dead_code)] // not every test binary uses every helper

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use backupcrawl_core::Status;
use backupcrawl_host::manifest::Manifest;
use backupcrawl_host::{CapabilityPolicy, Claim, Limits, Runtime, ScanInfo, WasmDetector, bundled};

/// Name of the bundled detector under test.
pub const PLUGIN: &str = "git";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// The runtime shared by every test in a binary.
///
/// Compiling the bundled git component takes seconds in a debug build, so all
/// tests reuse one [`Runtime`] — and therefore, via the runtime's component
/// cache, one compiled component. The cache root is the same shared path the
/// CLI tests use (`backupcrawl-test-cache/backupcrawl/plugins`), so the entire
/// workspace shares one wasmtime component cache. The git detector declares no
/// `cache` capability, so the shared plugin KV directory is never written here.
pub fn shared() -> &'static Runtime {
    static SHARED: OnceLock<Runtime> = OnceLock::new();
    SHARED.get_or_init(|| {
        let cache = std::env::temp_dir()
            .join("backupcrawl-test-cache")
            .join("backupcrawl")
            .join("plugins");
        Runtime::with_cache_root(Limits::default(), cache).expect("runtime")
    })
}

/// Instantiates detectors from the shared runtime.
pub struct Harness;

impl Harness {
    pub fn new() -> Self {
        // Compile the component once, before the test body runs.
        shared();
        Self
    }

    /// Instantiates and initializes the bundled git detector with `config`.
    pub fn detector(&self, root: &Path, config: &[(&str, &str)]) -> WasmDetector {
        let plugin = bundled::all()
            .iter()
            .find(|plugin| plugin.name == PLUGIN)
            .expect("git must be in the bundled registry");
        let manifest = Manifest::parse(plugin.manifest).expect("bundled git manifest");
        let policy = CapabilityPolicy::from_manifest(&manifest);
        let mut detector = shared()
            .instantiate(plugin.wasm, PLUGIN, policy)
            .expect("instantiate git component");
        detector
            .init(&scan_info(root, config))
            .unwrap_or_else(|err| panic!("init git detector: {err}"));
        detector
    }
}

pub fn scan_info(root: &Path, config: &[(&str, &str)]) -> ScanInfo {
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
pub fn dir_claim(detector: &mut WasmDetector, path: &Path) -> Claim {
    detector
        .check_dir(path)
        .unwrap_or_else(|err| panic!("check_dir {}: {err}", path.display()))
        .unwrap_or_else(|| panic!("expected a claim for {}", path.display()))
}

/// Looks up one claim attribute.
pub fn attr<'a>(claim: &'a Claim, key: &str) -> Option<&'a str> {
    claim
        .attrs
        .iter()
        .find(|attr| attr.key == key)
        .map(|attr| attr.value.as_str())
}

/// The status of a file claim, or `None` when unclaimed.
pub fn file_status(detector: &mut WasmDetector, path: &Path) -> Option<Status> {
    detector
        .check_file(path)
        .unwrap_or_else(|err| panic!("check_file {}: {err}", path.display()))
        .map(|claim| claim.status)
}

// ---------------------------------------------------------------------------
// Fixtures (ported from the M1.7 spike)
// ---------------------------------------------------------------------------

/// A git fixture in a temporary directory, kept alive by the struct.
pub struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("fixture tempdir"),
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Writes a file relative to the fixture root, creating parents.
    pub fn write(&self, rel: &str, contents: &str) {
        let path = self.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture directory");
        }
        std::fs::write(&path, contents).expect("write fixture file");
    }

    pub fn git(&self, args: &[&str]) -> String {
        git_in(self.path(), args)
    }

    /// `git init` with `main` as the initial branch.
    pub fn init(&self) {
        self.git(&["init", "-q", "-b", "main"]);
    }

    /// Stages everything and commits it.
    pub fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-qm", message]);
    }
}

/// Runs `git` in `cwd` with a hermetic environment and returns stdout.
pub fn git_in(cwd: &Path, args: &[&str]) -> String {
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
pub fn git_expect_failure(cwd: &Path, args: &[&str]) -> String {
    let output = git_command(cwd, args).expect("spawn git");
    assert!(
        !output.status.success(),
        "git {args:?} in {} unexpectedly succeeded",
        cwd.display()
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn git_command(cwd: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
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
pub fn with_upstream(fx: &Fixture, remote_root: &Path) -> PathBuf {
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

/// Creates a local library repository outside the worktree and adds it as the
/// `vendor/lib` submodule of `fx`. Returns the library tempdir (keep alive) and
/// the submodule worktree path.
pub fn add_submodule(fx: &Fixture) -> (tempfile::TempDir, PathBuf) {
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
