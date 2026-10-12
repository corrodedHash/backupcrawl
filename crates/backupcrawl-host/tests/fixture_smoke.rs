//! M1.1/M1.4 smoke test: the checked-in fixture detector component instantiates
//! through the real host [`Runtime`] and answers the detector calls with its
//! documented behavior.
//!
//! The M1.1 version of this test linked a minimal throwaway host that returned
//! "not implemented" for every import; it now uses `backupcrawl-host`'s runtime,
//! imports and limits, which is what M1.4 delivered.

use std::path::{Path, PathBuf};

use backupcrawl_host::{CapabilityPolicy, Limits, Runtime, ScanInfo};

fn fixture_bytes() -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/test-detector.wasm"),
    )
    .expect("checked-in fixture component")
}

#[test]
fn fixture_component_instantiates_and_runs() -> anyhow::Result<()> {
    let runtime = Runtime::new(Limits::default())?;
    let mut detector =
        runtime.instantiate(&fixture_bytes(), "fixture", CapabilityPolicy::none())?;

    detector.init(&ScanInfo {
        root: PathBuf::from("/"),
        user: "fixture".to_owned(),
        follow_symlinks: false,
        config: Vec::new(),
    })?;

    assert_eq!(detector.check_file(Path::new("/fixture.txt"))?, None);
    assert_eq!(detector.check_dir(Path::new("/fixture"))?, None);
    assert_eq!(
        detector.check_symlink(&backupcrawl_host::LinkInfo {
            path: PathBuf::from("/fixture-link"),
            target: PathBuf::from("/fixture.txt"),
            dangling: false,
        })?,
        None
    );
    assert_eq!(
        detector.check_files(&[PathBuf::from("/a.txt"), PathBuf::from("/b.txt")])?,
        vec![None, None]
    );

    detector.finish()?;
    assert_eq!(detector.errors(), 0);
    assert!(!detector.is_disabled());

    Ok(())
}
