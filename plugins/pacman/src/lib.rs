//! Bundled pacman detector plugin.
//!
//! Answers the three questions of §3 of `docs/rust-rewrite-plan.md` for Arch
//! Linux packages. Detection is **native**: the plugin parses
//! `<db-path>/local/<pkg>-<ver>/{files,desc,mtree}` itself and never spawns
//! `pacman` (§7.2).
//!
//! * **Managed?** A path is owned when it appears in a package's `files` list.
//!   Paths are verbatim (spaces included) and normalized to absolute `/...`
//!   keys; directories and symlinks are owned like files.
//! * **Current?** The live object is compared with its gzipped BSD mtree entry:
//!   type, size, mtime (seconds), permission bits, uid, gid and — for links —
//!   the raw target. `check-level = "checksum"` optionally hashes file
//!   contents against the mtree md5/sha256.
//! * **Propagated?** A clean package file is recoverable by reinstalling the
//!   package, so it is `clean`; modified files (including `%BACKUP%` files the
//!   administrator edited) are `dirty` and need their own backup.
//!
//! `init` builds one path → package/mtree index for the paths at or below the
//! crawl root and caches it through the host cache keyed by the database path,
//! the crawl root and the `local/` mtime. Scoping keeps a scan of `~` or a
//! project tree from reading the whole system database; for a full-system scan
//! the host grants `init` a larger fuel budget than ordinary calls. A missing
//! or unreadable database is an `init` error, so the host disables the plugin
//! with a visible note; a corrupt cache is ignored and rebuilt. Unparseable
//! mtree entries produce `unknown` with a diagnostic, never a wrong `clean`.
//!
//! # Config
//!
//! `db-path` (default `/var/lib/pacman`), `check-level = "metadata"` (default)
//! or `"checksum"`, `backend = "native"` (the `pacman -Qkk` fallback is
//! deferred and rejected with a clear error), `timeout-ms` (parsed and
//! reserved; the native backend runs synchronously under the host's fuel/epoch
//! limits).

mod check;
mod config;
mod db;

use std::path::{Path, PathBuf};

use backupcrawl_plugin_sdk::{
    Claim, Detector, DetectorError, Host, LinkInfo, LogLevel, ScanInfo, bind_detector,
};

use config::PacmanConfig;
use db::Index;

/// The pacman detector's mutable state (one instance per wasm store).
#[derive(Debug, Default)]
pub struct Pacman {
    /// Validated `[plugins.pacman]` config.
    config: PacmanConfig,
    /// Crawl root, used to resolve relative paths.
    root: PathBuf,
    /// Database snapshot for this scan.
    index: Index,
}

impl Pacman {
    fn answer(
        &mut self,
        path: &Path,
        link: Option<&LinkInfo>,
        host: &mut dyn Host,
    ) -> Result<Option<Claim>, DetectorError> {
        Ok(check::check(
            &mut self.index,
            &self.config,
            &self.root,
            path,
            link,
            host,
        ))
    }
}

impl Detector for Pacman {
    fn init(&mut self, info: &ScanInfo, host: &mut dyn Host) -> Result<(), DetectorError> {
        self.config = PacmanConfig::from_config(&info.config)?;
        self.root = check::canonical_root(&info.root);
        self.index = db::load_index(
            &self.config.db_path,
            &check::path_string(&self.root),
            self.config.check_level.as_str(),
            host,
        )?;
        host.log(
            LogLevel::Debug,
            &format!(
                "pacman: db={} check-level={} backend={} paths={}",
                self.config.db_path.display(),
                self.config.check_level.as_str(),
                self.config.backend.as_str(),
                self.index.len()
            ),
        );
        Ok(())
    }

    fn check_dir(
        &mut self,
        path: &Path,
        host: &mut dyn Host,
    ) -> Result<Option<Claim>, DetectorError> {
        self.answer(path, None, host)
    }

    fn check_file(
        &mut self,
        path: &Path,
        host: &mut dyn Host,
    ) -> Result<Option<Claim>, DetectorError> {
        self.answer(path, None, host)
    }

    fn check_symlink(
        &mut self,
        link: &LinkInfo,
        host: &mut dyn Host,
    ) -> Result<Option<Claim>, DetectorError> {
        self.answer(&link.path, Some(link), host)
    }
}

// Emits the component export glue for the canonical `plugin` world.
bind_detector!(Pacman);

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Write;

    use backupcrawl_plugin_sdk::{Attr, Config, FileInfo, FileKind, MockHost, Status};
    use flate2::Compression;
    use flate2::write::GzEncoder;

    use super::*;
    use crate::db::{MtreeEntry, MtreeKind, OwnedPath};

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn scan_info(root: &Path, attrs: &[(&str, &str)]) -> ScanInfo {
        ScanInfo {
            root: root.to_path_buf(),
            user: "tester".to_owned(),
            follow_symlinks: false,
            config: Config::new(
                attrs
                    .iter()
                    .map(|(key, value)| Attr::new(*key, *value))
                    .collect(),
            ),
        }
    }

    fn owned(name: &str, backup: bool, mtree: MtreeEntry) -> OwnedPath {
        OwnedPath {
            package: name.to_owned(),
            version: "1.0-1".to_owned(),
            backup,
            mtree: Some(mtree),
        }
    }

    fn file_mtree(size: u64, time: i64, mode: u32, uid: u32, gid: u32) -> MtreeEntry {
        MtreeEntry {
            kind: Some(MtreeKind::File),
            size: Some(size),
            time: Some(time),
            mode: Some(mode),
            uid: Some(uid),
            gid: Some(gid),
            ..MtreeEntry::default()
        }
    }

    fn file_info(size: u64, mtime_ms: u64, mode: u32, uid: u32, gid: u32) -> FileInfo {
        FileInfo {
            kind: FileKind::File,
            size,
            mtime_ms,
            mode,
            uid,
            gid,
            nlink: 1,
        }
    }

    fn index(entries: &[(&str, OwnedPath)]) -> Index {
        let entries = entries
            .iter()
            .map(|(path, owned)| ((*path).to_owned(), owned.clone()))
            .collect::<BTreeMap<_, _>>();
        Index::from_entries(&entries)
    }

    fn reason(claim: &Claim) -> &str {
        claim
            .attrs
            .iter()
            .find(|attr| attr.key == "reason")
            .map(|attr| attr.value.as_str())
            .unwrap_or("")
    }

    fn write_package(
        db: &Path,
        dir_name: &str,
        name: &str,
        version: &str,
        files: &str,
        backup: &str,
        mtree: &str,
    ) {
        let package = db.join("local").join(dir_name);
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("desc"),
            format!("%NAME%\n{name}\n\n%VERSION%\n{version}\n\n%BACKUP%\n{backup}\n"),
        )
        .unwrap();
        std::fs::write(package.join("files"), format!("%FILES%\n{files}\n")).unwrap();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(mtree.as_bytes()).unwrap();
        std::fs::write(package.join("mtree"), encoder.finish().unwrap()).unwrap();
    }

    // -----------------------------------------------------------------------
    // Lifecycle and cache
    // -----------------------------------------------------------------------

    #[test]
    fn init_fails_with_a_clear_message_when_the_database_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("missing");
        let mut detector = Pacman::default();
        let mut host = MockHost::new();

        let err = detector
            .init(
                &scan_info(Path::new("/"), &[("db-path", db.to_str().unwrap())]),
                &mut host,
            )
            .unwrap_err();

        assert!(err.message().contains("pacman database not found"), "{err}");
        assert!(err.message().contains("db-path"), "{err}");
    }

    #[test]
    fn init_builds_an_index_and_serves_it_from_the_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db");
        write_package(
            &db,
            "demo-1.0-1",
            "demo",
            "1.0-1",
            "usr/bin/demo",
            "",
            "./usr/bin/demo type=file size=1 mode=644\n",
        );

        let mut first = Pacman::default();
        let mut host = MockHost::new();
        first
            .init(
                &scan_info(Path::new("/"), &[("db-path", db.to_str().unwrap())]),
                &mut host,
            )
            .unwrap();
        assert_eq!(first.index.len(), 1);
        let writes = host.cache_sets().to_vec();
        assert!(
            writes.iter().any(|(key, _)| key.contains("pacman-index-v")),
            "{writes:?}"
        );

        // A fresh instance with the same cache entries must not rebuild.
        let mut cache = MockHost::new();
        for (key, value) in &writes {
            cache = cache.with_cache(key.clone(), value.clone());
        }
        let mut second = Pacman::default();
        let mut host = cache;
        second
            .init(
                &scan_info(Path::new("/"), &[("db-path", db.to_str().unwrap())]),
                &mut host,
            )
            .unwrap();
        assert_eq!(second.index.len(), 1);
        assert!(host.cache_sets().is_empty(), "cache should be a hit");
        assert!(
            host.logs()
                .iter()
                .any(|(_, message)| message.contains("cache hit")),
            "{:?}",
            host.logs()
        );
    }

    #[test]
    fn corrupt_cache_is_rebuilt_instead_of_failing() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db");
        write_package(
            &db,
            "demo-1.0-1",
            "demo",
            "1.0-1",
            "usr/bin/demo",
            "",
            "./usr/bin/demo type=file size=1 mode=644\n",
        );
        let mtime = std::fs::metadata(db.join("local")).unwrap().modified().ok();
        let key = db::cache_key(&db, "/", "metadata", mtime);

        let mut detector = Pacman::default();
        let mut host = MockHost::new().with_cache(&key, b"not a meta entry".to_vec());
        detector
            .init(
                &scan_info(Path::new("/"), &[("db-path", db.to_str().unwrap())]),
                &mut host,
            )
            .unwrap();

        assert_eq!(detector.index.len(), 1);
        assert!(
            !host.cache_sets().is_empty(),
            "the index is rebuilt and recached"
        );
        assert!(
            host.logs()
                .iter()
                .any(|(_, message)| message.contains("corrupt index cache")),
            "{:?}",
            host.logs()
        );
    }

    #[test]
    fn index_is_scoped_to_the_crawl_root() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("db");
        write_package(
            &db,
            "demo-1.0-1",
            "demo",
            "1.0-1",
            "usr/bin/demo",
            "",
            "./usr/bin/demo type=file size=1 mode=644\n",
        );
        let db_path = db.to_str().unwrap();

        // A scan outside the package tree must not index (or parse the mtree
        // of) any package.
        let mut outside = Pacman::default();
        let mut host = MockHost::new();
        outside
            .init(
                &scan_info(Path::new("/home/tester"), &[("db-path", db_path)]),
                &mut host,
            )
            .unwrap();
        assert_eq!(outside.index.len(), 0);

        // The same database still yields the owned path for a root that
        // contains it.
        let mut inside = Pacman::default();
        let mut host = MockHost::new();
        inside
            .init(
                &scan_info(Path::new("/"), &[("db-path", db_path)]),
                &mut host,
            )
            .unwrap();
        assert_eq!(inside.index.len(), 1);
    }

    // -----------------------------------------------------------------------
    // Detection and reason mapping
    // -----------------------------------------------------------------------

    #[test]
    fn metadata_mismatches_map_to_pacman_style_reasons() {
        let path = Path::new("/pkg/file");
        let mut idx = index(&[(
            "/pkg/file",
            owned("demo", false, file_mtree(999, 100, 0o600, 1, 2)),
        )]);
        let config = PacmanConfig::default();
        let mut host = MockHost::new().with_stat(path, file_info(1, 101_000, 0o100644, 0, 0));

        let claim = check::check(&mut idx, &config, Path::new("/"), path, None, &mut host).unwrap();
        assert_eq!(claim.status, Status::Dirty);
        assert_eq!(
            reason(&claim),
            "Size mismatch, Modification time mismatch, Permissions mismatch, \
             UID mismatch, GID mismatch"
        );
        assert!(claim.attrs.contains(&Attr::new("package", "demo")));
        assert!(claim.attrs.contains(&Attr::new("version", "1.0-1")));
    }

    #[test]
    fn matching_metadata_is_clean() {
        let path = Path::new("/pkg/file");
        let mut idx = index(&[(
            "/pkg/file",
            owned("demo", false, file_mtree(5, 100, 0o644, 0, 0)),
        )]);
        let config = PacmanConfig::default();
        let mut host = MockHost::new().with_stat(path, file_info(5, 100_500, 0o100644, 0, 0));

        let claim = check::check(&mut idx, &config, Path::new("/"), path, None, &mut host).unwrap();
        assert_eq!(claim.status, Status::Clean);
        assert_eq!(reason(&claim), "");
    }

    #[test]
    fn file_type_mismatch_is_dirty() {
        let path = Path::new("/pkg/thing");
        let mut mtree = file_mtree(1, 1, 0o644, 0, 0);
        mtree.kind = Some(MtreeKind::Dir);
        let mut idx = index(&[("/pkg/thing", owned("demo", false, mtree))]);
        let mut host = MockHost::new().with_stat(path, file_info(1, 1_000, 0o100644, 0, 0));

        let claim = check::check(
            &mut idx,
            &PacmanConfig::default(),
            Path::new("/"),
            path,
            None,
            &mut host,
        )
        .unwrap();
        assert_eq!(claim.status, Status::Dirty);
        assert_eq!(reason(&claim), "File type mismatch");
    }

    #[test]
    fn missing_path_is_dirty_with_a_reason() {
        let path = Path::new("/pkg/gone");
        let mut idx = index(&[(
            "/pkg/gone",
            owned("demo", false, file_mtree(1, 1, 0o644, 0, 0)),
        )]);
        let mut host = MockHost::new().with_stat_error(
            path,
            "stat `/pkg/gone`: No such file or directory (os error 2)",
        );

        let claim = check::check(
            &mut idx,
            &PacmanConfig::default(),
            Path::new("/"),
            path,
            None,
            &mut host,
        )
        .unwrap();
        assert_eq!(claim.status, Status::Dirty);
        assert_eq!(reason(&claim), "No such file or directory");
    }

    #[test]
    fn unowned_paths_are_not_claimed() {
        let mut host = MockHost::new();
        let claim = check::check(
            &mut Index::empty(),
            &PacmanConfig::default(),
            Path::new("/"),
            Path::new("/not/owned"),
            None,
            &mut host,
        );
        assert!(claim.is_none());
    }

    #[test]
    fn unknown_mtree_entries_are_unknown_not_clean() {
        let path = Path::new("/pkg/odd");
        let mtree = MtreeEntry {
            kind: None,
            invalid: Some("unknown mtree type `fifo`".to_owned()),
            ..MtreeEntry::default()
        };
        let mut idx = index(&[("/pkg/odd", owned("demo", false, mtree))]);
        let mut host = MockHost::new().with_stat(path, file_info(1, 1_000, 0o100644, 0, 0));

        let claim = check::check(
            &mut idx,
            &PacmanConfig::default(),
            Path::new("/"),
            path,
            None,
            &mut host,
        )
        .unwrap();
        assert_eq!(claim.status, Status::Unknown);
        assert!(reason(&claim).contains("Unparsable mtree entry"));

        // A package path with no mtree entry at all is unknown as well.
        let mut idx = index(&[(
            "/pkg/odd",
            OwnedPath {
                package: "demo".to_owned(),
                version: "1.0-1".to_owned(),
                backup: false,
                mtree: None,
            },
        )]);
        let mut host = MockHost::new().with_stat(path, file_info(1, 1_000, 0o100644, 0, 0));
        let claim = check::check(
            &mut idx,
            &PacmanConfig::default(),
            Path::new("/"),
            path,
            None,
            &mut host,
        )
        .unwrap();
        assert_eq!(claim.status, Status::Unknown);
        assert!(reason(&claim).contains("No mtree entry"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_targets_are_compared_verbatim() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        std::fs::write(&target, b"x").unwrap();
        let path = tmp.path().join("link");
        symlink("target", &path).unwrap();

        let mtree = MtreeEntry {
            kind: Some(MtreeKind::Link),
            link: Some("target".to_owned()),
            time: Some(1),
            mode: Some(0o777),
            uid: Some(0),
            gid: Some(0),
            ..MtreeEntry::default()
        };
        let key = path.to_string_lossy().into_owned();
        let mut idx = index(&[(key.as_str(), owned("demo", false, mtree.clone()))]);
        let info = FileInfo {
            kind: FileKind::Symlink,
            size: 6,
            mtime_ms: 1_000,
            mode: 0o120777,
            uid: 0,
            gid: 0,
            nlink: 1,
        };
        let link_info = LinkInfo {
            path: path.clone(),
            target: path.clone(), // unused: the guest can read the relative link
            dangling: false,
        };
        let mut host = MockHost::new().with_stat(&path, info);
        let claim = check::check(
            &mut idx,
            &PacmanConfig::default(),
            Path::new("/"),
            &path,
            Some(&link_info),
            &mut host,
        )
        .unwrap();
        assert_eq!(claim.status, Status::Clean);

        // A live relative link that points elsewhere is a mismatch.
        std::fs::remove_file(&path).unwrap();
        symlink("elsewhere", &path).unwrap();
        let mut idx = index(&[(key.as_str(), owned("demo", false, mtree))]);
        let mut host = MockHost::new().with_stat(&path, info);
        let claim = check::check(
            &mut idx,
            &PacmanConfig::default(),
            Path::new("/"),
            &path,
            Some(&link_info),
            &mut host,
        )
        .unwrap();
        assert_eq!(claim.status, Status::Dirty);
        assert_eq!(reason(&claim), "Symlink path mismatch");
    }

    #[cfg(unix)]
    #[test]
    fn absolute_symlink_targets_use_the_host_provided_target() {
        // cap-std refuses to read a link whose stored target is absolute, so
        // the guest falls back to the walker-provided `LinkInfo.target`.
        let path = Path::new("/pkg/link");
        let mut mtree = MtreeEntry {
            kind: Some(MtreeKind::Link),
            link: Some("/usr/lib/libfoo.so.1".to_owned()),
            ..MtreeEntry::default()
        };
        let info = FileInfo {
            kind: FileKind::Symlink,
            size: 19,
            mtime_ms: 1_000,
            mode: 0o120777,
            uid: 0,
            gid: 0,
            nlink: 1,
        };

        let matching = LinkInfo {
            path: path.to_path_buf(),
            target: PathBuf::from("/usr/lib/libfoo.so.1"),
            dangling: false,
        };
        let mut idx = index(&[("/pkg/link", owned("demo", false, mtree.clone()))]);
        let mut host = MockHost::new().with_stat(path, info);
        let claim = check::check(
            &mut idx,
            &PacmanConfig::default(),
            Path::new("/"),
            path,
            Some(&matching),
            &mut host,
        )
        .unwrap();
        assert_eq!(claim.status, Status::Clean);

        mtree.link = Some("/usr/lib/libbar.so.1".to_owned());
        let mut idx = index(&[("/pkg/link", owned("demo", false, mtree))]);
        let mut host = MockHost::new().with_stat(path, info);
        let claim = check::check(
            &mut idx,
            &PacmanConfig::default(),
            Path::new("/"),
            path,
            Some(&matching),
            &mut host,
        )
        .unwrap();
        assert_eq!(claim.status, Status::Dirty);
        assert_eq!(reason(&claim), "Symlink path mismatch");
    }

    #[test]
    fn backup_paths_are_flagged_but_checked_normally() {
        let path = Path::new("/etc/demo.conf");
        let mut idx = index(&[(
            "/etc/demo.conf",
            owned("demo", true, file_mtree(5, 100, 0o644, 0, 0)),
        )]);
        let mut host = MockHost::new().with_stat(path, file_info(4, 100_000, 0o100644, 0, 0));

        let claim = check::check(
            &mut idx,
            &PacmanConfig::default(),
            Path::new("/"),
            path,
            None,
            &mut host,
        )
        .unwrap();
        assert_eq!(claim.status, Status::Dirty);
        assert_eq!(reason(&claim), "Size mismatch");
        assert!(claim.attrs.contains(&Attr::new("backup", "true")));
    }

    #[test]
    fn checksum_level_verifies_md5_and_sha256() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("file");
        std::fs::write(&path, b"hello").unwrap();

        let mut mtree = file_mtree(5, 100, 0o644, 0, 0);
        mtree.md5 = Some("5d41402abc4b2a76b9719d911017c592".to_owned());
        mtree.sha256 =
            Some("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_owned());
        let key = path.to_string_lossy().into_owned();
        let mut idx = index(&[(key.as_str(), owned("demo", false, mtree.clone()))]);
        let config = PacmanConfig {
            check_level: crate::config::CheckLevel::Checksum,
            ..PacmanConfig::default()
        };
        let info = file_info(5, 100_000, 0o100644, 0, 0);

        let mut host = MockHost::new().with_stat(&path, info);
        let claim =
            check::check(&mut idx, &config, Path::new("/"), &path, None, &mut host).unwrap();
        assert_eq!(claim.status, Status::Clean);

        // Same size and metadata, different content: only the digest notices.
        std::fs::write(&path, b"world").unwrap();
        let mut host = MockHost::new().with_stat(&path, info);
        let claim =
            check::check(&mut idx, &config, Path::new("/"), &path, None, &mut host).unwrap();
        assert_eq!(claim.status, Status::Dirty);
        assert_eq!(
            reason(&claim),
            "MD5 checksum mismatch, SHA256 checksum mismatch"
        );

        // Missing digests at checksum level must not be reported clean.
        mtree.md5 = None;
        mtree.sha256 = None;
        let mut idx = index(&[(key.as_str(), owned("demo", false, mtree))]);
        let mut host = MockHost::new().with_stat(&path, info);
        let claim =
            check::check(&mut idx, &config, Path::new("/"), &path, None, &mut host).unwrap();
        assert_eq!(claim.status, Status::Unknown);
        assert_eq!(reason(&claim), "No checksum in mtree");
    }

    #[test]
    fn metadata_level_does_not_read_file_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("file");
        std::fs::write(&path, b"hello").unwrap();
        let mut mtree = file_mtree(5, 100, 0o644, 0, 0);
        mtree.sha256 = Some("0".repeat(64));
        let key = path.to_string_lossy().into_owned();
        let mut idx = index(&[(key.as_str(), owned("demo", false, mtree))]);
        let mut host = MockHost::new().with_stat(&path, file_info(5, 100_000, 0o100644, 0, 0));

        let claim = check::check(
            &mut idx,
            &PacmanConfig::default(),
            Path::new("/"),
            &path,
            None,
            &mut host,
        )
        .unwrap();
        assert_eq!(claim.status, Status::Clean);
    }

    #[test]
    fn relative_paths_resolve_against_the_crawl_root() {
        let root = Path::new("/tmp/root");
        assert_eq!(
            check::lookup_key(root, Path::new("./usr/bin/demo")),
            "/tmp/root/usr/bin/demo"
        );
        assert_eq!(
            check::lookup_key(root, Path::new("/usr/bin/demo")),
            "/usr/bin/demo"
        );
    }
}
