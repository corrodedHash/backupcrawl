//! Single-line `unknown` reasons.
//!
//! `attrs.reason` ends up in the JSON report, so it must stay on one line: gix
//! renders its cause chain with embedded newlines (even in the `{:#}` form) and
//! a status error can run to hundreds of bytes, which turns one report row into
//! a page of stack-trace-like text. The one-line reason keeps *what failed*; the
//! full chain belongs in the debug log (`-vv`), where the gix backend logs it
//! before collapsing it.

use std::fmt::Display;

/// Renders `err` as the single line used for an `unknown` verdict's `reason`.
///
/// Only the first non-empty line is kept: gix errors start with the operation
/// that failed (e.g. `"…" does not appear to be a git repository`) and append a
/// `Caused by:` tree, which is noise in the report. The line is trimmed, and an
/// error whose display is empty (or only whitespace) becomes
/// [`NO_MESSAGE`].
pub fn one_line(err: impl Display) -> String {
    err.to_string()
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(NO_MESSAGE)
        .to_owned()
}

/// The fallback reason for an error that renders to nothing at all.
pub const NO_MESSAGE: &str = "(no error message)";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_the_first_line() {
        let reason = one_line("failed to read HEAD: boom\n\nCaused by:\n    0: Missing HEAD");
        assert_eq!(reason, "failed to read HEAD: boom");
        assert!(!reason.contains('\n'));
    }

    #[test]
    fn trims_the_line_and_skips_leading_blank_lines() {
        assert_eq!(
            one_line("\n  failed: boom  \r\nCaused by: nope\n"),
            "failed: boom"
        );
        assert!(!one_line("\n\n  x\n").contains('\n'));
    }

    #[test]
    fn empty_display_has_a_fallback() {
        assert_eq!(one_line(""), NO_MESSAGE);
        assert_eq!(one_line("  \n\t\n"), NO_MESSAGE);
    }

    #[test]
    fn accepts_std_errors() {
        let err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied\nmore");
        assert_eq!(one_line(err), "denied");
    }
}
