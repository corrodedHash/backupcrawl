//! M2.1 golden tests: the bundled `pacman` detector component driven against a
//! programmatically generated pacman database.
//!
//! Unlike the git suite these fixtures do not need `pacman`: the test builds
//! `<db>/local/<pkg>-<ver>/{files,desc,mtree}` itself and points the plugin at
//! it through `[plugins.pacman] db-path`. That is exactly the integration the
//! detector promises (§7.2): native parsing, no process, verbatim paths.
//!
//! Covered here: clean file, modified file, deleted file, type change, symlink
//! match/mismatch, `%BACKUP%` file, a path with spaces, an unknown mtree entry
//! and checksum level.

use std::fs;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};

use backupcrawl_core::Status;
use backupcrawl_host::manifest::Manifest;
use backupcrawl_host::{
    CapabilityPolicy, Claim, Limits, LinkInfo, Runtime, ScanInfo, WasmDetector, bundled,
};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// One runtime for the whole test binary. The component cache is shared with
/// the other host/CLI suites (compiling the bundled components is the expensive
/// part); the per-plugin KV cache is isolated so pacman index entries never
/// leak into another test's expectations.
fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let base = std::env::temp_dir().join("backupcrawl-test-cache");
        Runtime::with_cache_dirs(
            Limits::default(),
            base.join("pacman-kv"),
            base.join("backupcrawl/plugins/_components"),
        )
        .expect("runtime")
    })
}

fn detector(db: &Path, root: &Path, config: &[(&str, &str)]) -> WasmDetector {
    let plugin = bundled::all()
        .iter()
        .find(|plugin| plugin.name == "pacman")
        .expect("pacman must be in the bundled registry");
    let manifest = Manifest::parse(plugin.manifest).expect("bundled pacman manifest");
    let policy = CapabilityPolicy::from_manifest(&manifest);
    let mut detector = runtime()
        .instantiate(plugin.wasm, "pacman", policy)
        .expect("instantiate pacman component");

    let mut pairs = vec![("db-path".to_owned(), db.display().to_string())];
    pairs.extend(
        config
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
    );
    detector
        .init(&ScanInfo {
            root: root.to_path_buf(),
            user: "tester".to_owned(),
            follow_symlinks: false,
            config: pairs,
        })
        .unwrap_or_else(|err| panic!("init pacman detector: {err}"));
    detector
}

fn attr<'a>(claim: &'a Claim, key: &str) -> Option<&'a str> {
    claim
        .attrs
        .iter()
        .find(|attr| attr.key == key)
        .map(|attr| attr.value.as_str())
}

fn reason(claim: &Claim) -> &str {
    attr(claim, "reason").unwrap_or("")
}

fn file_claim(detector: &mut WasmDetector, path: &Path) -> Claim {
    detector
        .check_file(path)
        .unwrap_or_else(|err| panic!("check_file {}: {err}", path.display()))
        .unwrap_or_else(|| panic!("expected a claim for {}", path.display()))
}

fn symlink_claim(detector: &mut WasmDetector, link: &Path) -> Claim {
    let info = LinkInfo {
        path: link.to_path_buf(),
        target: fs::read_link(link).unwrap(),
        dangling: !link.exists(),
    };
    detector
        .check_symlink(&info)
        .unwrap_or_else(|err| panic!("check_symlink {}: {err}", link.display()))
        .unwrap_or_else(|| panic!("expected a claim for {}", link.display()))
}

// ---------------------------------------------------------------------------
// Fixture database
// ---------------------------------------------------------------------------

struct Fixture {
    /// Kept alive for the fixture's lifetime.
    _dir: tempfile::TempDir,
    db: PathBuf,
    root: PathBuf,
    files: Vec<String>,
    /// `%BACKUP%` entries (root-relative, like the real database).
    backup: Vec<String>,
    mtree: Vec<String>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("db");
        let root = dir.path().join("root");
        fs::create_dir_all(&root).unwrap();
        Self {
            _dir: dir,
            db,
            root,
            files: Vec::new(),
            backup: Vec::new(),
            mtree: Vec::new(),
        }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn db(&self) -> &Path {
        &self.db
    }

    fn write(&mut self, rel: &str, contents: &[u8]) -> PathBuf {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, contents).unwrap();
        path
    }

    /// Registers a regular file with the live metadata and both digests.
    fn file(&mut self, rel: &str, contents: &[u8]) -> PathBuf {
        self.file_with(rel, contents, true)
    }

    fn file_with(&mut self, rel: &str, contents: &[u8], digests: bool) -> PathBuf {
        let path = self.write(rel, contents);
        let meta = fs::metadata(&path).unwrap();
        let mut fields = format!(
            "type=file size={} time={} mode={:o} uid={} gid={}",
            meta.len(),
            mtime_seconds(&meta),
            meta.mode() & 0o7777,
            meta.uid(),
            meta.gid(),
        );
        if digests {
            let sha = hex_sha256(contents);
            fields.push_str(&format!(" sha256digest={sha}"));
        }
        self.entry(&path, &fields);
        path
    }

    fn symlink(&mut self, rel: &str, target: &str) -> PathBuf {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        std::os::unix::fs::symlink(target, &path).unwrap();
        let meta = fs::symlink_metadata(&path).unwrap();
        self.entry(
            &path,
            &format!(
                "type=link link={} time={} mode={:o} uid={} gid={}",
                mtree_escape(target),
                mtime_seconds(&meta),
                meta.mode() & 0o7777,
                meta.uid(),
                meta.gid(),
            ),
        );
        path
    }

    /// Adds a literal mtree field list for `path` (used for malformed/unknown
    /// entries); the `files` entry is still written so the path is owned.
    fn entry(&mut self, path: &Path, fields: &str) {
        let relative = path
            .strip_prefix("/")
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        if !self.files.contains(&relative) {
            self.files.push(relative);
        }
        self.mtree.push(format!(
            "./{} {fields}",
            mtree_escape(&path.to_string_lossy())
        ));
    }

    fn backup(&mut self, path: &Path) {
        let relative = path
            .strip_prefix("/")
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        self.backup.push(relative);
    }

    /// Writes `<db>/local/demo-1.0-1/{files,desc,mtree}` with one package.
    fn write_db(&self) {
        let package = self.db.join("local").join("demo-1.0-1");
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join("files"),
            format!("%FILES%\n{}\n", self.files.join("\n")),
        )
        .unwrap();
        fs::write(
            package.join("desc"),
            format!(
                "%NAME%\ndemo\n\n%VERSION%\n1.0-1\n\n%BACKUP%\n{}\n",
                self.backup.join("\n")
            ),
        )
        .unwrap();
        let text = format!(
            "#mtree\n/set type=file uid=0 gid=0 mode=644\n{}\n",
            self.mtree.join("\n")
        );
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(text.as_bytes()).unwrap();
        fs::write(package.join("mtree"), encoder.finish().unwrap()).unwrap();
    }
}

fn mtime_seconds(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn hex_sha256(contents: &[u8]) -> String {
    hex::encode(Sha256::digest(contents))
}

/// BSD mtree escapes: `\` and whitespace (and `#`) are octal.
fn mtree_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\134"),
            ' ' => out.push_str("\\040"),
            '\t' => out.push_str("\\011"),
            '\n' => out.push_str("\\012"),
            '#' => out.push_str("\\043"),
            other => out.push(other),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn clean_file_is_clean_with_package_attrs() {
    let mut fx = Fixture::new();
    let path = fx.file("usr/bin/demo", b"contents\n");
    fx.write_db();

    let mut detector = detector(fx.db(), fx.root(), &[]);
    let claim = file_claim(&mut detector, &path);

    assert_eq!(claim.status, Status::Clean, "{claim:?}");
    assert_eq!(attr(&claim, "package"), Some("demo"));
    assert_eq!(attr(&claim, "version"), Some("1.0-1"));
    assert_eq!(attr(&claim, "reason"), None);
    assert_eq!(detector.report().errors, 0);
}

#[test]
fn modified_file_reports_size_and_time_mismatch() {
    let mut fx = Fixture::new();
    let path = fx.file("usr/bin/demo", b"contents\n");
    fx.write_db();
    fs::write(&path, b"different contents\n").unwrap();

    let mut detector = detector(fx.db(), fx.root(), &[]);
    let claim = file_claim(&mut detector, &path);

    assert_eq!(claim.status, Status::Dirty);
    assert!(
        reason(&claim).contains("Size mismatch"),
        "got: {}",
        reason(&claim)
    );
}

#[test]
fn deleted_file_is_dirty_not_unclaimed() {
    let mut fx = Fixture::new();
    let path = fx.file("usr/bin/demo", b"contents\n");
    fx.write_db();
    fs::remove_file(&path).unwrap();

    let mut detector = detector(fx.db(), fx.root(), &[]);
    let claim = file_claim(&mut detector, &path);

    assert_eq!(claim.status, Status::Dirty);
    assert_eq!(reason(&claim), "No such file or directory");
}

#[test]
fn type_change_is_dirty() {
    let mut fx = Fixture::new();
    // The database expects a file; the live object is a directory. Register
    // only the `type=file` entry (a real mtree never lists a path twice).
    let path = fx.root().join("usr/lib/thing");
    fs::create_dir_all(&path).unwrap();
    fx.entry(&path, "type=file size=1 time=1 mode=644");
    fx.write_db();

    let mut detector = detector(fx.db(), fx.root(), &[]);
    let claim = detector
        .check_dir(&path)
        .unwrap()
        .expect("the path is owned");
    assert_eq!(claim.status, Status::Dirty);
    assert_eq!(reason(&claim), "File type mismatch");
}

#[test]
fn symlink_match_and_mismatch() {
    let mut fx = Fixture::new();
    let _target = fx.file("usr/lib/demo/target", b"x");
    let link = fx.symlink("usr/bin/demo", "../lib/demo/target");
    fx.write_db();

    let mut detector = detector(fx.db(), fx.root(), &[]);
    let claim = symlink_claim(&mut detector, &link);
    assert_eq!(claim.status, Status::Clean, "{claim:?}");
    assert_eq!(attr(&claim, "package"), Some("demo"));

    // Point the live link somewhere else; the raw target no longer matches.
    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink("elsewhere", &link).unwrap();
    let claim = symlink_claim(&mut detector, &link);
    assert_eq!(claim.status, Status::Dirty, "{claim:?}");
    assert!(
        reason(&claim).contains("Symlink path mismatch"),
        "got: {}",
        reason(&claim)
    );
}

#[test]
fn absolute_symlink_match_and_mismatch() {
    // cap-std cannot read a link whose target is absolute, so this exercises
    // the walker-provided target fallback against a realistic `/usr/lib` link.
    let mut fx = Fixture::new();
    let _target = fx.file("usr/lib/libfoo.so.1", b"x");
    let link = fx.symlink("usr/lib/libfoo.so", "/usr/lib/libfoo.so.1");
    fx.write_db();

    let mut detector = detector(fx.db(), fx.root(), &[]);
    let claim = symlink_claim(&mut detector, &link);
    assert_eq!(claim.status, Status::Clean, "{claim:?}");

    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink("/usr/lib/libbar.so.1", &link).unwrap();
    let claim = symlink_claim(&mut detector, &link);
    assert_eq!(claim.status, Status::Dirty, "{claim:?}");
    assert!(
        reason(&claim).contains("Symlink path mismatch"),
        "got: {}",
        reason(&claim)
    );
}

#[test]
fn backup_file_is_flagged_and_checked_normally() {
    let mut fx = Fixture::new();
    let path = fx.file("etc/demo.conf", b"default\n");
    fx.backup(&path);
    fx.write_db();
    fs::write(&path, b"edited\n").unwrap();

    let mut detector = detector(fx.db(), fx.root(), &[]);
    let claim = file_claim(&mut detector, &path);

    assert_eq!(claim.status, Status::Dirty);
    assert_eq!(attr(&claim, "backup"), Some("true"));
    assert!(reason(&claim).contains("Size mismatch"), "{claim:?}");
}

#[test]
fn path_with_spaces_is_owned_verbatim() {
    let mut fx = Fixture::new();
    let path = fx.file("usr/share/my app/config file", b"x");
    fx.write_db();

    let mut detector = detector(fx.db(), fx.root(), &[]);
    let claim = file_claim(&mut detector, &path);
    assert_eq!(claim.status, Status::Clean, "{claim:?}");
    assert_eq!(attr(&claim, "package"), Some("demo"));
}

#[test]
fn unknown_mtree_entry_is_unknown_with_a_diagnostic() {
    let mut fx = Fixture::new();
    let path = fx.write("usr/bin/odd", b"x");
    fx.entry(&path, "type=fifo size=1");
    fx.write_db();

    let mut detector = detector(fx.db(), fx.root(), &[]);
    let claim = file_claim(&mut detector, &path);

    assert_eq!(claim.status, Status::Unknown);
    assert!(
        reason(&claim).contains("Unparsable mtree entry"),
        "got: {}",
        reason(&claim)
    );
}

#[test]
fn checksum_level_detects_same_size_edits() {
    let mut fx = Fixture::new();
    let path = fx.file("usr/bin/demo", b"hello");
    fx.write_db();
    let original = fs::metadata(&path).unwrap().modified().unwrap();

    let mut detector = detector(fx.db(), fx.root(), &[("check-level", "checksum")]);
    let claim = file_claim(&mut detector, &path);
    assert_eq!(claim.status, Status::Clean, "{claim:?}");

    // Same size, same mtime: only the digest can notice the edit.
    fs::write(&path, b"world").unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(original)
        .unwrap();
    let claim = file_claim(&mut detector, &path);
    assert_eq!(claim.status, Status::Dirty);
    assert_eq!(reason(&claim), "SHA256 checksum mismatch");
}

#[test]
fn checksum_level_without_a_digest_is_unknown() {
    let mut fx = Fixture::new();
    let path = fx.file_with("usr/bin/demo", b"hello", false);
    fx.write_db();

    let mut detector = detector(fx.db(), fx.root(), &[("check-level", "checksum")]);
    let claim = file_claim(&mut detector, &path);

    assert_eq!(claim.status, Status::Unknown);
    assert_eq!(reason(&claim), "No checksum in mtree");
}

#[test]
fn unowned_paths_are_not_claimed() {
    let mut fx = Fixture::new();
    fx.file("usr/bin/demo", b"x");
    fx.write_db();
    let other = fx.write("usr/bin/other", b"x");

    let mut detector = detector(fx.db(), fx.root(), &[]);
    assert_eq!(detector.check_file(&other).unwrap(), None);
}

#[test]
fn missing_database_is_an_init_error() {
    let dir = tempfile::tempdir().unwrap();
    let plugin = bundled::all()
        .iter()
        .find(|plugin| plugin.name == "pacman")
        .unwrap();
    let manifest = Manifest::parse(plugin.manifest).unwrap();
    let mut detector = runtime()
        .instantiate(
            plugin.wasm,
            "pacman",
            CapabilityPolicy::from_manifest(&manifest),
        )
        .unwrap();

    let err = detector
        .init(&ScanInfo {
            root: dir.path().to_path_buf(),
            user: "tester".to_owned(),
            follow_symlinks: false,
            config: vec![(
                "db-path".to_owned(),
                dir.path().join("no-db").display().to_string(),
            )],
        })
        .unwrap_err();

    let message = err.to_string();
    assert!(message.contains("pacman database not found"), "{message}");
    assert!(detector.is_disabled(), "a failed init disables the plugin");
}
