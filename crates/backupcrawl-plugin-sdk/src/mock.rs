//! A mock [`Host`] for native detector tests.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use crate::error::HostError;
use crate::host::Host;
use crate::types::{ExecRequest, ExecResult, FileInfo, HttpRequest, HttpResponse, LogLevel};

/// A programmable in-memory host.
///
/// Tests stage the responses a detector should see (`with_stat`,
/// `with_cache`, ...) and then inspect what the detector did (`logs`,
/// `exec_requests`, `cache_sets`, ...). Look-ups that were not staged fail with
/// a descriptive [`HostError`] so a test cannot silently pass on missing setup.
///
/// ```
/// use std::path::Path;
/// use backupcrawl_plugin_sdk::{FileInfo, FileKind, Host, MockHost};
///
/// let mut host = MockHost::new().with_stat(
///     Path::new("/x/.marker"),
///     FileInfo { kind: FileKind::File, size: 0, mtime_ms: 0, mode: 0o644, uid: 0, gid: 0, nlink: 1 },
/// );
/// let info = host.stat(Path::new("/x/.marker")).unwrap();
/// assert_eq!(info.kind, FileKind::File);
/// ```
#[derive(Debug, Default, Clone)]
pub struct MockHost {
    stats: HashMap<PathBuf, Result<FileInfo, HostError>>,
    exec_results: VecDeque<Result<ExecResult, HostError>>,
    exec_requests: Vec<ExecRequest>,
    cache: HashMap<String, Vec<u8>>,
    cache_sets: Vec<(String, Vec<u8>)>,
    cache_reads: Vec<(String, u64, u32)>,
    http_results: VecDeque<Result<HttpResponse, HostError>>,
    http_requests: Vec<HttpRequest>,
    logs: Vec<(LogLevel, String)>,
}

impl MockHost {
    /// An empty mock host: every call is unstaged and therefore fails.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stage a successful `stat` for `path`.
    #[must_use]
    pub fn with_stat(mut self, path: impl Into<PathBuf>, info: FileInfo) -> Self {
        self.stats.insert(path.into(), Ok(info));
        self
    }

    /// Stage a failing `stat` for `path`.
    #[must_use]
    pub fn with_stat_error(mut self, path: impl Into<PathBuf>, message: impl Into<String>) -> Self {
        self.stats.insert(path.into(), Err(HostError::new(message)));
        self
    }

    /// Stage the next `exec` result, consumed in FIFO order.
    #[must_use]
    pub fn with_exec_result(mut self, result: ExecResult) -> Self {
        self.exec_results.push_back(Ok(result));
        self
    }

    /// Stage the next `exec` failure, consumed in FIFO order.
    #[must_use]
    pub fn with_exec_error(mut self, message: impl Into<String>) -> Self {
        self.exec_results.push_back(Err(HostError::new(message)));
        self
    }

    /// Stage the next `http` result, consumed in FIFO order.
    #[must_use]
    pub fn with_http_response(mut self, response: HttpResponse) -> Self {
        self.http_results.push_back(Ok(response));
        self
    }

    /// Stage the next `http` failure, consumed in FIFO order.
    #[must_use]
    pub fn with_http_error(mut self, message: impl Into<String>) -> Self {
        self.http_results.push_back(Err(HostError::new(message)));
        self
    }

    /// Pre-populate a cache entry.
    #[must_use]
    pub fn with_cache(mut self, key: impl Into<String>, value: impl Into<Vec<u8>>) -> Self {
        self.cache.insert(key.into(), value.into());
        self
    }

    /// All `exec` requests in call order.
    pub fn exec_requests(&self) -> &[ExecRequest] {
        &self.exec_requests
    }

    /// All `http` requests in call order.
    pub fn http_requests(&self) -> &[HttpRequest] {
        &self.http_requests
    }

    /// All `log` calls in call order.
    pub fn logs(&self) -> &[(LogLevel, String)] {
        &self.logs
    }

    /// All `cache_set` calls in call order.
    pub fn cache_sets(&self) -> &[(String, Vec<u8>)] {
        &self.cache_sets
    }

    /// All `cache_read` calls in call order.
    pub fn cache_reads(&self) -> &[(String, u64, u32)] {
        &self.cache_reads
    }

    /// Current cache contents (including pre-populated entries).
    pub fn cache(&self) -> &HashMap<String, Vec<u8>> {
        &self.cache
    }

    /// A default regular-file [`FileInfo`] with the given size.
    pub fn file_info(size: u64) -> FileInfo {
        FileInfo {
            kind: crate::types::FileKind::File,
            size,
            mtime_ms: 0,
            mode: 0o644,
            uid: 0,
            gid: 0,
            nlink: 1,
        }
    }
}

impl Host for MockHost {
    fn stat(&mut self, path: &Path) -> Result<FileInfo, HostError> {
        match self.stats.get(path) {
            Some(result) => result.clone(),
            None => Err(HostError::new(format!(
                "MockHost: no stat staged for {}",
                path.display()
            ))),
        }
    }

    fn exec(&mut self, request: &ExecRequest) -> Result<ExecResult, HostError> {
        self.exec_requests.push(request.clone());
        self.exec_results
            .pop_front()
            .unwrap_or_else(|| Err(HostError::new("MockHost: no exec result staged")))
    }

    fn cache_get(&mut self, key: &str) -> Option<Vec<u8>> {
        self.cache.get(key).cloned()
    }

    fn cache_set(&mut self, key: &str, value: &[u8]) -> Result<(), HostError> {
        self.cache.insert(key.to_owned(), value.to_vec());
        self.cache_sets.push((key.to_owned(), value.to_vec()));
        Ok(())
    }

    fn cache_read(
        &mut self,
        key: &str,
        offset: u64,
        len: u32,
    ) -> Result<Option<Vec<u8>>, HostError> {
        self.cache_reads.push((key.to_owned(), offset, len));
        match self.cache.get(key) {
            None => Ok(None),
            Some(bytes) => Ok(Some(slice_at(bytes, offset, len as usize))),
        }
    }

    fn http(&mut self, request: &HttpRequest) -> Result<HttpResponse, HostError> {
        self.http_requests.push(request.clone());
        self.http_results
            .pop_front()
            .unwrap_or_else(|| Err(HostError::new("MockHost: no http response staged")))
    }

    fn log(&mut self, level: LogLevel, message: &str) {
        self.logs.push((level, message.to_owned()));
    }
}

/// The `[offset, offset + len)` slice of `bytes`, clamped to the end.
fn slice_at(bytes: &[u8], offset: u64, len: usize) -> Vec<u8> {
    let start = (offset as usize).min(bytes.len());
    let end = start.saturating_add(len).min(bytes.len());
    bytes[start..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_results_are_consumed_in_order() {
        let mut host = MockHost::new()
            .with_exec_result(ExecResult {
                code: 0,
                stdout: b"ok".to_vec(),
                stderr: Vec::new(),
                timed_out: false,
            })
            .with_exec_error("boom");
        let request = ExecRequest::new("git").arg("status");

        assert_eq!(host.exec(&request).unwrap().stdout, b"ok");
        assert!(host.exec(&request).is_err());
        assert_eq!(host.exec_requests().len(), 2);
        // No staged result left: the call fails instead of silently succeeding.
        assert!(host.exec(&request).is_err());
    }

    #[test]
    fn cache_and_http_round_trip() {
        let mut host = MockHost::new()
            .with_cache("k", b"v".to_vec())
            .with_http_response(HttpResponse {
                status: 200,
                headers: vec![("content-type".to_owned(), "text/plain".to_owned())],
                body: b"hi".to_vec(),
            });

        assert_eq!(host.cache_get("k"), Some(b"v".to_vec()));
        host.cache_set("k2", b"v2").unwrap();
        assert_eq!(host.cache_sets(), &[("k2".to_owned(), b"v2".to_vec())]);

        let response = host
            .http(&HttpRequest::new("GET", "http://127.0.0.1/rest/db/status"))
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(host.http_requests().len(), 1);
    }

    #[test]
    fn logs_are_recorded() {
        let mut host = MockHost::new();
        host.log(LogLevel::Warn, "careful");
        assert_eq!(host.logs(), &[(LogLevel::Warn, "careful".to_owned())]);
    }

    #[test]
    fn unstaged_stat_reports_a_helpful_error() {
        let mut host = MockHost::new();
        let error = host.stat(Path::new("/missing")).unwrap_err();
        assert!(error.message().contains("/missing"));
    }
}
