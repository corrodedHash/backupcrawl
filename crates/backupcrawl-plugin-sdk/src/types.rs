//! Native records a detector author works with.
//!
//! These mirror the canonical `backupcrawl:detector@1.0.0` WIT types
//! (`crates/backupcrawl-wit/wit/detector.wit`) one-to-one but use idiomatic
//! Rust (`String`, `PathBuf`, `Vec`) so detector logic compiles and runs under
//! plain `cargo test`, without the wasm toolchain. The [`bind_detector!`] macro
//! translates between these records and the generated component types.
//!
//! [`bind_detector!`]: crate::bind_detector

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

/// How a path relates to a backup mechanism (WIT `types.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    /// A verified copy exists.
    Clean,
    /// Managed by the mechanism, but the current content is not captured.
    Dirty,
    /// Captured locally, not propagated (shown as "Unsynced").
    Ahead,
    /// Managed, but the state could not be determined.
    Unknown,
    /// Deliberately excluded by the mechanism (`.gitignore`, `.stignore`).
    Ignored,
}

impl Status {
    /// Lowercase WIT/JSON spelling of the status.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Dirty => "dirty",
            Self::Ahead => "ahead",
            Self::Unknown => "unknown",
            Self::Ignored => "ignored",
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Status {
    type Err = UnknownStatus;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "clean" => Ok(Self::Clean),
            "dirty" => Ok(Self::Dirty),
            "ahead" => Ok(Self::Ahead),
            "unknown" => Ok(Self::Unknown),
            "ignored" => Ok(Self::Ignored),
            other => Err(UnknownStatus(other.to_owned())),
        }
    }
}

/// Error returned when parsing an unrecognized [`Status`] string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownStatus(pub String);

impl fmt::Display for UnknownStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown detector status: {:?}", self.0)
    }
}

impl std::error::Error for UnknownStatus {}

/// A free-form key/value pair attached to a claim (WIT `types.attr`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attr {
    /// Attribute name, e.g. `package`.
    pub key: String,
    /// Attribute value, e.g. `bash`.
    pub value: String,
}

impl Attr {
    /// Create an attribute.
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

/// A detector's verdict for one path (WIT `types.claim`).
///
/// Returning `None` from a check means "not managed by me". A `Some(claim)`
/// always carries a [`Status`]; `prune` asks the walker not to descend into a
/// claimed directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// The verdict.
    pub status: Status,
    /// Details shown in `--all` output.
    pub attrs: Vec<Attr>,
    /// Do not descend into this directory (ignored for files/symlinks).
    pub prune: bool,
}

impl Claim {
    /// A claim with no attributes and `prune = false`.
    pub fn new(status: Status) -> Self {
        Self {
            status,
            attrs: Vec::new(),
            prune: false,
        }
    }

    /// A `clean` claim.
    pub fn clean() -> Self {
        Self::new(Status::Clean)
    }

    /// A `dirty` claim.
    pub fn dirty() -> Self {
        Self::new(Status::Dirty)
    }

    /// An `ahead` claim.
    pub fn ahead() -> Self {
        Self::new(Status::Ahead)
    }

    /// An `unknown` claim.
    pub fn unknown() -> Self {
        Self::new(Status::Unknown)
    }

    /// An `ignored` claim.
    pub fn ignored() -> Self {
        Self::new(Status::Ignored)
    }

    /// Add an attribute (builder style).
    #[must_use]
    pub fn attr(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attrs.push(Attr::new(key, value));
        self
    }

    /// Mark a directory claim as pruning (builder style).
    #[must_use]
    pub fn prune(mut self) -> Self {
        self.prune = true;
        self
    }

    /// Mark a directory claim as descending (builder style; the default).
    #[must_use]
    pub fn no_prune(mut self) -> Self {
        self.prune = false;
        self
    }
}

/// The plugin's own configuration table from the rcfile (WIT `scan-info.config`).
///
/// This is the flattened `[plugins.<name>]` table, delivered as a list of
/// string attributes so the contract stays schema-agnostic. Use [`Config::get`]
/// with the same defaulting logic the plugin documents.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    attrs: Vec<Attr>,
}

impl Config {
    /// Wrap a list of attributes.
    pub fn new(attrs: Vec<Attr>) -> Self {
        Self { attrs }
    }

    /// Look up an attribute value by key.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|attr| attr.key == key)
            .map(|attr| attr.value.as_str())
    }

    /// Whether a key is present.
    pub fn contains_key(&self, key: &str) -> bool {
        self.attrs.iter().any(|attr| attr.key == key)
    }

    /// Iterate over all attributes in declaration order.
    pub fn iter(&self) -> impl Iterator<Item = &Attr> {
        self.attrs.iter()
    }

    /// The raw attribute list.
    pub fn attrs(&self) -> &[Attr] {
        &self.attrs
    }

    /// Number of attributes.
    pub fn len(&self) -> usize {
        self.attrs.len()
    }

    /// Whether the config table is empty.
    pub fn is_empty(&self) -> bool {
        self.attrs.is_empty()
    }
}

/// Context for one scan, delivered to [`Detector::init`](crate::Detector::init).
///
/// (WIT `types.scan-info`.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanInfo {
    /// Directory the crawl started at.
    pub root: PathBuf,
    /// Name of the user running the crawl.
    pub user: String,
    /// Whether the walker follows symlinks (off by default in v1).
    pub follow_symlinks: bool,
    /// This plugin's configuration table.
    pub config: Config,
}

/// A symlink seen by the walker. (WIT `types.link-info`.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkInfo {
    /// Path of the symlink itself.
    pub path: PathBuf,
    /// Target the link points at (not necessarily resolved).
    pub target: PathBuf,
    /// Whether the target does not exist.
    pub dangling: bool,
}

/// Unix file kind (WIT `host.file-kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    /// Regular file.
    File,
    /// Directory.
    Directory,
    /// Symlink.
    Symlink,
    /// Socket, FIFO, device node, ...
    Other,
}

/// Unix metadata that WASI 0.2 does not expose (WIT `host.file-info`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileInfo {
    /// File kind.
    pub kind: FileKind,
    /// Size in bytes.
    pub size: u64,
    /// Modification time in milliseconds since the Unix epoch.
    pub mtime_ms: u64,
    /// Unix mode bits.
    pub mode: u32,
    /// Owning user id.
    pub uid: u32,
    /// Owning group id.
    pub gid: u32,
    /// Number of hard links.
    pub nlink: u64,
}

/// A request to run an allowlisted program (WIT `host.exec-request`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecRequest {
    /// Program name looked up in the plugin's allowlist.
    pub program: String,
    /// Command-line arguments.
    pub args: Vec<String>,
    /// Working directory.
    pub cwd: Option<PathBuf>,
    /// Environment overrides.
    pub env: Vec<(String, String)>,
    /// Optional stdin bytes.
    pub stdin: Option<Vec<u8>>,
    /// Optional timeout in milliseconds.
    pub timeout_ms: Option<u32>,
}

impl ExecRequest {
    /// A request for `program` with no arguments.
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            stdin: None,
            timeout_ms: None,
        }
    }

    /// Append one argument.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Append several arguments.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Set the working directory.
    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Add an environment override.
    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Set stdin.
    #[must_use]
    pub fn stdin(mut self, stdin: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(stdin.into());
        self
    }

    /// Set the timeout in milliseconds.
    #[must_use]
    pub fn timeout_ms(mut self, timeout_ms: u32) -> Self {
        self.timeout_ms = Some(timeout_ms);
        self
    }
}

/// The result of [`Host::exec`](crate::Host::exec) (WIT `host.exec-result`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecResult {
    /// Exit code, or `-1` when the child was killed.
    pub code: i32,
    /// Captured stdout.
    pub stdout: Vec<u8>,
    /// Captured stderr.
    pub stderr: Vec<u8>,
    /// Whether the timeout fired.
    pub timed_out: bool,
}

/// An HTTP request (WIT `host.http-request`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// HTTP method, e.g. `GET`.
    pub method: String,
    /// Absolute URL.
    pub url: String,
    /// Request headers.
    pub headers: Vec<(String, String)>,
    /// Optional request body.
    pub body: Option<Vec<u8>>,
}

impl HttpRequest {
    /// A request for `method` on `url`.
    pub fn new(method: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            url: url.into(),
            headers: Vec::new(),
            body: None,
        }
    }

    /// Add a header.
    #[must_use]
    pub fn header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((key.into(), value.into()));
        self
    }

    /// Set the body.
    #[must_use]
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(body.into());
        self
    }
}

/// An HTTP response (WIT `host.http-response`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers.
    pub headers: Vec<(String, String)>,
    /// Response body.
    pub body: Vec<u8>,
}

/// Severity of a [`Host::log`](crate::Host::log) message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LogLevel {
    /// Verbose tracing.
    Debug,
    /// Normal progress information.
    Info,
    /// Something surprising but not fatal.
    Warn,
    /// A failure worth reporting.
    Error,
}

impl LogLevel {
    /// Lowercase spelling used by the WIT `log` import.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_round_trips_through_strings() {
        for status in [
            Status::Clean,
            Status::Dirty,
            Status::Ahead,
            Status::Unknown,
            Status::Ignored,
        ] {
            assert_eq!(status.to_string().parse::<Status>().unwrap(), status);
        }
        assert!("nope".parse::<Status>().is_err());
    }

    #[test]
    fn config_reads_attributes() {
        let config = Config::new(vec![Attr::new("a", "1"), Attr::new("b", "2")]);
        assert_eq!(config.get("a"), Some("1"));
        assert_eq!(config.get("missing"), None);
        assert!(config.contains_key("b"));
        assert_eq!(config.len(), 2);
        assert!(!config.is_empty());
        assert_eq!(config.iter().count(), 2);
    }

    #[test]
    fn claim_builders_chain() {
        let claim = Claim::dirty().attr("reason", "x").prune();
        assert_eq!(claim.status, Status::Dirty);
        assert_eq!(claim.attrs, vec![Attr::new("reason", "x")]);
        assert!(claim.prune);
        assert!(!Claim::clean().prune);
    }
}
