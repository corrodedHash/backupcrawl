//! Ignore-pattern matching.
//!
//! Patterns use shell globs and are matched against full entry paths, the same
//! shape as the Python implementation's `fnmatch(expanduser(pattern))`.

use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};

/// Failure to compile the configured ignore patterns.
#[derive(Debug, thiserror::Error)]
pub enum IgnoreError {
    /// A pattern is not a valid glob.
    #[error("invalid ignore pattern {pattern:?}: {source}")]
    InvalidPattern {
        /// The offending pattern, before `~` expansion.
        pattern: String,
        /// Underlying glob parser error.
        source: globset::Error,
    },
    /// The set could not be built from individually valid globs.
    #[error("failed to build the ignore matcher: {source}")]
    Build {
        /// Underlying globset error.
        source: globset::Error,
    },
}

/// Compiled ignore globs.
#[derive(Debug)]
pub struct IgnoreMatcher {
    set: GlobSet,
}

impl IgnoreMatcher {
    /// Compiles `patterns`, expanding a leading `~` to the home directory.
    ///
    /// No `*` matching restrictions are enabled, so `*` crosses `/`, matching
    /// Python's `fnmatch` behavior.
    pub fn new<I, S>(patterns: I) -> Result<Self, IgnoreError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut builder = GlobSetBuilder::new();
        for pattern in patterns {
            let expanded = expand_tilde(pattern.as_ref());
            let glob = Glob::new(&expanded).map_err(|source| IgnoreError::InvalidPattern {
                pattern: pattern.as_ref().to_owned(),
                source,
            })?;
            builder.add(glob);
        }
        let set = builder
            .build()
            .map_err(|source| IgnoreError::Build { source })?;
        Ok(Self { set })
    }

    /// Returns whether an empty set was compiled (no patterns configured).
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    /// Returns whether `path` matches any configured pattern.
    pub fn is_ignored(&self, path: &Path) -> bool {
        self.set.is_match(path)
    }
}

/// Expands a leading `~/` or a bare `~` to the current user's home directory.
fn expand_tilde(pattern: &str) -> String {
    let rest = match pattern {
        "~" => Some(""),
        _ => pattern.strip_prefix("~/"),
    };
    match (rest, dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest).to_string_lossy().into_owned(),
        _ => pattern.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn matches_glob_across_separators_like_fnmatch() {
        let matcher = IgnoreMatcher::new(["*/node_modules"]).unwrap();
        assert!(matcher.is_ignored(&PathBuf::from("/home/u/proj/node_modules")));
        assert!(!matcher.is_ignored(&PathBuf::from("/home/u/proj/src")));
    }

    #[test]
    fn expands_tilde() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let matcher = IgnoreMatcher::new(["~/secret"]).unwrap();
        assert!(matcher.is_ignored(&home.join("secret")));
    }

    #[test]
    fn rejects_invalid_patterns() {
        assert!(matches!(
            IgnoreMatcher::new(["["]),
            Err(IgnoreError::InvalidPattern { .. })
        ));
    }
}
