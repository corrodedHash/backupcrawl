//! Progress reporting for a running crawl.
//!
//! The walker publishes a plain [`ProgressSnapshot`] after each directory it
//! finishes. Rendering is deliberately left to the caller (the CLI uses
//! `indicatif` on stderr); core knows nothing about terminals, throttling or
//! output formats. Pass [`NoProgress`] when the caller does not want updates.
//!
//! Snapshots are cheap counter reads; a sink that draws to a terminal is
//! expected to throttle itself. Counters are monotonic for the whole scan.

/// A point-in-time view of the crawl, safe to hand to a renderer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProgressSnapshot {
    /// Directories popped from the work queue (finished or in flight).
    pub directories_opened: u64,
    /// Directories whose entries have been processed and enqueued.
    pub directories_closed: u64,
    /// Regular files seen after ignore filtering.
    pub files_seen: u64,
    /// Claims recorded by any detector (directory, file or symlink).
    pub claims: u64,
    /// Paths that could not be read.
    pub denied: u64,
}

impl ProgressSnapshot {
    /// Directories currently being processed (`opened - closed`).
    pub fn directories_open(&self) -> u64 {
        self.directories_opened
            .saturating_sub(self.directories_closed)
    }
}

/// Receives crawl progress. Implementations must be safe to call from several
/// worker threads and should throttle their own rendering.
pub trait ProgressSink: Send + Sync {
    /// Called after each directory finishes; `snapshot` is monotonic.
    fn update(&self, snapshot: ProgressSnapshot);
}

/// A sink that ignores every update.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn update(&self, _snapshot: ProgressSnapshot) {}
}
