//! Fixture detector component for host tests.
//!
//! Implements the canonical `backupcrawl:detector@1.0.0` `plugin` world with
//! raw `wit-bindgen` (the authoring SDK arrives in M1.2). Behavior:
//!
//! - `init` and `finish` succeed,
//! - `check-dir`, `check-file` and `check-symlink` return no claim (`none`),
//! - `check-files` loops the singular `check-file` semantics.
//!
//! Nothing beyond the `host` interface is imported (no `std::fs`). The
//! wasm-only glue is gated with `#[cfg(target_family = "wasm")]` so plain
//! `cargo clippy`/`cargo test` compile this crate as an empty library; only
//! `cargo component build` (wasm32-wasip1) sees the component. Rebuild and
//! refresh the checked-in artifact with `scripts/build-test-components.sh`.

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "wit",
        world: "plugin",
    });

    use exports::backupcrawl::detector::detector::{Claim, Guest, LinkInfo, ScanInfo};

    struct TestDetector;

    impl Guest for TestDetector {
        fn init(_info: ScanInfo) -> Result<(), String> {
            Ok(())
        }

        fn check_dir(_path: String) -> Result<Option<Claim>, String> {
            Ok(None)
        }

        fn check_file(_path: String) -> Result<Option<Claim>, String> {
            Ok(None)
        }

        fn check_symlink(_link: LinkInfo) -> Result<Option<Claim>, String> {
            Ok(None)
        }

        /// Bulk variant: exactly N singular calls.
        fn check_files(paths: Vec<String>) -> Result<Vec<Option<Claim>>, String> {
            paths.into_iter().map(Self::check_file).collect()
        }

        fn finish() -> Result<(), String> {
            Ok(())
        }
    }

    export!(TestDetector);
}
