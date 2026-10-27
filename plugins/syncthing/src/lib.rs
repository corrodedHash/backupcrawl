//! Bundled syncthing detector plugin.
//!
//! Answers the three questions of §3 of `docs/rust-rewrite-plan.md` for
//! syncthing folders. Syncthing is **replication, not archival backup**: a
//! folder that is in sync with a peer is still gone if both machines are lost,
//! so clean claims carry `note = "synced to cluster, not archived"`.
//!
//! * **Managed?** A directory is a syncthing folder root when it appears as a
//!   `<folder path=...>` in `config.xml` (and `.stfolder` marks it as valid).
//!   Paths outside every configured folder path are not claimed. A folder root
//!   is claimed once and pruned; when the crawl root itself lies inside a
//!   configured folder, the crawl root is claimed with that folder's state.
//! * **Current + propagated?** One `GET /rest/db/status?folder=<id>` query per
//!   folder per scan through the host `http` import (`X-API-Key`, loopback
//!   plain HTTP). `state = idle` with `needFiles = 0` and `pullErrors = 0` is
//!   `clean`; a busy state is `ahead`; `state = error`, `pullErrors > 0`,
//!   remote data still needed or a missing `.stfolder` marker is `dirty`.
//!   Receive-only divergence follows `receive-only-changes`.
//! * **Unknown?** A missing daemon, missing API key, non-2xx response or
//!   unparseable JSON yields `unknown` with a `reason`, never a silent
//!   `clean`. `assume-marker-only = true` explicitly opts into a marker-only
//!   `clean` with a visible note.
//!
//! # Config
//!
//! | Key | Default | Meaning |
//! | --- | --- | --- |
//! | `address` | `<gui><address>` / `127.0.0.1:8384` | REST base URL |
//! | `api-key` | `<gui><apikey>` | `X-API-Key`; without it the status is `unknown` |
//! | `require-daemon` | `true` | `false` skips the API and trusts the marker |
//! | `assume-marker-only` | `false` | API failure → marker-only `clean` |
//! | `receive-only-changes` | `dirty` | `dirty`, `ahead` or `clean` |
//! | `timeout-ms` | *(host limit)* | Parsed and reserved (host HTTP cap) |
//! | `config-dir` | auto-detect | Test/debug override for the config directory |
//!
//! Config discovery order: `config-dir`, `$STCONFDIR`,
//! `~/.local/state/syncthing`, `~/.config/syncthing`. The parsed folder list
//! and each folder's status are cached for the scan, so the expensive
//! `/rest/db/status` endpoint is queried once per folder (the API itself warns
//! that it is costly) and never per file.
//!
//! # Attributes
//!
//! Every claim carries `folder = <id>` and `state = <state>`. Clean claims add
//! `note`; non-clean claims add `reason`; a label/type from `config.xml` is
//! included when present.

mod config;
mod status;
mod xml;

use std::path::{Path, PathBuf};

use backupcrawl_plugin_sdk::{
    Claim, Detector, DetectorError, FileKind, Host, LogLevel, ScanInfo, Status, bind_detector,
};

use config::SyncthingConfig;
use xml::ConfigFile;
use xml::{ConfigLocation, FolderEntry};

/// The note attached to every clean folder claim.
pub const CLEAN_NOTE: &str = "synced to cluster, not archived";

/// One cached folder verdict, keyed by the configured folder path.
#[derive(Debug, Clone)]
struct CachedClaim {
    folder_path: PathBuf,
    claim: Claim,
}

/// The syncthing detector's mutable state (one instance per wasm store).
#[derive(Debug, Default)]
pub struct Syncthing {
    /// Validated `[plugins.syncthing]` config.
    config: SyncthingConfig,
    /// Normalized crawl root, used for the ancestor claim.
    root: PathBuf,
    /// Configured folders from `config.xml`.
    folders: Vec<FolderEntry>,
    /// Where the config came from (for the debug log).
    config_location: Option<ConfigLocation>,
    /// REST base URL and API key after applying overrides.
    address: String,
    api_key: Option<String>,
    /// Per-scan status cache (one daemon call per folder).
    cache: Vec<CachedClaim>,
}

impl Syncthing {
    /// The index of the configured folder a directory belongs to.
    ///
    /// An exact folder root always matches. A directory that is not a root is
    /// only considered when it is the crawl root, in which case the most
    /// specific configured ancestor wins (the folder root itself is outside the
    /// walked tree).
    fn folder_for(&self, path: &Path) -> Option<usize> {
        if let Some(index) = self.folders.iter().position(|folder| folder.path == path) {
            return Some(index);
        }
        if path != self.root {
            return None;
        }
        self.folders
            .iter()
            .enumerate()
            .filter(|(_, folder)| path.starts_with(&folder.path))
            .max_by_key(|(_, folder)| folder.path.components().count())
            .map(|(index, _)| index)
    }

    /// The claim for a folder, querying the daemon at most once per scan.
    fn claim_for_folder(
        &mut self,
        index: usize,
        host: &mut dyn Host,
    ) -> Result<Claim, DetectorError> {
        let folder = self.folders[index].clone();
        if let Some(cached) = self
            .cache
            .iter()
            .find(|cached| cached.folder_path == folder.path)
        {
            return Ok(cached.claim.clone());
        }

        let claim = self.inspect_folder(&folder, host);
        self.cache.push(CachedClaim {
            folder_path: folder.path.clone(),
            claim: claim.clone(),
        });
        Ok(claim)
    }

    /// Builds the folder-root claim from the marker and the daemon.
    fn inspect_folder(&self, folder: &FolderEntry, host: &mut dyn Host) -> Claim {
        if !marker_present(folder, host) {
            return folder_claim(
                folder,
                Status::Dirty,
                "invalid",
                Some("folder marker .stfolder is missing; syncthing will not sync it".to_owned()),
                None,
                false,
            );
        }

        if !self.config.require_daemon {
            return folder_claim(
                folder,
                Status::Clean,
                "marker-only",
                Some("daemon checks disabled by `require-daemon = false`".to_owned()),
                Some(CLEAN_NOTE),
                false,
            );
        }

        let Some(api_key) = self.api_key.as_deref() else {
            return self.api_failure(
                folder,
                "no API key configured; set `[plugins.syncthing] api-key` or `<gui><apikey>`",
            );
        };

        match status::query(
            &self.address,
            api_key,
            &folder.id,
            self.config.timeout_ms,
            host,
        ) {
            Ok(state) => {
                let verdict = status::map(&state, self.config.receive_only_changes);
                let note = (verdict.status == Status::Clean).then_some(CLEAN_NOTE);
                folder_claim(
                    folder,
                    verdict.status,
                    &verdict.state,
                    verdict.reason,
                    note,
                    verdict.receive_only_changed,
                )
            }
            Err(reason) => self.api_failure(folder, &reason),
        }
    }

    /// The `unknown` (or opted-in marker-only `clean`) verdict for API failures.
    fn api_failure(&self, folder: &FolderEntry, reason: &str) -> Claim {
        if self.config.assume_marker_only {
            folder_claim(
                folder,
                Status::Clean,
                "marker-only",
                Some(reason.to_owned()),
                Some(&format!("{CLEAN_NOTE} (marker only: {reason})")),
                false,
            )
        } else {
            folder_claim(
                folder,
                Status::Unknown,
                "unavailable",
                Some(reason.to_owned()),
                None,
                false,
            )
        }
    }
}

impl Detector for Syncthing {
    fn init(&mut self, info: &ScanInfo, host: &mut dyn Host) -> Result<(), DetectorError> {
        self.config = SyncthingConfig::from_config(&info.config)?;
        self.root = xml::normalize(&info.root);
        self.cache.clear();

        let home = xml::home_dir(&info.user);
        let location = xml::resolve_config_dir(
            self.config.config_dir.as_deref(),
            xml::stconfdir().as_deref(),
            home.as_deref(),
        );
        let file = match &location {
            Some(location) => xml::load_config(&location.dir, home.as_deref())?,
            None => ConfigFile::default(),
        };

        self.address = status::canonical_address(
            self.config
                .address
                .as_deref()
                .or(file.gui_address.as_deref()),
        );
        self.api_key = self
            .config
            .api_key
            .clone()
            .or(file.api_key.clone())
            .filter(|key| !key.trim().is_empty());
        self.folders = file.folders;
        self.config_location = location;

        host.log(
            LogLevel::Debug,
            &format!(
                "syncthing: config={} folders={} address={} api-key={} require-daemon={} \
                 assume-marker-only={} receive-only-changes={}",
                self.config_location
                    .as_ref()
                    .map(|location| format!("{} ({})", location.dir.display(), location.source))
                    .unwrap_or_else(|| "(none)".to_owned()),
                self.folders.len(),
                self.address,
                if self.api_key.is_some() {
                    "set"
                } else {
                    "missing"
                },
                self.config.require_daemon,
                self.config.assume_marker_only,
                self.config.receive_only_changes.as_str(),
            ),
        );
        Ok(())
    }

    fn check_dir(
        &mut self,
        path: &Path,
        host: &mut dyn Host,
    ) -> Result<Option<Claim>, DetectorError> {
        let path = xml::normalize(path);
        let Some(index) = self.folder_for(&path) else {
            return Ok(None);
        };
        Ok(Some(self.claim_for_folder(index, host)?))
    }
}

/// Whether `<folder>/.stfolder` is an existing directory.
fn marker_present(folder: &FolderEntry, host: &mut dyn Host) -> bool {
    host.stat(&folder.path.join(".stfolder"))
        .is_ok_and(|info| info.kind == FileKind::Directory)
}

/// Builds the folder-root claim with the stable attribute set.
fn folder_claim(
    folder: &FolderEntry,
    status: Status,
    state: &str,
    reason: Option<String>,
    note: Option<&str>,
    receive_only_changed: bool,
) -> Claim {
    let mut claim = Claim::new(status)
        .attr("folder", folder.id.as_str())
        .attr("state", state)
        .prune();
    if let Some(label) = &folder.label {
        claim = claim.attr("label", label.as_str());
    }
    if let Some(folder_type) = &folder.folder_type {
        claim = claim.attr("type", folder_type.as_str());
    }
    if let Some(reason) = reason {
        claim = claim.attr("reason", reason);
    }
    if let Some(note) = note {
        claim = claim.attr("note", note);
    }
    if receive_only_changed {
        claim = claim.attr("receive-only-changed", "true");
    }
    claim
}

// Emits the component export glue for the canonical `plugin` world.
bind_detector!(Syncthing);

#[cfg(test)]
mod tests {
    use std::fs;

    use backupcrawl_plugin_sdk::{
        Attr, Config, FileInfo, HttpResponse, MockHost, ScanInfo, Status,
    };

    use super::*;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    struct Fixture {
        _dir: tempfile::TempDir,
        folder: PathBuf,
        config_dir: PathBuf,
    }

    impl Fixture {
        /// A config directory with one folder (`id`) rooted at a temp path.
        fn new(id: &str, gui: &str, extra_xml: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let folder = dir.path().join("folder");
            fs::create_dir_all(&folder).unwrap();
            let config_dir = dir.path().join("conf");
            fs::create_dir_all(&config_dir).unwrap();
            fs::write(
                config_dir.join("config.xml"),
                format!(
                    r#"<configuration>
                         <folder id="{id}" path="{}" label="Docs" type="sendreceive"/>
                         {extra_xml}
                         <gui><address>{gui}</address><apikey>xml-key</apikey></gui>
                       </configuration>"#,
                    folder.display()
                ),
            )
            .unwrap();
            Self {
                _dir: dir,
                folder,
                config_dir,
            }
        }

        fn scan_info(&self, extra: &[(&str, &str)]) -> ScanInfo {
            let mut attrs = vec![Attr::new(
                "config-dir",
                self.config_dir.display().to_string(),
            )];
            attrs.extend(extra.iter().map(|(key, value)| Attr::new(*key, *value)));
            ScanInfo {
                root: self.folder.clone(),
                user: "tester".to_owned(),
                follow_symlinks: false,
                config: Config::new(attrs),
            }
        }

        fn marker(&self) -> PathBuf {
            self.folder.join(".stfolder")
        }
    }

    fn dir_info() -> FileInfo {
        FileInfo {
            kind: FileKind::Directory,
            ..MockHost::file_info(0)
        }
    }

    fn json(body: &str) -> HttpResponse {
        HttpResponse {
            status: 200,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: body.as_bytes().to_vec(),
        }
    }

    fn attr<'a>(claim: &'a Claim, key: &str) -> Option<&'a str> {
        claim
            .attrs
            .iter()
            .find(|attr| attr.key == key)
            .map(|attr| attr.value.as_str())
    }

    /// Initializes a detector with a present marker and one staged API response.
    fn new_detector(
        fixture: &Fixture,
        config: &[(&str, &str)],
        response: &str,
    ) -> (Syncthing, MockHost) {
        let mut detector = Syncthing::default();
        let mut host = MockHost::new().with_stat(fixture.marker(), dir_info());
        host = host.with_http_response(json(response));
        detector
            .init(&fixture.scan_info(config), &mut host)
            .unwrap_or_else(|err| panic!("init: {err}"));
        (detector, host)
    }

    fn check(detector: &mut Syncthing, fixture: &Fixture, host: &mut MockHost) -> Claim {
        detector
            .check_dir(&fixture.folder, host)
            .unwrap_or_else(|err| panic!("check_dir: {err}"))
            .expect("a configured folder root must be claimed")
    }

    const CLEAN_STATUS: &str = r#"{"state":"idle","needFiles":0,"pullErrors":0}"#;

    // -----------------------------------------------------------------------
    // Config discovery and parsing
    // -----------------------------------------------------------------------

    #[test]
    fn init_without_a_config_file_has_no_folders() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        let mut detector = Syncthing::default();
        let mut host = MockHost::new();
        let info = ScanInfo {
            root: dir.path().to_path_buf(),
            user: "tester".to_owned(),
            follow_symlinks: false,
            config: Config::new(vec![Attr::new("config-dir", empty.display().to_string())]),
        };
        detector.init(&info, &mut host).unwrap();
        assert!(detector.folders.is_empty());
        assert_eq!(detector.check_dir(dir.path(), &mut host).unwrap(), None);
    }

    #[test]
    fn malformed_config_disables_the_plugin() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.xml"), "<configuration><folder").unwrap();
        let mut detector = Syncthing::default();
        let mut host = MockHost::new();
        let info = ScanInfo {
            root: dir.path().to_path_buf(),
            user: "tester".to_owned(),
            follow_symlinks: false,
            config: Config::new(vec![Attr::new(
                "config-dir",
                dir.path().display().to_string(),
            )]),
        };
        let err = detector.init(&info, &mut host).unwrap_err();
        assert!(err.message().contains("config.xml"), "{err}");
    }

    #[test]
    fn address_and_api_key_overrides_take_precedence() {
        let fixture = Fixture::new("docs", "http://127.0.0.1:9999", "");
        let (mut detector, mut host) = new_detector(
            &fixture,
            &[
                ("address", "http://127.0.0.1:7777"),
                ("api-key", "override-key"),
            ],
            CLEAN_STATUS,
        );
        let claim = check(&mut detector, &fixture, &mut host);
        assert_eq!(claim.status, Status::Clean);

        let requests = host.http_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url,
            "http://127.0.0.1:7777/rest/db/status?folder=docs"
        );
        assert_eq!(
            requests[0].headers,
            vec![("X-API-Key".to_owned(), "override-key".to_owned())]
        );
    }

    #[test]
    fn gui_address_and_key_are_used_without_overrides() {
        let fixture = Fixture::new("docs", "127.0.0.1:8384", "");
        let (mut detector, mut host) = new_detector(&fixture, &[], CLEAN_STATUS);
        check(&mut detector, &fixture, &mut host);

        let requests = host.http_requests();
        assert_eq!(
            requests[0].url,
            "http://127.0.0.1:8384/rest/db/status?folder=docs"
        );
        assert_eq!(
            requests[0].headers,
            vec![("X-API-Key".to_owned(), "xml-key".to_owned())]
        );
    }

    // -----------------------------------------------------------------------
    // Managed detection and markers
    // -----------------------------------------------------------------------

    #[test]
    fn paths_outside_configured_folders_are_not_claimed() {
        let fixture = Fixture::new("docs", "127.0.0.1:8384", "");
        let (mut detector, mut host) = new_detector(&fixture, &[], CLEAN_STATUS);

        assert_eq!(
            detector
                .check_dir(&fixture.folder.join("nested"), &mut host)
                .unwrap(),
            None
        );
        assert_eq!(
            detector
                .check_dir(&fixture._dir.path().join("elsewhere"), &mut host)
                .unwrap(),
            None
        );
        assert!(host.http_requests().is_empty());
    }

    #[test]
    fn missing_marker_is_dirty_and_does_not_call_the_daemon() {
        let fixture = Fixture::new("docs", "127.0.0.1:8384", "");
        let mut detector = Syncthing::default();
        // No stat staged: the marker is missing.
        let mut host = MockHost::new();
        detector.init(&fixture.scan_info(&[]), &mut host).unwrap();

        let claim = check(&mut detector, &fixture, &mut host);
        assert_eq!(claim.status, Status::Dirty);
        assert_eq!(attr(&claim, "state"), Some("invalid"));
        assert_eq!(attr(&claim, "folder"), Some("docs"));
        assert!(attr(&claim, "reason").unwrap().contains(".stfolder"));
        assert!(host.http_requests().is_empty());
    }

    #[test]
    fn marker_must_be_a_directory() {
        let fixture = Fixture::new("docs", "127.0.0.1:8384", "");
        let mut detector = Syncthing::default();
        let mut host = MockHost::new().with_stat(fixture.marker(), MockHost::file_info(0));
        detector.init(&fixture.scan_info(&[]), &mut host).unwrap();
        let claim = check(&mut detector, &fixture, &mut host);
        assert_eq!(claim.status, Status::Dirty);
    }

    #[test]
    fn crawl_root_inside_a_folder_uses_the_ancestor_claim() {
        let fixture = Fixture::new("docs", "127.0.0.1:8384", "");
        let nested = fixture.folder.join("nested");
        fs::create_dir_all(&nested).unwrap();
        let mut detector = Syncthing::default();
        let mut host = MockHost::new()
            .with_stat(fixture.marker(), dir_info())
            .with_http_response(json(CLEAN_STATUS));
        let mut info = fixture.scan_info(&[]);
        info.root = nested.clone();
        detector.init(&info, &mut host).unwrap();

        let claim = detector
            .check_dir(&nested, &mut host)
            .unwrap()
            .expect("ancestor folder claim");
        assert_eq!(claim.status, Status::Clean);
        assert_eq!(attr(&claim, "folder"), Some("docs"));
        assert!(claim.prune);
    }

    // -----------------------------------------------------------------------
    // Status mapping through the detector
    // -----------------------------------------------------------------------

    #[test]
    fn clean_folder_has_folder_state_note_and_prunes() {
        let fixture = Fixture::new("docs", "127.0.0.1:8384", "");
        let (mut detector, mut host) = new_detector(&fixture, &[], CLEAN_STATUS);
        let claim = check(&mut detector, &fixture, &mut host);

        assert_eq!(claim.status, Status::Clean);
        assert!(claim.prune);
        assert_eq!(attr(&claim, "folder"), Some("docs"));
        assert_eq!(attr(&claim, "state"), Some("idle"));
        assert_eq!(attr(&claim, "note"), Some(CLEAN_NOTE));
        assert_eq!(attr(&claim, "label"), Some("Docs"));
        assert_eq!(attr(&claim, "type"), Some("sendreceive"));
        assert_eq!(attr(&claim, "reason"), None);
    }

    #[test]
    fn status_is_cached_for_the_scan() {
        let fixture = Fixture::new("docs", "127.0.0.1:8384", "");
        let (mut detector, mut host) = new_detector(&fixture, &[], CLEAN_STATUS);
        check(&mut detector, &fixture, &mut host);
        check(&mut detector, &fixture, &mut host);
        assert_eq!(host.http_requests().len(), 1);
    }

    #[test]
    fn syncing_folder_is_ahead() {
        let fixture = Fixture::new("docs", "127.0.0.1:8384", "");
        let (mut detector, mut host) = new_detector(
            &fixture,
            &[],
            r#"{"state":"syncing","needFiles":0,"pullErrors":0}"#,
        );
        let claim = check(&mut detector, &fixture, &mut host);
        assert_eq!(claim.status, Status::Ahead);
        assert_eq!(attr(&claim, "state"), Some("syncing"));
        assert!(attr(&claim, "reason").unwrap().contains("not propagated"));
    }

    #[test]
    fn error_state_and_pull_errors_are_dirty() {
        let fixture = Fixture::new("docs", "127.0.0.1:8384", "");
        let (mut detector, mut host) = new_detector(
            &fixture,
            &[],
            r#"{"state":"error","needFiles":0,"pullErrors":0}"#,
        );
        assert_eq!(
            check(&mut detector, &fixture, &mut host).status,
            Status::Dirty
        );

        let fixture = Fixture::new("docs", "127.0.0.1:8384", "");
        let (mut detector, mut host) = new_detector(
            &fixture,
            &[],
            r#"{"state":"idle","needFiles":0,"pullErrors":2}"#,
        );
        let claim = check(&mut detector, &fixture, &mut host);
        assert_eq!(claim.status, Status::Dirty);
        assert!(attr(&claim, "reason").unwrap().contains("pull error"));
    }

    #[test]
    fn missing_remote_data_is_dirty() {
        let fixture = Fixture::new("docs", "127.0.0.1:8384", "");
        let (mut detector, mut host) = new_detector(
            &fixture,
            &[],
            r#"{"state":"idle","needFiles":4,"pullErrors":0}"#,
        );
        let claim = check(&mut detector, &fixture, &mut host);
        assert_eq!(claim.status, Status::Dirty);
        assert!(attr(&claim, "reason").unwrap().contains("4 file"));
    }

    #[test]
    fn receive_only_changes_follow_the_config() {
        let response =
            r#"{"state":"idle","needFiles":0,"pullErrors":0,"receiveOnlyChangedFiles":1}"#;

        let fixture = Fixture::new("docs", "127.0.0.1:8384", "");
        let (mut detector, mut host) = new_detector(&fixture, &[], response);
        let claim = check(&mut detector, &fixture, &mut host);
        assert_eq!(claim.status, Status::Dirty);
        assert_eq!(attr(&claim, "receive-only-changed"), Some("true"));

        let fixture = Fixture::new("docs", "127.0.0.1:8384", "");
        let (mut detector, mut host) =
            new_detector(&fixture, &[("receive-only-changes", "ahead")], response);
        assert_eq!(
            check(&mut detector, &fixture, &mut host).status,
            Status::Ahead
        );

        let fixture = Fixture::new("docs", "127.0.0.1:8384", "");
        let (mut detector, mut host) =
            new_detector(&fixture, &[("receive-only-changes", "clean")], response);
        let claim = check(&mut detector, &fixture, &mut host);
        assert_eq!(claim.status, Status::Clean);
        assert_eq!(attr(&claim, "note"), Some(CLEAN_NOTE));
    }

    // -----------------------------------------------------------------------
    // Daemon failures
    // -----------------------------------------------------------------------

    #[test]
    fn unreachable_daemon_is_unknown_with_a_reason() {
        let fixture = Fixture::new("docs", "127.0.0.1:8384", "");
        let mut detector = Syncthing::default();
        let mut host = MockHost::new()
            .with_stat(fixture.marker(), dir_info())
            .with_http_error("connection refused");
        detector.init(&fixture.scan_info(&[]), &mut host).unwrap();

        let claim = check(&mut detector, &fixture, &mut host);
        assert_eq!(claim.status, Status::Unknown);
        assert_eq!(attr(&claim, "state"), Some("unavailable"));
        assert!(attr(&claim, "reason").unwrap().contains("unreachable"));
        assert_eq!(attr(&claim, "note"), None);
    }

    #[test]
    fn assume_marker_only_turns_a_failure_into_a_noted_clean() {
        let fixture = Fixture::new("docs", "127.0.0.1:8384", "");
        let mut detector = Syncthing::default();
        let mut host = MockHost::new()
            .with_stat(fixture.marker(), dir_info())
            .with_http_error("connection refused");
        detector
            .init(
                &fixture.scan_info(&[("assume-marker-only", "true")]),
                &mut host,
            )
            .unwrap();

        let claim = check(&mut detector, &fixture, &mut host);
        assert_eq!(claim.status, Status::Clean);
        assert_eq!(attr(&claim, "state"), Some("marker-only"));
        let note = attr(&claim, "note").unwrap();
        assert!(note.contains(CLEAN_NOTE), "{note}");
        assert!(note.contains("marker only"), "{note}");
        assert!(attr(&claim, "reason").unwrap().contains("unreachable"));
    }

    #[test]
    fn non_2xx_is_unknown_and_assume_marker_only_applies() {
        let fixture = Fixture::new("docs", "127.0.0.1:8384", "");
        let mut detector = Syncthing::default();
        let mut host = MockHost::new()
            .with_stat(fixture.marker(), dir_info())
            .with_http_response(HttpResponse {
                status: 403,
                headers: Vec::new(),
                body: Vec::new(),
            });
        detector.init(&fixture.scan_info(&[]), &mut host).unwrap();
        let claim = check(&mut detector, &fixture, &mut host);
        assert_eq!(claim.status, Status::Unknown);
        assert!(attr(&claim, "reason").unwrap().contains("HTTP 403"));

        let fixture = Fixture::new("docs", "127.0.0.1:8384", "");
        let mut detector = Syncthing::default();
        let mut host = MockHost::new()
            .with_stat(fixture.marker(), dir_info())
            .with_http_response(HttpResponse {
                status: 403,
                headers: Vec::new(),
                body: Vec::new(),
            });
        detector
            .init(
                &fixture.scan_info(&[("assume-marker-only", "true")]),
                &mut host,
            )
            .unwrap();
        assert_eq!(
            check(&mut detector, &fixture, &mut host).status,
            Status::Clean
        );
    }

    #[test]
    fn missing_api_key_is_unknown_without_a_request() {
        let fixture = Fixture::new("docs", "127.0.0.1:8384", "");
        let mut host = MockHost::new().with_stat(fixture.marker(), dir_info());
        // Remove the key the fixture wrote.
        fs::write(
            fixture.config_dir.join("config.xml"),
            format!(
                r#"<configuration><folder id="docs" path="{}"/><gui><address>127.0.0.1:8384</address></gui></configuration>"#,
                fixture.folder.display()
            ),
        )
        .unwrap();
        let mut detector = Syncthing::default();
        detector.init(&fixture.scan_info(&[]), &mut host).unwrap();
        let claim = check(&mut detector, &fixture, &mut host);
        assert_eq!(claim.status, Status::Unknown);
        assert!(attr(&claim, "reason").unwrap().contains("no API key"));
        assert!(host.http_requests().is_empty());
    }

    #[test]
    fn require_daemon_false_trusts_the_marker() {
        let fixture = Fixture::new("docs", "127.0.0.1:8384", "");
        let mut detector = Syncthing::default();
        let mut host = MockHost::new().with_stat(fixture.marker(), dir_info());
        detector
            .init(
                &fixture.scan_info(&[("require-daemon", "false")]),
                &mut host,
            )
            .unwrap();
        let claim = check(&mut detector, &fixture, &mut host);
        assert_eq!(claim.status, Status::Clean);
        assert_eq!(attr(&claim, "state"), Some("marker-only"));
        assert!(host.http_requests().is_empty());
    }
}
