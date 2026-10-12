//! The detector authoring trait.

use std::path::{Path, PathBuf};

use crate::error::DetectorError;
use crate::host::Host;
use crate::types::{Claim, LinkInfo, ScanInfo};

/// Logic of one detector, written in native Rust.
///
/// This mirrors the shape of the core `Detector` trait (section 4 of the
/// rewrite plan: `init`/`check_dir`/`check_file`/`check_symlink`/`finish`) but
/// is the *authoring* trait: it works on native records and takes the [`Host`]
/// explicitly, so it is fully unit-testable with a [`MockHost`](crate::MockHost)
/// under plain `cargo test` — no wasm toolchain involved.
///
/// Every method has a default implementation, so a detector only overrides what
/// it needs. A return value of `Ok(None)` means "not managed by me"; the first
/// detector returning `Some(claim)` wins (section 5.4).
///
/// Authors call [`bind_detector!`](crate::bind_detector) to emit the wasm
/// component export glue for a type implementing this trait.
pub trait Detector: Sized + 'static {
    /// Called once before the scan. Expensive preparation (global listings,
    /// cached state) belongs here. Returning `Err` disables the plugin for the
    /// scan with a visible warning; the scan continues.
    fn init(&mut self, _info: &ScanInfo, _host: &mut dyn Host) -> Result<(), DetectorError> {
        Ok(())
    }

    /// Ask the detector about a directory. `prune = true` stops the descent;
    /// `prune = false` records the claim and keeps descending.
    fn check_dir(
        &mut self,
        _path: &Path,
        _host: &mut dyn Host,
    ) -> Result<Option<Claim>, DetectorError> {
        Ok(None)
    }

    /// Ask the detector about a regular file.
    fn check_file(
        &mut self,
        _path: &Path,
        _host: &mut dyn Host,
    ) -> Result<Option<Claim>, DetectorError> {
        Ok(None)
    }

    /// Ask the detector about several files at once.
    ///
    /// The default implementation is exactly N singular [`check_file`] calls,
    /// which is what the WIT `@1` contract requires of `check-files`. Detectors
    /// may override it for a bulk optimization, but the observable behavior
    /// must stay identical to the singular calls.
    ///
    /// [`check_file`]: Detector::check_file
    fn check_files(
        &mut self,
        paths: &[PathBuf],
        host: &mut dyn Host,
    ) -> Result<Vec<Option<Claim>>, DetectorError> {
        paths
            .iter()
            .map(|path| self.check_file(path, host))
            .collect()
    }

    /// Ask the detector about a symlink. The walker keeps skipping symlinks
    /// until M2; the method exists so detectors can opt in when it lands.
    fn check_symlink(
        &mut self,
        _link: &LinkInfo,
        _host: &mut dyn Host,
    ) -> Result<Option<Claim>, DetectorError> {
        Ok(None)
    }

    /// Called once after the scan. Flush diagnostics or persist caches here.
    fn finish(&mut self, _host: &mut dyn Host) -> Result<(), DetectorError> {
        Ok(())
    }
}
