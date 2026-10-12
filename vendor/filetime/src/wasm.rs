//! WASI implementation for `filetime`.
//!
//! Upstream `filetime` stubs the read accessors with `unimplemented!()` on
//! wasm, which makes it panic whenever a dependency reads a timestamp (gix does
//! this while opening the index). WASI can read timestamps fine through
//! `std::fs::Metadata`, so implement the accessors here and keep only the
//! write paths unsupported (the plugin is read-only).
//!
//! Wired in through `[patch.crates-io]` in the spike workspace; see
//! `docs/rust-rewrite-plan.md` §7.1 "M1.7 spike results".

use crate::FileTime;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn set_symlink_file_times(_p: &Path, _atime: FileTime, _mtime: FileTime) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Wasm not implemented",
    ))
}

fn from_system_time(time: SystemTime) -> FileTime {
    match time.duration_since(UNIX_EPOCH) {
        Ok(since_epoch) => FileTime::from_unix_time(
            since_epoch.as_secs() as i64,
            since_epoch.subsec_nanos(),
        ),
        // Timestamps before the epoch: mirror the unix representation of a
        // negative duration as (secs, positive nanos).
        Err(err) => {
            let duration = err.duration();
            let (seconds, nanos) = if duration.subsec_nanos() == 0 {
                (-(duration.as_secs() as i64), 0)
            } else {
                (
                    -(duration.as_secs() as i64) - 1,
                    1_000_000_000 - duration.subsec_nanos(),
                )
            };
            FileTime::from_unix_time(seconds, nanos)
        }
    }
}

pub fn from_last_modification_time(meta: &fs::Metadata) -> FileTime {
    from_system_time(meta.modified().unwrap_or(UNIX_EPOCH))
}

pub fn from_last_access_time(meta: &fs::Metadata) -> FileTime {
    from_system_time(meta.accessed().unwrap_or(UNIX_EPOCH))
}

pub fn from_creation_time(meta: &fs::Metadata) -> Option<FileTime> {
    meta.created().ok().map(from_system_time)
}

pub fn open(path: &Path) -> io::Result<fs::File> {
    fs::File::open(path)
}
