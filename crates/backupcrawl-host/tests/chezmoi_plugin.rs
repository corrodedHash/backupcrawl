//! M3.1 golden tests: the bundled `chezmoi` detector component driven against
//! a fake `chezmoi` executable.
//!
//! The fixture is a small `/bin/sh` script named `chezmoi` living in a temp
//! directory next to preset `managed.json` / `status.txt` files. `[plugins
//! .chezmoi] chezmoi-path` points at it, so the test exercises the real host
//! `exec` path (allowlist, empty environment, `HOME` forwarding) without
//! requiring chezmoi on the machine.
//!
//! Covered here: clean, target-modified (`dirty`), source-modified (`ahead`),
//! status failure (`unknown`), a managed symlink, a missing binary and the
//! snapshot cache (the second detector instance must not exec).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use backupcrawl_core::Status;
use backupcrawl_host::manifest::Manifest;
use backupcrawl_host::{
    CapabilityPolicy, Claim, DetectorError, Limits, LinkInfo, Runtime, ScanInfo, WasmDetector,
    bundled,
};

const PLUGIN: &str = "chezmoi";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// One runtime for the whole test binary; the component cache is shared with
/// the other host/CLI suites. The per-plugin KV cache is isolated so a snapshot
/// from one test never leaks into another (the cache key also contains the
/// fixture paths, but isolation keeps expectations simple).
fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let base = std::env::temp_dir().join("backupcrawl-test-cache");
        Runtime::with_cache_dirs(
            Limits::default(),
            base.join("chezmoi-kv"),
            base.join("backupcrawl/plugins/_components"),
        )
        .expect("runtime")
    })
}

fn manifest() -> Manifest {
    let plugin = bundled::all()
        .iter()
        .find(|plugin| plugin.name == PLUGIN)
        .expect("chezmoi must be in the bundled registry");
    Manifest::parse(plugin.manifest).expect("bundled chezmoi manifest")
}

fn instantiate() -> WasmDetector {
    let plugin = bundled::all()
        .iter()
        .find(|plugin| plugin.name == PLUGIN)
        .expect("chezmoi must be in the bundled registry");
    let policy = CapabilityPolicy::from_manifest(&manifest());
    runtime()
        .instantiate(plugin.wasm, PLUGIN, policy)
        .expect("instantiate chezmoi component")
}

fn scan_info(fx: &Fixture, extra: &[(&str, &str)]) -> ScanInfo {
    let mut config = vec![
        ("source-dir".to_owned(), fx.source.display().to_string()),
        ("dest-dir".to_owned(), fx.dest.display().to_string()),
        ("chezmoi-path".to_owned(), fx.script.display().to_string()),
    ];
    config.extend(
        extra
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
    );
    ScanInfo {
        root: fx.dest.clone(),
        user: "tester".to_owned(),
        follow_symlinks: false,
        config,
    }
}

fn try_detector(fx: &Fixture, extra: &[(&str, &str)]) -> Result<WasmDetector, DetectorError> {
    let mut detector = instantiate();
    detector.init(&scan_info(fx, extra))?;
    Ok(detector)
}

fn detector(fx: &Fixture, extra: &[(&str, &str)]) -> WasmDetector {
    try_detector(fx, extra).unwrap_or_else(|err| panic!("init chezmoi detector: {err}"))
}

fn file_claim(detector: &mut WasmDetector, path: &Path) -> Claim {
    detector
        .check_file(path)
        .unwrap_or_else(|err| panic!("check_file {}: {err}", path.display()))
        .unwrap_or_else(|| panic!("expected a claim for {}", path.display()))
}

fn dir_claim(detector: &mut WasmDetector, path: &Path) -> Claim {
    detector
        .check_dir(path)
        .unwrap_or_else(|err| panic!("check_dir {}: {err}", path.display()))
        .unwrap_or_else(|| panic!("expected a claim for {}", path.display()))
}

fn symlink_claim(detector: &mut WasmDetector, link: &LinkInfo) -> Claim {
    detector
        .check_symlink(link)
        .unwrap_or_else(|err| panic!("check_symlink {}: {err}", link.path.display()))
        .unwrap_or_else(|| panic!("expected a claim for {}", link.path.display()))
}

fn attr<'a>(claim: &'a Claim, key: &str) -> Option<&'a str> {
    claim
        .attrs
        .iter()
        .find(|attr| attr.key == key)
        .map(|attr| attr.value.as_str())
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Fixture {
    /// Kept alive for the fixture's lifetime.
    _dir: tempfile::TempDir,
    root: PathBuf,
    source: PathBuf,
    dest: PathBuf,
    script: PathBuf,
    managed: PathBuf,
    status: PathBuf,
    status_error: PathBuf,
    calls: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let source = dir.path().join("source");
        let dest = dir.path().join("dest");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&dest).unwrap();

        let script = dir.path().join("chezmoi");
        fs::write(
            &script,
            r#"#!/bin/sh
dir=${0%/*}
printf '%s\n' "$*" >> "$dir/calls.log"
case "$1" in
  managed)
    while IFS= read -r line || [ -n "$line" ]; do printf '%s\n' "$line"; done < "$dir/managed.json"
    ;;
  status)
    if [ -f "$dir/status-error" ]; then
      while IFS= read -r line || [ -n "$line" ]; do printf '%s\n' "$line" >&2; done < "$dir/status-error"
      exit 1
    fi
    while IFS= read -r line || [ -n "$line" ]; do printf '%s\n' "$line"; done < "$dir/status.txt"
    ;;
  *)
    printf 'unexpected command: %s\n' "$*" >&2
    exit 2
    ;;
esac
"#,
        )
        .unwrap();
        let mut perms = fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script, perms).unwrap();

        let managed = dir.path().join("managed.json");
        let status = dir.path().join("status.txt");
        let status_error = dir.path().join("status-error");
        let calls = dir.path().join("calls.log");
        fs::write(&managed, b"[]\n").unwrap();
        fs::write(&status, b"").unwrap();

        let fx = Self {
            _dir: dir,
            root,
            source,
            dest,
            script,
            managed,
            status,
            status_error,
            calls,
        };
        fx.set_managed(&[]);
        fx
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn dest(&self) -> &Path {
        &self.dest
    }

    fn source(&self) -> &Path {
        &self.source
    }

    /// Writes the `managed` JSON list (paths are stored verbatim).
    fn set_managed(&self, paths: &[PathBuf]) {
        let json = serde_json::to_vec(paths).unwrap();
        fs::write(&self.managed, json).unwrap();
    }

    fn set_status(&self, text: &str) {
        fs::write(&self.status, text).unwrap();
    }

    fn set_status_error(&self, stderr: &str) {
        fs::write(&self.status_error, stderr).unwrap();
    }

    /// Number of times the fake binary ran.
    fn calls(&self) -> usize {
        fs::read_to_string(&self.calls)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    /// A regular file in the destination, registered as managed.
    fn file(&self, rel: &str) -> PathBuf {
        let path = self.dest.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"x").unwrap();
        path
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn clean_managed_file_and_directory_are_clean() {
    let fx = Fixture::new();
    let file = fx.file(".bashrc");
    let dir = fx.dest.join(".config");
    fs::create_dir_all(&dir).unwrap();
    fx.set_managed(&[file.clone(), dir.clone()]);

    let mut detector = detector(&fx, &[]);

    let claim = file_claim(&mut detector, &file);
    assert_eq!(claim.status, Status::Clean, "{claim:?}");
    assert_eq!(attr(&claim, "source"), Some(fx.source().to_str().unwrap()));

    let claim = dir_claim(&mut detector, &dir);
    assert_eq!(claim.status, Status::Clean, "{claim:?}");

    // Unmanaged paths return no claim.
    let other = fx.dest.join("unmanaged.txt");
    fs::write(&other, b"x").unwrap();
    assert_eq!(detector.check_file(&other).unwrap(), None);

    assert_eq!(detector.report().errors, 0);
    assert_eq!(fx.calls(), 2, "managed and status run once each");
}

#[test]
fn target_modified_is_dirty() {
    let fx = Fixture::new();
    let file = fx.file(".bashrc");
    fx.set_managed(std::slice::from_ref(&file));
    fx.set_status(&format!(" M {}\n", file.display()));

    let mut detector = detector(&fx, &[]);
    let claim = file_claim(&mut detector, &file);
    assert_eq!(claim.status, Status::Dirty, "{claim:?}");
    assert_eq!(attr(&claim, "state"), Some("target-modified"));
}

#[test]
fn both_columns_modified_is_dirty() {
    let fx = Fixture::new();
    let file = fx.file(".zshrc");
    fx.set_managed(std::slice::from_ref(&file));
    fx.set_status(&format!("MM {}\n", file.display()));

    let mut detector = detector(&fx, &[]);
    let claim = file_claim(&mut detector, &file);
    assert_eq!(claim.status, Status::Dirty, "{claim:?}");
}

#[test]
fn source_modified_is_ahead() {
    let fx = Fixture::new();
    let file = fx.file(".vimrc");
    fx.set_managed(std::slice::from_ref(&file));
    fx.set_status(&format!("M  {}\n", file.display()));

    let mut detector = detector(&fx, &[]);
    let claim = file_claim(&mut detector, &file);
    assert_eq!(claim.status, Status::Ahead, "{claim:?}");
    assert_eq!(attr(&claim, "state"), Some("source-modified"));
}

#[test]
fn status_failure_is_unknown_with_a_reason() {
    let fx = Fixture::new();
    let file = fx.file(".bashrc");
    fx.set_managed(std::slice::from_ref(&file));
    fx.set_status_error("chezmoi: template: boom\n");

    let mut detector = detector(&fx, &[]);
    let claim = file_claim(&mut detector, &file);
    assert_eq!(claim.status, Status::Unknown, "{claim:?}");
    let reason = attr(&claim, "reason").expect("reason attr");
    assert!(reason.contains("chezmoi status failed"), "{reason}");
    assert!(reason.contains("template: boom"), "{reason}");
}

#[cfg(unix)]
#[test]
fn managed_symlink_is_claimed() {
    let fx = Fixture::new();
    let target = fx.file(".config/real");
    let link = fx.dest.join(".config/link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    fx.set_managed(std::slice::from_ref(&link));

    let mut detector = detector(&fx, &[]);
    let claim = symlink_claim(
        &mut detector,
        &LinkInfo {
            path: link.clone(),
            target: target.clone(),
            dangling: false,
        },
    );
    assert_eq!(claim.status, Status::Clean, "{claim:?}");
    assert_eq!(attr(&claim, "source"), Some(fx.source().to_str().unwrap()));
}

#[test]
fn missing_binary_is_an_init_error() {
    let fx = Fixture::new();
    let missing = fx._dir.path().join("does-not-exist/chezmoi");
    let mut detector = instantiate();

    let err = detector
        .init(&ScanInfo {
            root: fx.root().to_path_buf(),
            user: "tester".to_owned(),
            follow_symlinks: false,
            config: vec![
                ("source-dir".to_owned(), fx.source().display().to_string()),
                ("dest-dir".to_owned(), fx.dest().display().to_string()),
                ("chezmoi-path".to_owned(), missing.display().to_string()),
            ],
        })
        .expect_err("missing binary must fail init");

    let message = err.to_string();
    assert!(message.contains("cannot find program"), "{message}");
    assert!(detector.is_disabled(), "a failed init disables the plugin");
    assert_eq!(fx.calls(), 0, "nothing may be claimed or executed");
}

#[test]
fn cache_hit_skips_both_execs() {
    let fx = Fixture::new();
    let file = fx.file(".bashrc");
    fx.set_managed(std::slice::from_ref(&file));
    fx.set_status(&format!(" M {}\n", file.display()));

    let mut first = detector(&fx, &[]);
    assert_eq!(file_claim(&mut first, &file).status, Status::Dirty);
    assert_eq!(fx.calls(), 2, "first init execs both commands");

    let mut second = detector(&fx, &[]);
    assert_eq!(file_claim(&mut second, &file).status, Status::Dirty);
    assert_eq!(fx.calls(), 2, "a cache hit must skip both execs");
}

#[test]
fn run_scripts_are_ignored() {
    let fx = Fixture::new();
    let script = fx.source().join("run_once_setup.sh");
    fs::write(&script, b"echo setup\n").unwrap();
    let file = fx.file(".bashrc");
    fx.set_managed(&[script.clone(), file.clone()]);

    let mut detector = detector(&fx, &[]);
    assert_eq!(detector.check_file(&script).unwrap(), None);
    assert_eq!(file_claim(&mut detector, &file).status, Status::Clean);
}

/// Sanity check that the fake script receives the documented arguments.
#[test]
fn exec_receives_the_documented_commands() {
    let fx = Fixture::new();
    let _detector = detector(&fx, &[]);

    let calls = fs::read_to_string(&fx.calls).unwrap();
    let lines: Vec<&str> = calls.lines().collect();
    assert_eq!(lines.len(), 2, "{calls}");
    assert!(
        lines[0].starts_with("managed --format json --path-style absolute"),
        "{calls}"
    );
    assert!(
        lines[1].starts_with("status --path-style absolute"),
        "{calls}"
    );
    // The explicit test overrides are forwarded as global flags.
    assert!(lines[0].contains("--source"), "{calls}");
    assert!(lines[0].contains("--destination"), "{calls}");
}
