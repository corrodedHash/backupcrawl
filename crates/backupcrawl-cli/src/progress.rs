//! `--progress` rendering.
//!
//! Progress goes to **stderr** so it can never corrupt console or JSON output on
//! stdout. Rendering is disabled when stderr is not a terminal (the assert_cmd
//! tests see a pipe), and updates are throttled because the walker publishes a
//! snapshot after every directory.

use std::io::IsTerminal;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use indicatif::{ProgressBar, ProgressStyle};

use backupcrawl_core::progress::{ProgressSink, ProgressSnapshot};

/// Renders crawl progress on stderr when stderr is a terminal; otherwise every
/// update is discarded and no bytes are written.
pub struct ProgressDisplay {
    bar: ProgressBar,
    enabled: bool,
    last: Mutex<Instant>,
    throttle: Duration,
}

impl ProgressDisplay {
    /// Creates a display targeting stderr, disabled on a non-TTY.
    pub fn stderr() -> Self {
        let enabled = std::io::stderr().is_terminal();
        let bar = if enabled {
            let bar = ProgressBar::new_spinner();
            bar.set_style(
                ProgressStyle::with_template("{msg}").expect("the progress template is valid"),
            );
            bar.set_message(progress_message(&ProgressSnapshot::default()));
            bar
        } else {
            ProgressBar::hidden()
        };
        Self {
            bar,
            enabled,
            // Start "long ago" so the first snapshot always draws.
            last: Mutex::new(Instant::now() - Duration::from_secs(1)),
            throttle: Duration::from_millis(100),
        }
    }
}

impl ProgressSink for ProgressDisplay {
    fn update(&self, snapshot: ProgressSnapshot) {
        if !self.enabled {
            return;
        }
        {
            let mut last = self.last.lock().unwrap_or_else(|err| err.into_inner());
            if last.elapsed() < self.throttle {
                return;
            }
            *last = Instant::now();
        }
        self.bar.set_message(progress_message(&snapshot));
    }
}

impl Drop for ProgressDisplay {
    fn drop(&mut self) {
        if self.enabled {
            // Leave the terminal clean before the result panels are printed.
            self.bar.finish_and_clear();
        }
    }
}

/// The one-line progress text.
///
/// Kept separate from the `indicatif` bar so the format is unit-testable without
/// a terminal.
pub fn progress_message(snapshot: &ProgressSnapshot) -> String {
    format!(
        "walking: {} open · {} closed · {} files · {} claims · {} denied",
        snapshot.directories_open(),
        snapshot.directories_closed,
        snapshot.files_seen,
        snapshot.claims,
        snapshot.denied,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_message_reports_open_and_closed_directories() {
        let snapshot = ProgressSnapshot {
            directories_opened: 10,
            directories_closed: 7,
            files_seen: 42,
            claims: 3,
            denied: 1,
        };
        let message = progress_message(&snapshot);
        assert!(message.contains("3 open"), "{message}");
        assert!(message.contains("7 closed"), "{message}");
        assert!(message.contains("42 files"), "{message}");
        assert!(message.contains("3 claims"), "{message}");
        assert!(message.contains("1 denied"), "{message}");
    }
}
