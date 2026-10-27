//! Performance tests for the M1.7 component.
//!
//! The small-repo test runs as part of the normal suite and prints timings;
//! the large synthetic benchmark is `#[ignore]`d because it creates ~50k
//! files. Run it explicitly:
//!
//! ```console
//! $ CARGO_TARGET_DIR=/var/lib/opencode-agent/targets/m1-gix-spike \
//!   cargo test --release --test large_repo -- --ignored --nocapture
//! ```
//!
//! Set `GIX_SPIKE_LARGE_REPO=/path/to/repo` to benchmark an existing
//! repository instead of generating the synthetic one. Set
//! `GIX_SPIKE_COMPONENT` to point at a release-built component.

mod common;

use common::host;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
fn small_repo_timing() {
    let fx = common::Fixture::new();
    fx.init();
    for i in 0..10 {
        fx.write(&format!("f{i}.txt"), "x\n");
    }
    fx.commit("initial");

    let root = fx.path().to_str().unwrap();
    let host = host();
    let cold = host.run_cold(root).expect("cold run");
    eprintln!(
        "small repo: instantiate {:.1} ms, run {:.1} ms",
        cold.instantiate.unwrap().as_secs_f64() * 1e3,
        cold.call.as_secs_f64() * 1e3
    );

    let (mut store, world) = host.instantiate().expect("instantiate");
    for i in 0..3 {
        let call = host.call_warm(&mut store, &world, root).expect("warm run");
        eprintln!("small repo: warm run {i} {:.1} ms", call.call.as_secs_f64() * 1e3);
    }

    let json: Value = serde_json::from_str(&cold.json).unwrap();
    assert_eq!(json["error"], Value::Null);
    assert_eq!(json["dirty"], false);
}

#[test]
#[ignore = "creates ~50k files; run explicitly with --ignored --nocapture"]
fn large_synthetic_repo_benchmark() {
    let (repo, _tmp) = match std::env::var_os("GIX_SPIKE_LARGE_REPO") {
        Some(path) => (PathBuf::from(path), None),
        None => {
            let tmp = tempfile::tempdir().expect("tempdir");
            let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../gen-large-repo.sh");
            let status = Command::new("bash")
                .arg(&script)
                .arg(tmp.path().join("repo"))
                .status()
                .expect("failed to run gen-large-repo.sh");
            assert!(status.success(), "generator failed");
            (tmp.path().join("repo"), Some(tmp))
        }
    };
    let root = repo.to_str().unwrap();
    let host = host();
    let cold = host.run_cold(root).expect("cold run");
    let json: Value = serde_json::from_str(&cold.json).unwrap();
    assert_eq!(json["error"], Value::Null, "{json}");
    assert_eq!(json["dirty"], false, "{json}");
    eprintln!(
        "large repo {}: instantiate {:.1} ms, run {:.1} ms",
        repo.display(),
        cold.instantiate.unwrap().as_secs_f64() * 1e3,
        cold.call.as_secs_f64() * 1e3
    );

    let (mut store, world) = host.instantiate().expect("instantiate");
    for i in 0..3 {
        let call = host.call_warm(&mut store, &world, root).expect("warm run");
        eprintln!("large repo: warm run {i} {:.1} ms", call.call.as_secs_f64() * 1e3);
    }
}
