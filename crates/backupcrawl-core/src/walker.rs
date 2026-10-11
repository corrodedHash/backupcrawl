//! Depth-first crawler.
//!
//! The traversal mirrors the Python implementation: per directory, ignored
//! entries and symlinks are skipped, unreadable files/directories are collected
//! as denied, sockets/FIFOs/devices are ignored, and the remaining regular
//! files are offered to the detectors. In M0 no detectors are wired yet, so
//! every file ends up loose.
//!
//! Loose-path collapsing is preserved: a directory whose subtree contains no
//! claimed paths is reported as the directory itself instead of listing all of
//! its files (this is what keeps `/usr`-style output readable).

use std::path::{Path, PathBuf};

use log::{debug, warn};

use crate::ignore::IgnoreMatcher;
use crate::model::{CrawlResult, PluginReport};

/// Scans `root` depth-first and returns the aggregated result.
pub fn scan(root: &Path, ignore: &IgnoreMatcher) -> CrawlResult {
    let mut result = CrawlResult {
        root: root.to_path_buf(),
        loose_paths: Vec::new(),
        denied_paths: Vec::new(),
        plugins: Vec::new(),
    };
    let dir = crawl_dir(root, ignore);
    result.plugins.extend(dir.claims);
    result.loose_paths.extend(dir.loose_paths);
    result.denied_paths.extend(dir.denied_paths);
    result
}

/// Result of crawling a single directory, before merging into its parent.
///
/// `claims` is empty until detectors are wired in (M1); the merge logic below
/// is already shaped around it so the walker does not need to change.
#[derive(Debug, Default)]
struct DirResult {
    loose_paths: Vec<PathBuf>,
    denied_paths: Vec<PathBuf>,
    claims: Vec<PluginReport>,
}

fn crawl_dir(root: &Path, ignore: &IgnoreMatcher) -> DirResult {
    debug!("Entering {}", root.display());
    let mut out = DirResult::default();

    let reader = match std::fs::read_dir(root) {
        Ok(reader) => reader,
        Err(err) => {
            if err.kind() == std::io::ErrorKind::PermissionDenied {
                out.denied_paths.push(root.to_path_buf());
            } else {
                warn!("cannot read {}: {err}", root.display());
            }
            return out;
        }
    };

    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for entry in reader {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                warn!("cannot read an entry in {}: {err}", root.display());
                continue;
            }
        };
        let path = entry.path();
        if ignore.is_ignored(&path) {
            continue;
        }
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(err) => {
                warn!("cannot stat {}: {err}", path.display());
                continue;
            }
        };
        if file_type.is_symlink() {
            // Symlinks are reported from M2 on; for now they are skipped.
            continue;
        } else if file_type.is_file() {
            if is_readable(&path, false) {
                files.push(path);
            } else {
                out.denied_paths.push(path);
            }
        } else if file_type.is_dir() {
            if is_readable(&path, true) {
                dirs.push(path);
            } else {
                out.denied_paths.push(path);
            }
        }
        // Sockets, FIFOs and device nodes are ignored, as in the Python tool.
    }

    files.sort();
    dirs.sort();

    // No file detectors yet: every readable file is loose.
    out.loose_paths.extend(files);

    for dir in dirs {
        let child = crawl_dir(&dir, ignore);
        out.denied_paths.extend(child.denied_paths);
        let has_claims = !child.claims.is_empty();
        out.claims.extend(child.claims);
        if has_claims {
            out.loose_paths.extend(child.loose_paths);
        } else if !child.loose_paths.is_empty() {
            // Fully loose subtree: report the directory, not its contents.
            out.loose_paths.push(dir);
        }
    }

    out
}

/// Checks read access the way the Python tool did (`os.access`): files need
/// `R_OK`, directories need `R_OK | X_OK` to be traversable.
#[cfg(unix)]
fn is_readable(path: &Path, need_execute: bool) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let mode = if need_execute {
        libc::R_OK | libc::X_OK
    } else {
        libc::R_OK
    };
    match CString::new(path.as_os_str().as_bytes()) {
        // SAFETY: access(2) only reads the path and the process's credentials.
        Ok(cpath) => unsafe { libc::access(cpath.as_ptr(), mode) == 0 },
        // A path with an interior NUL cannot exist.
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_readable(path: &Path, _need_execute: bool) -> bool {
    // Best effort on non-Unix platforms; the tool targets Unix.
    path.exists()
}
