//! Package integrity comparison (§7.2 of `docs/rust-rewrite-plan.md`).
//!
//! A path is claimed only when the database owns it. The live object is
//! compared against its mtree entry: existence, type, size, mtime (seconds),
//! permission bits, uid, gid and — for symlinks — the raw link target.
//! `check-level = "checksum"` additionally hashes regular files with md5 and
//! sha256.
//!
//! The status mapping is deliberately conservative:
//!
//! * every mismatch → `dirty`, with pacman-style reasons joined into the
//!   `reason` attribute (`Size mismatch`, `Modification time mismatch`, ...);
//! * an unparseable/absent mtree entry, or a checksum that cannot be read or is
//!   missing from the database at checksum level → `unknown`, never `clean`;
//! * otherwise → `clean` (the package can be reinstalled, so the path does not
//!   need its own backup).

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use backupcrawl_plugin_sdk::{Claim, FileInfo, Host, LinkInfo, Status};
use md5::{Digest as _, Md5};
use sha2::{Digest as _, Sha256};

use crate::config::{CheckLevel, PacmanConfig};
use crate::db::{Index, MtreeKind, OwnedPath, lexical_normalize};

/// Answers one path check. Returns `None` when the database does not own it.
///
/// `link` is the host-provided symlink info for `check_symlink`; it is only
/// consulted when the guest cannot read the raw link target itself (see
/// [`compare_link`]).
pub fn check(
    index: &Index,
    config: &PacmanConfig,
    root: &Path,
    path: &Path,
    link: Option<&LinkInfo>,
    host: &mut dyn Host,
) -> Option<Claim> {
    let key = lookup_key(root, path);
    let owned = index.entries.get(&key)?;

    let mut reasons = Vec::new();
    let mut unknown = Vec::new();

    let Some(mtree) = &owned.mtree else {
        return Some(claim(
            owned,
            Status::Unknown,
            &["No mtree entry for path".to_owned()],
        ));
    };
    if let Some(invalid) = &mtree.invalid {
        return Some(claim(
            owned,
            Status::Unknown,
            &[format!("Unparsable mtree entry: {invalid}")],
        ));
    }

    let info = match host.stat(path) {
        Ok(info) => info,
        Err(err) => {
            let message = err.message();
            let reason = if message.contains("No such file or directory") {
                "No such file or directory".to_owned()
            } else {
                format!("Cannot stat path: {message}")
            };
            return Some(claim(owned, Status::Dirty, &[reason]));
        }
    };

    let live = live_kind(&info);
    let Some(expected_db) = mtree.kind else {
        return Some(claim(
            owned,
            Status::Unknown,
            &["Mtree entry has no usable type".to_owned()],
        ));
    };
    if live != expected_db {
        return Some(claim(
            owned,
            Status::Dirty,
            &["File type mismatch".to_owned()],
        ));
    }

    if let Some(size) = mtree.size
        && info.size != size
    {
        reasons.push("Size mismatch".to_owned());
    }
    if let Some(time) = mtree.time
        && (info.mtime_ms / 1000) as i64 != time
    {
        reasons.push("Modification time mismatch".to_owned());
    }
    if let Some(mode) = mtree.mode
        && (info.mode & 0o7777) != (mode & 0o7777)
    {
        reasons.push("Permissions mismatch".to_owned());
    }
    if let Some(uid) = mtree.uid
        && info.uid != uid
    {
        reasons.push("UID mismatch".to_owned());
    }
    if let Some(gid) = mtree.gid
        && info.gid != gid
    {
        reasons.push("GID mismatch".to_owned());
    }

    if expected_db == MtreeKind::Link
        && let Some(expected_target) = &mtree.link
    {
        compare_link(path, expected_target, link, &mut reasons, &mut unknown);
    }

    if config.check_level == CheckLevel::Checksum && expected_db == MtreeKind::File {
        if mtree.md5.is_none() && mtree.sha256.is_none() {
            unknown.push("No checksum in mtree".to_owned());
        } else {
            match digest_file(path) {
                Ok((md5, sha256)) => {
                    if let Some(expected_md5) = &mtree.md5
                        && md5 != *expected_md5
                    {
                        reasons.push("MD5 checksum mismatch".to_owned());
                    }
                    if let Some(expected_sha) = &mtree.sha256
                        && sha256 != *expected_sha
                    {
                        reasons.push("SHA256 checksum mismatch".to_owned());
                    }
                }
                Err(err) => unknown.push(format!("Cannot read file: {err}")),
            }
        }
    }

    let (status, messages) = if !reasons.is_empty() {
        (Status::Dirty, reasons)
    } else if !unknown.is_empty() {
        (Status::Unknown, unknown)
    } else {
        (Status::Clean, Vec::new())
    };
    Some(claim(owned, status, &messages))
}

/// Looks up `path` as an absolute, lexically normalized database key.
pub fn lookup_key(root: &Path, path: &Path) -> String {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    path_string(&lexical_normalize(&absolute))
}

/// Compares a live symlink with its expected mtree target.
///
/// The guest reads the raw target with [`std::fs::read_link`] whenever it can.
/// The sandbox (cap-std under the WASI preopen) refuses to read a link whose
/// stored target is *absolute* — it returns `EPERM` to avoid leaking host paths
/// — and real packages do use absolute links (`link=/usr/lib/libfoo.so.1`). In
/// that case the host-provided [`LinkInfo::target`] is used: it is the same
/// target, already normalized to an absolute lexical path by the walker.
fn compare_link(
    path: &Path,
    expected_target: &str,
    link: Option<&LinkInfo>,
    reasons: &mut Vec<String>,
    unknown: &mut Vec<String>,
) {
    match std::fs::read_link(path) {
        Ok(target) => {
            if target.to_string_lossy() != expected_target {
                reasons.push("Symlink path mismatch".to_owned());
            }
        }
        Err(err) => {
            if Path::new(expected_target).is_absolute() {
                match link {
                    Some(info) => {
                        let expected = lexical_normalize(Path::new(expected_target));
                        if lexical_normalize(&info.target) != expected {
                            reasons.push("Symlink path mismatch".to_owned());
                        }
                    }
                    None => unknown.push(format!(
                        "Cannot read symlink target: {err} (no link info from the walker)"
                    )),
                }
            } else {
                unknown.push(format!("Cannot read symlink target: {err}"));
            }
        }
    }
}

fn path_string(path: &Path) -> String {
    let text = path.to_string_lossy();
    let trimmed = text.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_owned()
    } else if trimmed.starts_with('/') {
        trimmed.to_owned()
    } else {
        format!("/{trimmed}")
    }
}

fn live_kind(info: &FileInfo) -> MtreeKind {
    match info.kind {
        backupcrawl_plugin_sdk::FileKind::Directory => MtreeKind::Dir,
        backupcrawl_plugin_sdk::FileKind::Symlink => MtreeKind::Link,
        // Sockets/FIFOs/devices are not package-managed file types; treating
        // them as files makes the verdict a type mismatch instead of a clean.
        _ => MtreeKind::File,
    }
}

fn claim(owned: &OwnedPath, status: Status, reasons: &[String]) -> Claim {
    let mut claim = Claim::new(status)
        .attr("package", owned.package.clone())
        .attr("version", owned.version.clone());
    if owned.backup {
        claim = claim.attr("backup", "true");
    }
    if !reasons.is_empty() {
        claim = claim.attr("reason", reasons.join(", "));
    }
    claim
}

/// Streams a file through md5 and sha256 so checksum mode never loads a whole
/// package file into memory.
fn digest_file(path: &Path) -> std::io::Result<(String, String)> {
    let mut file = File::open(path)?;
    let mut md5 = Md5::new();
    let mut sha256 = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        md5.update(&buffer[..read]);
        sha256.update(&buffer[..read]);
    }
    Ok((hex::encode(md5.finalize()), hex::encode(sha256.finalize())))
}

/// Resolves the root used for relative crawl paths once, in `init`.
pub fn canonical_root(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| lexical_normalize(root))
}
