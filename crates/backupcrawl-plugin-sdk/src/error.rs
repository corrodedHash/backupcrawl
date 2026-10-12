//! Error types for detector authors.

use std::fmt;

/// A host service call failed.
///
/// Returned by [`Host`](crate::Host) methods; the message is the string carried
/// across the component boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostError {
    message: String,
}

impl HostError {
    /// Build a host error from any string-like message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The error message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HostError {}

impl From<String> for HostError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

impl From<&str> for HostError {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}

/// A detector check failed.
///
/// Returning one of these from [`Detector`](crate::Detector) aborts that single
/// call; the host logs it, treats the path as unclaimed and keeps going (see
/// section 5.4 of the rewrite plan).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectorError {
    message: String,
}

impl DetectorError {
    /// Build a detector error from any string-like message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The error message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for DetectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DetectorError {}

impl From<String> for DetectorError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

impl From<&str> for DetectorError {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}

impl From<HostError> for DetectorError {
    fn from(error: HostError) -> Self {
        Self::new(error.message)
    }
}
