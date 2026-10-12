//! Example detector built with `backupcrawl-plugin-sdk`.
//!
//! It is a tiny, self-contained detector used to demonstrate (and test) the
//! SDK's authoring story:
//!
//! - the logic lives in `impl Detector for Dotbak` and talks to native records
//!   plus the [`Host`] service trait,
//! - [`bind_detector!`] emits the wasm component export glue for the canonical
//!   `plugin` world,
//! - the unit tests below run under plain `cargo test` with a [`MockHost`],
//!   so no wasm toolchain is needed to test detector logic.
//!
//! Semantics: a directory containing a marker file (default `.dotbak`) is a
//! managed directory and is claimed `clean` with `prune`; files ending in a
//! backup suffix (default `.bak`) are claimed `dirty`; a dangling symlink is
//! claimed `dirty`. `init` reads the plugin config table, counts invocations in
//! the persistent cache and logs its configuration.
//!
//! [`Host`]: backupcrawl_plugin_sdk::Host
//! [`MockHost`]: backupcrawl_plugin_sdk::MockHost

use std::path::Path;

use backupcrawl_plugin_sdk::{
    Claim, Detector, DetectorError, FileKind, Host, LinkInfo, LogLevel, ScanInfo, bind_detector,
};

/// Marker file that marks a managed directory when the config omits one.
pub const DEFAULT_MARKER: &str = ".dotbak";
/// Backup suffix claimed `dirty` when the config omits one.
pub const DEFAULT_SUFFIX: &str = ".bak";
/// Persistent cache key used to count `init` invocations.
pub const CACHE_KEY: &str = "dotbak/init-count";

/// The example detector's state.
#[derive(Debug)]
pub struct Dotbak {
    marker: String,
    suffix: String,
    init_count: u64,
}

impl Default for Dotbak {
    fn default() -> Self {
        Self {
            marker: DEFAULT_MARKER.to_owned(),
            suffix: DEFAULT_SUFFIX.to_owned(),
            init_count: 0,
        }
    }
}

impl Dotbak {
    /// Whether `path` contains a regular marker file.
    fn is_marker(&self, path: &Path, host: &mut dyn Host) -> bool {
        matches!(
            host.stat(&path.join(&self.marker)),
            Ok(info) if info.kind == FileKind::File
        )
    }

    /// Whether `path` looks like a backup file.
    fn is_backup(&self, path: &Path) -> bool {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(&self.suffix))
    }

    /// Number of times `init` has run against the persistent cache.
    pub fn init_count(&self) -> u64 {
        self.init_count
    }
}

impl Detector for Dotbak {
    fn init(&mut self, info: &ScanInfo, host: &mut dyn Host) -> Result<(), DetectorError> {
        self.marker = info
            .config
            .get("marker")
            .unwrap_or(DEFAULT_MARKER)
            .to_owned();
        self.suffix = info
            .config
            .get("suffix")
            .unwrap_or(DEFAULT_SUFFIX)
            .to_owned();

        let previous = host
            .cache_get(CACHE_KEY)
            .map_or(0, |bytes| bytes.len() as u64);
        self.init_count = previous + 1;
        host.cache_set(CACHE_KEY, &self.init_count.to_le_bytes())?;
        host.log(
            LogLevel::Info,
            &format!("dotbak: marker={} suffix={}", self.marker, self.suffix),
        );
        Ok(())
    }

    fn check_dir(
        &mut self,
        path: &Path,
        host: &mut dyn Host,
    ) -> Result<Option<Claim>, DetectorError> {
        if self.is_marker(path, host) {
            host.log(
                LogLevel::Debug,
                &format!("dotbak: managed directory {}", path.display()),
            );
            Ok(Some(
                Claim::clean().attr("marker", self.marker.clone()).prune(),
            ))
        } else {
            Ok(None)
        }
    }

    fn check_file(
        &mut self,
        path: &Path,
        _host: &mut dyn Host,
    ) -> Result<Option<Claim>, DetectorError> {
        if self.is_backup(path) {
            Ok(Some(Claim::dirty().attr("reason", "backup-suffix")))
        } else {
            Ok(None)
        }
    }

    fn check_symlink(
        &mut self,
        link: &LinkInfo,
        _host: &mut dyn Host,
    ) -> Result<Option<Claim>, DetectorError> {
        if link.dangling {
            Ok(Some(Claim::dirty().attr("reason", "dangling-symlink")))
        } else {
            Ok(None)
        }
    }
}

// Emits `wit_bindgen::generate!` for the canonical `plugin` world plus the
// export glue. This compiles natively too (the imported host functions are
// `unreachable!` stubs), so `cargo test`/clippy exercise the macro output.
bind_detector!(Dotbak);

#[cfg(test)]
mod tests {
    use super::*;
    use backupcrawl_plugin_sdk::{Attr, Config, FileInfo, MockHost, Status};
    use std::path::PathBuf;

    fn scan_info(attrs: &[(&str, &str)]) -> ScanInfo {
        ScanInfo {
            root: PathBuf::from("/data"),
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

    fn regular_file() -> FileInfo {
        FileInfo {
            kind: FileKind::File,
            size: 0,
            mtime_ms: 0,
            mode: 0o644,
            uid: 0,
            gid: 0,
            nlink: 1,
        }
    }

    #[test]
    fn init_reads_config_and_count_persists_in_cache() {
        let mut detector = Dotbak::default();
        let mut host = MockHost::new().with_cache(CACHE_KEY, vec![0u8; 4]);

        detector
            .init(
                &scan_info(&[("marker", ".mymark"), ("suffix", ".old")]),
                &mut host,
            )
            .unwrap();

        assert_eq!(detector.marker, ".mymark");
        assert_eq!(detector.suffix, ".old");
        assert_eq!(detector.init_count(), 5);
        assert_eq!(
            host.cache().get(CACHE_KEY),
            Some(&5u64.to_le_bytes().to_vec())
        );
        assert_eq!(host.logs().len(), 1);
        assert_eq!(host.logs()[0].0, LogLevel::Info);
    }

    #[test]
    fn marked_directory_is_clean_and_pruned() {
        let mut detector = Dotbak::default();
        let mut host = MockHost::new().with_stat("/data/proj/.dotbak", regular_file());

        let claim = detector
            .check_dir(Path::new("/data/proj"), &mut host)
            .unwrap()
            .expect("marked directory is claimed");

        assert_eq!(claim.status, Status::Clean);
        assert!(claim.prune);
        assert_eq!(claim.attrs, vec![Attr::new("marker", ".dotbak")]);
    }

    #[test]
    fn unmarked_directory_is_unclaimed() {
        let mut detector = Dotbak::default();
        // Nothing staged: `stat` fails, which the detector treats as "no marker".
        let mut host = MockHost::new();

        assert_eq!(
            detector
                .check_dir(Path::new("/data/other"), &mut host)
                .unwrap(),
            None
        );
        assert!(host.logs().is_empty());
    }

    #[test]
    fn backup_files_are_dirty_and_others_unclaimed() {
        let mut detector = Dotbak::default();
        let mut host = MockHost::new();

        let dirty = detector
            .check_file(Path::new("/data/notes.txt.bak"), &mut host)
            .unwrap()
            .expect("backup file is claimed");
        assert_eq!(dirty.status, Status::Dirty);
        assert_eq!(dirty.attrs, vec![Attr::new("reason", "backup-suffix")]);

        assert_eq!(
            detector
                .check_file(Path::new("/data/notes.txt"), &mut host)
                .unwrap(),
            None
        );
    }

    #[test]
    fn dangling_symlink_is_dirty() {
        let mut detector = Dotbak::default();
        let mut host = MockHost::new();
        let link = LinkInfo {
            path: PathBuf::from("/data/link"),
            target: PathBuf::from("/data/gone"),
            dangling: true,
        };

        let claim = detector
            .check_symlink(&link, &mut host)
            .unwrap()
            .expect("dangling link is claimed");
        assert_eq!(claim.status, Status::Dirty);
        assert_eq!(claim.attrs, vec![Attr::new("reason", "dangling-symlink")]);
    }

    #[test]
    fn default_check_files_matches_singular_calls() {
        let mut detector = Dotbak::default();
        let mut host = MockHost::new();
        let paths = [
            PathBuf::from("/data/a.bak"),
            PathBuf::from("/data/a.txt"),
            PathBuf::from("/data/b.bak"),
        ];

        let bulk = detector.check_files(&paths, &mut host).unwrap();
        let singular = paths
            .iter()
            .map(|path| detector.check_file(path, &mut host).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(bulk, singular);
    }
}
