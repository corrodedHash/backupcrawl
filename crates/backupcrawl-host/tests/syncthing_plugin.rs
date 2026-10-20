//! M3.2 golden tests: the bundled `syncthing` detector component driven against
//! a local fake REST daemon and real folder trees.
//!
//! The fake daemon is a `TcpListener` on `127.0.0.1:0`; the test origin is
//! granted explicitly with [`CapabilityPolicy::with_http`] because the
//! manifest's default origin (`127.0.0.1:8384`) cannot match an ephemeral port.
//! Filesystem content is granted with [`FsGrants::from_roots`] for the fixture
//! directory (config + folders).
//!
//! Covered: clean, syncing, error, unreachable daemon and a missing `.stfolder`
//! marker, plus the request shape (`GET /rest/db/status?folder=...`,
//! `X-API-Key`).

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use backupcrawl_core::Status;
use backupcrawl_host::manifest::Manifest;
use backupcrawl_host::{
    CapabilityPolicy, Claim, FsGrants, Limits, Runtime, ScanInfo, WasmDetector, bundled,
};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// One runtime for the whole test binary, sharing the workspace component cache
/// while isolating the per-plugin KV directory.
fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let base = std::env::temp_dir().join("backupcrawl-test-cache");
        Runtime::with_cache_dirs(
            Limits::default(),
            base.join("syncthing-kv"),
            base.join("backupcrawl/plugins/_components"),
        )
        .expect("runtime")
    })
}

fn plugin() -> &'static bundled::BundledPlugin {
    bundled::all()
        .iter()
        .find(|plugin| plugin.name == "syncthing")
        .expect("syncthing must be in the bundled registry")
}

fn detector(fixture: &Fixture, origin: &str) -> WasmDetector {
    let manifest = Manifest::parse(plugin().manifest).expect("bundled syncthing manifest");
    let policy = CapabilityPolicy::from_manifest(&manifest).with_http([format!("http://{origin}")]);
    let grants = FsGrants::from_roots([fixture._dir.path()]);
    let mut detector = runtime()
        .instantiate_with_fs(plugin().wasm(), "syncthing", policy, &grants)
        .expect("instantiate syncthing component");
    detector
        .init(&ScanInfo {
            root: fixture.folder.clone(),
            user: "tester".to_owned(),
            follow_symlinks: false,
            config: vec![(
                "config-dir".to_owned(),
                fixture.config_dir.display().to_string(),
            )],
        })
        .unwrap_or_else(|err| panic!("init syncthing detector: {err}"));
    detector
}

fn attr<'a>(claim: &'a Claim, key: &str) -> Option<&'a str> {
    claim
        .attrs
        .iter()
        .find(|attr| attr.key == key)
        .map(|attr| attr.value.as_str())
}

// ---------------------------------------------------------------------------
// Fixture and fake daemon
// ---------------------------------------------------------------------------

struct Fixture {
    /// Kept alive for the fixture's lifetime.
    _dir: tempfile::TempDir,
    folder: PathBuf,
    config_dir: PathBuf,
}

impl Fixture {
    fn new(address: &str, marker: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("docs");
        fs::create_dir_all(&folder).unwrap();
        if marker {
            fs::create_dir_all(folder.join(".stfolder")).unwrap();
        }
        let config_dir = dir.path().join("conf");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("config.xml"),
            format!(
                r#"<configuration>
                     <folder id="docs" path="{}" label="Docs" type="sendreceive"/>
                     <gui><address>{address}</address><apikey>secret-key</apikey></gui>
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
}

struct FakeDaemon {
    address: SocketAddr,
    handle: JoinHandle<Vec<String>>,
}

impl FakeDaemon {
    /// Serves one response per entry and returns the captured request heads.
    fn serve(responses: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut raw = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    let read = stream.read(&mut chunk).unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    raw.extend_from_slice(&chunk[..read]);
                    if raw.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                requests.push(String::from_utf8_lossy(&raw).into_owned());
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().unwrap();
            }
            requests
        });
        Self { address, handle }
    }

    fn join(self) -> Vec<String> {
        self.handle.join().expect("fake daemon thread")
    }
}

fn http_json(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

/// A loopback address with nothing listening on it.
fn unused_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    address
}

// ---------------------------------------------------------------------------
// Golden cases
// ---------------------------------------------------------------------------

#[test]
fn clean_folder_reports_synced_to_cluster_not_archived() {
    let daemon = FakeDaemon::serve(vec![http_json(
        r#"{"state":"idle","needFiles":0,"pullErrors":0}"#,
    )]);
    let fixture = Fixture::new(&daemon.address.to_string(), true);
    let mut detector = detector(&fixture, &daemon.address.to_string());

    let claim = detector
        .check_dir(&fixture.folder)
        .unwrap()
        .expect("clean syncthing folder claim");
    assert_eq!(claim.status, Status::Clean);
    assert!(claim.prune);
    assert_eq!(attr(&claim, "folder"), Some("docs"));
    assert_eq!(attr(&claim, "state"), Some("idle"));
    assert_eq!(
        attr(&claim, "note"),
        Some("synced to cluster, not archived")
    );

    let requests = daemon.join();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].starts_with("GET /rest/db/status?folder=docs HTTP/1.1\r\n"),
        "request head:\n{}",
        requests[0]
    );
    assert!(
        requests[0]
            .to_ascii_lowercase()
            .contains("x-api-key: secret-key"),
        "request head:\n{}",
        requests[0]
    );
}

#[test]
fn syncing_folder_is_ahead() {
    let daemon = FakeDaemon::serve(vec![http_json(
        r#"{"state":"syncing","needFiles":0,"pullErrors":0}"#,
    )]);
    let fixture = Fixture::new(&daemon.address.to_string(), true);
    let mut detector = detector(&fixture, &daemon.address.to_string());

    let claim = detector
        .check_dir(&fixture.folder)
        .unwrap()
        .expect("syncing folder claim");
    assert_eq!(claim.status, Status::Ahead);
    assert_eq!(attr(&claim, "state"), Some("syncing"));
    daemon.join();
}

#[test]
fn error_folder_is_dirty() {
    let daemon = FakeDaemon::serve(vec![http_json(
        r#"{"state":"error","needFiles":0,"pullErrors":0}"#,
    )]);
    let fixture = Fixture::new(&daemon.address.to_string(), true);
    let mut detector = detector(&fixture, &daemon.address.to_string());

    let claim = detector
        .check_dir(&fixture.folder)
        .unwrap()
        .expect("error folder claim");
    assert_eq!(claim.status, Status::Dirty);
    assert_eq!(attr(&claim, "state"), Some("error"));
    assert!(attr(&claim, "reason").unwrap().contains("error"));
    daemon.join();
}

#[test]
fn unreachable_daemon_is_unknown_not_clean() {
    let address = unused_address();
    let fixture = Fixture::new(&address.to_string(), true);
    let mut detector = detector(&fixture, &address.to_string());

    let claim = detector
        .check_dir(&fixture.folder)
        .unwrap()
        .expect("unreachable folder claim");
    assert_eq!(claim.status, Status::Unknown);
    assert_eq!(attr(&claim, "state"), Some("unavailable"));
    assert!(
        attr(&claim, "reason").unwrap().contains("unreachable"),
        "claim: {claim:?}"
    );
    assert_eq!(attr(&claim, "note"), None);
}

#[test]
fn missing_marker_is_dirty_without_a_daemon_call() {
    let address = unused_address();
    let fixture = Fixture::new(&address.to_string(), false);
    let mut detector = detector(&fixture, &address.to_string());

    let claim = detector
        .check_dir(&fixture.folder)
        .unwrap()
        .expect("marker-missing folder claim");
    assert_eq!(claim.status, Status::Dirty);
    assert_eq!(attr(&claim, "state"), Some("invalid"));
    assert!(attr(&claim, "reason").unwrap().contains(".stfolder"));
}

#[test]
fn folder_outside_the_config_is_not_claimed() {
    let address = unused_address();
    let fixture = Fixture::new(&address.to_string(), true);
    let mut detector = detector(&fixture, &address.to_string());

    assert_eq!(
        detector
            .check_dir(Path::new("/definitely/not/a/configured/folder"))
            .unwrap(),
        None
    );
}
