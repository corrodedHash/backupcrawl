//! M1.4 acceptance: the wasmtime runtime, host imports and detector lifecycle.
//!
//! These tests run the checked-in fixture component
//! (`tests/fixtures/test-detector.wasm`) through the real [`Runtime`] and
//! [`WasmDetector`], covering successful init/check/finish, disabled-on-init-error,
//! traps, fuel exhaustion and cancellation, and every host import.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use backupcrawl_core::Status;
use backupcrawl_host::{
    CapabilityPolicy, Claim, DetectorError, Limits, LinkInfo, Runtime, ScanInfo, WasmDetector,
};

fn fixture_bytes() -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/test-detector.wasm"),
    )
    .expect("checked-in fixture component")
}

/// The shared wasmtime component cache, matching the one the CLI tests and the
/// git-plugin tests use, so the fixture component is compiled once for the whole
/// workspace instead of once per test.
fn shared_component_cache() -> PathBuf {
    std::env::temp_dir()
        .join("backupcrawl-test-cache")
        .join("backupcrawl")
        .join("plugins")
        .join("_components")
}

/// A runtime with an isolated per-plugin cache directory (tempdir kept alive by
/// the caller) but the shared component cache.
fn runtime(limits: Limits) -> (Runtime, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let runtime =
        Runtime::with_cache_dirs(limits, dir.path(), shared_component_cache()).expect("runtime");
    (runtime, dir)
}

fn scan_info(config: &[(&str, &str)]) -> ScanInfo {
    ScanInfo {
        root: PathBuf::from("/"),
        user: "tester".to_owned(),
        follow_symlinks: false,
        config: config
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect(),
    }
}

/// Instantiates the fixture with `policy` and runs `init`.
fn detector(runtime: &Runtime, policy: CapabilityPolicy, config: &[(&str, &str)]) -> WasmDetector {
    let mut detector = runtime
        .instantiate(&fixture_bytes(), "fixture", policy)
        .expect("instantiate fixture");
    detector
        .init(&scan_info(config))
        .unwrap_or_else(|err| panic!("init fixture: {err}"));
    detector
}

fn attr<'a>(claim: &'a Claim, key: &str) -> Option<&'a str> {
    claim
        .attrs
        .iter()
        .find(|attr| attr.key == key)
        .map(|attr| attr.value.as_str())
}

#[test]
fn successful_lifecycle() {
    let (runtime, _dir) = runtime(Limits::default());
    let mut detector = detector(&runtime, CapabilityPolicy::none(), &[]);

    assert_eq!(detector.check_dir(Path::new("/x")).unwrap(), None);
    assert_eq!(detector.check_file(Path::new("/x/f")).unwrap(), None);
    assert_eq!(
        detector
            .check_symlink(&LinkInfo {
                path: PathBuf::from("/l"),
                target: PathBuf::from("/x/f"),
                dangling: false,
            })
            .unwrap(),
        None
    );
    assert_eq!(
        detector
            .check_files(&[PathBuf::from("/a"), PathBuf::from("/b")])
            .unwrap(),
        vec![None, None]
    );

    detector.finish().unwrap();
    let report = detector.report();
    assert_eq!(report.errors, 0);
    assert!(report.disabled.is_none());
    assert!(report.calls >= 6);
}

#[test]
fn init_error_disables_the_plugin() {
    let (runtime, _dir) = runtime(Limits::default());
    let mut detector = runtime
        .instantiate(&fixture_bytes(), "fixture", CapabilityPolicy::none())
        .unwrap();

    let error = detector
        .init(&scan_info(&[("scenario", "init-error")]))
        .expect_err("init must fail");
    assert!(matches!(error, DetectorError::Disabled { .. }));
    assert!(detector.is_disabled());
    assert!(
        detector
            .disable_reason()
            .expect("disable reason")
            .contains("init failed")
    );

    // Further calls are refused, and finishing is a no-op.
    assert!(matches!(
        detector.check_file(Path::new("/x")).unwrap_err(),
        DetectorError::Disabled { .. }
    ));
    detector.finish().unwrap();

    let report = detector.report();
    assert_eq!(
        report.disabled,
        detector.disable_reason().map(str::to_owned)
    );
}

#[test]
fn trap_becomes_no_claim() {
    let (runtime, _dir) = runtime(Limits::default());
    let mut detector = detector(&runtime, CapabilityPolicy::none(), &[("scenario", "trap")]);

    assert_eq!(detector.check_file(Path::new("/x")).unwrap(), None);
    assert_eq!(detector.errors(), 1);
    // The instance survives a trap and keeps answering.
    assert_eq!(detector.check_file(Path::new("/y")).unwrap(), None);
    assert_eq!(detector.errors(), 2);
}

#[test]
fn guest_errors_trip_the_circuit_breaker() {
    let limits = Limits {
        max_consecutive_failures: 3,
        ..Limits::default()
    };
    let (runtime, _dir) = runtime(limits);
    let mut detector = detector(&runtime, CapabilityPolicy::none(), &[("scenario", "error")]);

    for _ in 0..3 {
        assert_eq!(detector.check_file(Path::new("/x")).unwrap(), None);
    }
    assert!(detector.is_disabled());
    assert_eq!(detector.errors(), 3);

    // Once disabled, the detector refuses to be asked again.
    assert!(matches!(
        detector.check_file(Path::new("/x")).unwrap_err(),
        DetectorError::Disabled { .. }
    ));
}

#[test]
fn fuel_exhaustion_becomes_no_claim() {
    let limits = Limits {
        fuel_per_call: 1_000_000,
        ..Limits::default()
    };
    let (runtime, _dir) = runtime(limits);
    let mut detector = detector(&runtime, CapabilityPolicy::none(), &[("scenario", "loop")]);

    let started = Instant::now();
    assert_eq!(detector.check_file(Path::new("/x")).unwrap(), None);
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "fuel exhaustion must terminate quickly"
    );
    assert_eq!(detector.errors(), 1);
}

#[test]
fn cancellation_interrupts_a_runaway_call() {
    let limits = Limits {
        // Enough fuel that only the epoch watchdog can stop the loop.
        fuel_per_call: u64::MAX,
        epoch_interval_ms: 10,
        ..Limits::default()
    };
    let (runtime, _dir) = runtime(limits);
    let mut detector = detector(&runtime, CapabilityPolicy::none(), &[("scenario", "loop")]);

    runtime.cancel();
    let started = Instant::now();
    assert_eq!(detector.check_file(Path::new("/x")).unwrap(), None);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "cooperative cancellation must interrupt the loop"
    );
    assert_eq!(detector.errors(), 1);
}

#[test]
fn stat_import_reports_metadata() {
    let data = tempfile::tempdir().unwrap();
    let file = data.path().join("data");
    std::fs::write(&file, b"abc").unwrap();

    let (runtime, _dir) = runtime(Limits::default());
    let mut detector = detector(
        &runtime,
        CapabilityPolicy::none(),
        &[("scenario", "stat"), ("stat-path", file.to_str().unwrap())],
    );

    let claim = detector
        .check_file(Path::new("/ignored"))
        .unwrap()
        .expect("stat claim");
    assert_eq!(claim.status, Status::Dirty);
    assert_eq!(attr(&claim, "kind"), Some("file"));
    assert_eq!(attr(&claim, "size"), Some("3"));
    assert!(attr(&claim, "mode").is_some());
    assert!(attr(&claim, "uid").is_some());
    assert!(attr(&claim, "gid").is_some());
    assert!(attr(&claim, "nlink").is_some());
}

#[test]
fn exec_import_reports_exit_code_and_streams() {
    if !Path::new("/bin/sh").exists() {
        eprintln!("skipping: /bin/sh is not available");
        return;
    }
    let (runtime, _dir) = runtime(Limits::default());
    let mut detector = detector(
        &runtime,
        CapabilityPolicy::none().with_exec(["sh"]),
        &[
            ("scenario", "exec"),
            ("program", "/bin/sh"),
            ("args", "-c\u{1f}printf out; printf err 1>&2; exit 3"),
        ],
    );

    let claim = detector
        .check_file(Path::new("/x"))
        .unwrap()
        .expect("exec claim");
    assert_eq!(attr(&claim, "code"), Some("3"));
    assert_eq!(attr(&claim, "stdout"), Some("out"));
    assert_eq!(attr(&claim, "stderr"), Some("err"));
    assert_eq!(attr(&claim, "timed-out"), Some("false"));
}

#[test]
fn exec_import_honors_the_timeout() {
    if !Path::new("/bin/sleep").exists() {
        eprintln!("skipping: /bin/sleep is not available");
        return;
    }
    let (runtime, _dir) = runtime(Limits::default());
    let mut detector = detector(
        &runtime,
        CapabilityPolicy::none().with_exec(["sleep"]),
        &[
            ("scenario", "exec"),
            ("program", "/bin/sleep"),
            ("args", "5"),
            ("timeout-ms", "100"),
        ],
    );

    let claim = detector
        .check_file(Path::new("/x"))
        .unwrap()
        .expect("exec claim");
    assert_eq!(attr(&claim, "timed-out"), Some("true"));
    assert_eq!(attr(&claim, "code"), Some("-1"));
}

#[test]
fn exec_import_is_refused_without_the_capability() {
    let (runtime, _dir) = runtime(Limits::default());
    let mut detector = detector(
        &runtime,
        CapabilityPolicy::none(),
        &[
            ("scenario", "exec"),
            ("program", "/bin/sh"),
            ("args", "-c\u{1f}true"),
        ],
    );

    let claim = detector
        .check_file(Path::new("/x"))
        .unwrap()
        .expect("error claim");
    assert_eq!(claim.status, Status::Unknown);
    assert!(
        attr(&claim, "error").unwrap().contains("not granted"),
        "expected a gating error, got: {:?}",
        attr(&claim, "error")
    );
}

#[test]
fn cache_import_round_trips_across_instances() {
    let (runtime, _dir) = runtime(Limits::default());
    let policy = CapabilityPolicy::none().with_cache(true);
    let config = [
        ("scenario", "cache"),
        ("cache-key", "answer"),
        ("cache-value", "42"),
    ];

    let mut first = detector(&runtime, policy.clone(), &config);
    let first_claim = first.check_file(Path::new("/x")).unwrap().unwrap();
    assert_eq!(attr(&first_claim, "cache-hit"), Some("false"));
    assert_eq!(attr(&first_claim, "cache-value"), Some("42"));

    let mut second = detector(&runtime, policy, &config);
    let second_claim = second.check_file(Path::new("/x")).unwrap().unwrap();
    assert_eq!(attr(&second_claim, "cache-hit"), Some("true"));
    assert_eq!(attr(&second_claim, "cache-value"), Some("42"));
}

#[test]
fn http_import_against_a_local_server() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut scratch = [0u8; 1024];
        let _ = stream.read(&mut scratch);
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello");
    });

    let (runtime, _dir) = runtime(Limits::default());
    let policy = CapabilityPolicy::none().with_http([format!("http://{address}/")]);
    let mut detector = detector(
        &runtime,
        policy,
        &[
            ("scenario", "http"),
            ("url", &format!("http://{address}/status")),
        ],
    );

    let claim = detector
        .check_file(Path::new("/x"))
        .unwrap()
        .expect("http claim");
    assert_eq!(attr(&claim, "status"), Some("200"));
    assert_eq!(attr(&claim, "body"), Some("hello"));
    server.join().unwrap();
}

#[test]
fn http_import_is_refused_without_the_capability() {
    let (runtime, _dir) = runtime(Limits::default());
    let mut detector = detector(
        &runtime,
        CapabilityPolicy::none(),
        &[("scenario", "http"), ("url", "http://127.0.0.1:1/")],
    );

    let claim = detector
        .check_file(Path::new("/x"))
        .unwrap()
        .expect("error claim");
    assert_eq!(claim.status, Status::Unknown);
    assert!(attr(&claim, "error").unwrap().contains("not granted"));
}

#[test]
fn log_import_never_fails() {
    let (runtime, _dir) = runtime(Limits::default());
    let mut detector = detector(&runtime, CapabilityPolicy::none(), &[("scenario", "log")]);

    assert_eq!(detector.check_file(Path::new("/x")).unwrap(), None);
    assert_eq!(detector.errors(), 0);
}

#[test]
fn finish_error_is_surfaced_in_the_report() {
    let (runtime, _dir) = runtime(Limits::default());
    let mut detector = detector(
        &runtime,
        CapabilityPolicy::none(),
        &[("scenario", "finish-error")],
    );

    assert!(detector.finish().is_err());
    let report = detector.report();
    assert_eq!(report.errors, 1);
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.contains("finish failed")),
        "notes: {:?}",
        report.notes
    );
}
