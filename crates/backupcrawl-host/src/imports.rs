//! Host imports of the `backupcrawl:detector/host` interface.
//!
//! This module implements the generated
//! [`Host`](crate::bindings::host_imports::Host) trait on [`HostState`], the
//! per-detector store data. It is the security boundary of the host: every
//! privileged operation goes through a [`CapabilityPolicy`] derived from the
//! plugin's `plugin.toml`, and each import refuses if the manifest did not grant
//! its capability.
//!
//! | Import | Capability | Notes |
//! | --- | --- | --- |
//! | `stat` | *(always)* | `lstat` metadata; never reads content |
//! | `log` | *(always)* | routed to the `log` crate |
//! | `exec` | `[capabilities] exec` | program allowlist + timeout + output caps |
//! | `cache-get`/`cache-set` | `[capabilities] cache` | persistent per-plugin KV |
//! | `http` | `[capabilities] http` | origin allowlist, localhost-only by default |
//!
//! Filesystem *content* is not an import: since M2.0 the runtime grants
//! read-only WASI preopens for the crawl root plus the resolved
//! `[capabilities].fs-read` roots ([`crate::fs`]), so plugins read files with
//! `std::fs` and this module only fills the metadata gap WASI 0.2 leaves.
//!
//! # Trust model
//!
//! `exec` is the strongest grant: a spawned program runs with the user's normal
//! rights and is *not* sandboxed (section 10 of the rewrite plan). The program
//! allowlist and its visibility in `plugins info` are the defense, not process
//! isolation.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, UNIX_EPOCH};

use url::Url;
use wasmtime::StoreLimits;
use wasmtime::component::ResourceTable;
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::bindings::host_imports::{
    ExecRequest, ExecResult, FileInfo, FileKind, Host, HttpRequest, HttpResponse,
};
use crate::fs::{FsGrants, FsReadScope};
use crate::limits::Limits;
use crate::manifest::{Capabilities, Manifest};

/// Which privileged host imports a detector instance may use.
///
/// Built from the manifest's `[capabilities]` table by
/// [`CapabilityPolicy::from_manifest`] (or from [`Capabilities`] directly), or
/// constructed by hand in tests with the `with_*` builders.
///
/// `stat` and `log` are unconditional and therefore not represented here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityPolicy {
    exec: Vec<String>,
    fs_read: Vec<String>,
    http: Vec<String>,
    http_localhost_only: bool,
    cache: bool,
}

impl Default for CapabilityPolicy {
    /// No capability granted; HTTP is localhost-only.
    fn default() -> Self {
        Self {
            exec: Vec::new(),
            fs_read: Vec::new(),
            http: Vec::new(),
            http_localhost_only: true,
            cache: false,
        }
    }
}

impl CapabilityPolicy {
    /// A policy that grants nothing (the safest default).
    pub fn none() -> Self {
        Self::default()
    }

    /// Builds a policy from a manifest's `[capabilities]` table.
    pub fn from_manifest(manifest: &Manifest) -> Self {
        Self::from_capabilities(&manifest.capabilities)
    }

    /// Builds a policy from parsed [`Capabilities`].
    ///
    /// The HTTP posture is localhost-only regardless of the declared origins;
    /// see [`CapabilityPolicy::with_http_localhost_only`] for why and how to
    /// relax it.
    pub fn from_capabilities(capabilities: &Capabilities) -> Self {
        Self {
            exec: capabilities.exec.clone(),
            fs_read: capabilities.fs_read.clone(),
            http: capabilities.http.clone(),
            http_localhost_only: true,
            cache: capabilities.cache,
        }
    }

    /// Replaces the `exec` program allowlist.
    #[must_use]
    pub fn with_exec<I, S>(mut self, programs: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.exec = programs.into_iter().map(Into::into).collect();
        self
    }

    /// Replaces the `fs-read` spec list.
    ///
    /// Specs are resolved to read-only preopens at instantiation (see
    /// [`crate::fs`]); a spec that does not resolve is a warning and grants
    /// nothing. `Runtime::instantiate` grants exactly these roots;
    /// `Runtime::instantiate_plugin` additionally grants the crawl root.
    #[must_use]
    pub fn with_fs_read<I, S>(mut self, specs: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.fs_read = specs.into_iter().map(Into::into).collect();
        self
    }

    /// Replaces the `http` origin allowlist.
    #[must_use]
    pub fn with_http<I, S>(mut self, origins: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.http = origins.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the localhost-only HTTP posture (default `true`).
    ///
    /// With `true` (the default, section 10 of the rewrite plan) an HTTP request
    /// is refused unless its host is loopback, even if the origin appears in the
    /// allowlist. Relaxing it is a deliberate, future configuration decision;
    /// tests use it to exercise non-loopback allowlist matching.
    #[must_use]
    pub fn with_http_localhost_only(mut self, localhost_only: bool) -> Self {
        self.http_localhost_only = localhost_only;
        self
    }

    /// Grants or revokes the persistent cache capability.
    #[must_use]
    pub fn with_cache(mut self, cache: bool) -> Self {
        self.cache = cache;
        self
    }

    /// Whether the persistent cache is granted.
    pub fn cache_enabled(&self) -> bool {
        self.cache
    }

    /// Whether the HTTP posture is localhost-only.
    pub fn is_http_localhost_only(&self) -> bool {
        self.http_localhost_only
    }

    /// The `exec` program allowlist.
    pub fn exec_allowlist(&self) -> &[String] {
        &self.exec
    }

    /// The declared `fs-read` specs, resolved at instantiation.
    pub fn fs_read_specs(&self) -> &[String] {
        &self.fs_read
    }

    /// The `http` origin allowlist.
    pub fn http_allowlist(&self) -> &[String] {
        &self.http
    }

    /// Whether `program` may be executed.
    ///
    /// A program matches when it equals an allowlist entry or when its final
    /// path component does (`/bin/git` matches `git`).
    pub fn allows_program(&self, program: &str) -> bool {
        let basename = Path::new(program)
            .file_name()
            .and_then(|name| name.to_str());
        self.exec
            .iter()
            .any(|allowed| allowed == program || basename == Some(allowed.as_str()))
    }

    /// Whether an HTTP request to `url` passes the allowlist and locality
    /// posture.
    pub fn allows_http(&self, url: &Url) -> bool {
        if self.http_localhost_only && !is_loopback(url) {
            return false;
        }
        self.http
            .iter()
            .any(|allowed| Url::parse(allowed).is_ok_and(|allowed| origin_matches(&allowed, url)))
    }
}

/// Whether `url`'s host is the local machine.
fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Scheme, host, port and path-prefix match (the §10 allowlist semantics).
fn origin_matches(allowed: &Url, requested: &Url) -> bool {
    if allowed.scheme() != requested.scheme() {
        return false;
    }
    // `Url::host_str` is already lowercased for domains and normalized for IPs.
    if allowed.host_str() != requested.host_str() {
        return false;
    }
    if allowed.port_or_known_default() != requested.port_or_known_default() {
        return false;
    }
    requested.path().starts_with(allowed.path())
}

/// The per-detector store data: WASI context plus the custom import state.
///
/// One `HostState` lives inside exactly one wasmtime [`Store`](wasmtime::Store);
/// the store is not `Sync` (section 9), so one instance per worker.
pub struct HostState {
    ctx: WasiCtx,
    table: ResourceTable,
    store_limits: StoreLimits,
    policy: CapabilityPolicy,
    plugin_name: String,
    cache: PluginCache,
    limits: Limits,
}

impl HostState {
    /// Builds the store data for `plugin_name`, granting the policy's declared
    /// `fs-read` roots (no crawl root).
    ///
    /// This is the constructor the runtime uses for the test-oriented
    /// `Runtime::instantiate`; the crawl-aware instantiation goes through
    /// [`HostState::with_grants`]. The WASI context always has an **empty
    /// environment and argument list**. `cache_root` is the base cache
    /// directory (`$XDG_CACHE_HOME/backupcrawl/plugins` by default); each plugin
    /// gets a subdirectory named after itself.
    pub fn new(
        plugin_name: impl Into<String>,
        policy: CapabilityPolicy,
        cache_root: &Path,
        limits: &Limits,
    ) -> anyhow::Result<Self> {
        let scope = FsReadScope::resolve(policy.fs_read_specs());
        Self::with_grants(
            plugin_name,
            policy,
            cache_root,
            limits,
            &FsGrants::for_scope(&scope),
        )
    }

    /// Builds the store data with an explicit set of read-only preopens.
    ///
    /// Every [`FsRoot`](crate::fs::FsRoot) is preopened read-only at its own
    /// absolute guest path, so `std::fs`/`gix` absolute paths keep working.
    /// Roots outside the granted set are invisible to the guest; the metadata
    /// `stat` import stays unconditional.
    pub fn with_grants(
        plugin_name: impl Into<String>,
        policy: CapabilityPolicy,
        cache_root: &Path,
        limits: &Limits,
        grants: &FsGrants,
    ) -> anyhow::Result<Self> {
        let plugin_name = plugin_name.into();

        let mut ctx = WasiCtxBuilder::new();
        // Empty environment/args are the builder default; read-only preopens
        // make writes through `std::fs` fail in `cap-std`.
        for root in grants.roots() {
            ctx.preopened_dir(
                &root.host,
                root.guest.to_string_lossy().as_ref(),
                FsPerms::ReadOnly,
            )?;
        }
        let ctx = ctx.build();

        let cache = PluginCache::new(cache_root.join(percent_encode(&plugin_name)), policy.cache);
        let store_limits = wasmtime::StoreLimitsBuilder::new()
            .memory_size(limits.max_memory)
            .build();

        Ok(Self {
            ctx,
            table: ResourceTable::new(),
            store_limits,
            policy,
            plugin_name,
            cache,
            limits: limits.clone(),
        })
    }

    /// The store limiter, for [`wasmtime::Store::limiter`].
    pub fn limiter(&mut self) -> &mut StoreLimits {
        &mut self.store_limits
    }

    /// The cache directory this detector reads and writes.
    pub fn cache_dir(&self) -> &Path {
        self.cache.dir()
    }
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

impl crate::bindings::wit_types::Host for HostState {}

impl Host for HostState {
    fn stat(&mut self, path: String) -> Result<FileInfo, String> {
        // `symlink_metadata` (lstat) so a symlink reports `symlink` rather than
        // its target's kind; content is never touched.
        let meta = fs::symlink_metadata(&path).map_err(|err| format!("stat `{path}`: {err}"))?;
        Ok(file_info(&meta))
    }

    fn exec(&mut self, request: ExecRequest) -> Result<ExecResult, String> {
        if self.policy.exec_allowlist().is_empty() {
            return Err("exec: capability not granted by the manifest".to_owned());
        }
        if !self.policy.allows_program(&request.program) {
            return Err(format!(
                "exec: program `{}` is not in the allowlist",
                request.program
            ));
        }
        run_exec(request, &self.limits)
    }

    fn cache_get(&mut self, key: String) -> Option<Vec<u8>> {
        self.cache.get(&key)
    }

    fn cache_set(&mut self, key: String, value: Vec<u8>) -> Result<(), String> {
        self.cache.set(&key, &value)
    }

    fn http(&mut self, request: HttpRequest) -> Result<HttpResponse, String> {
        if self.policy.http_allowlist().is_empty() {
            return Err("http: capability not granted by the manifest".to_owned());
        }
        run_http(request, &self.policy, &self.limits)
    }

    fn log(&mut self, level: String, message: String) {
        let name = &self.plugin_name;
        match level.to_ascii_lowercase().as_str() {
            "trace" => log::trace!("[{name}] {message}"),
            "debug" => log::debug!("[{name}] {message}"),
            "warn" | "warning" => log::warn!("[{name}] {message}"),
            "error" => log::error!("[{name}] {message}"),
            _ => log::info!("[{name}] {message}"),
        }
    }
}

/// Builds the WIT `file-info` record from lstat metadata.
fn file_info(meta: &fs::Metadata) -> FileInfo {
    let file_type = meta.file_type();
    let kind = if file_type.is_symlink() {
        FileKind::Symlink
    } else if file_type.is_dir() {
        FileKind::Directory
    } else if file_type.is_file() {
        FileKind::File
    } else {
        FileKind::Other
    };
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|delta| delta.as_millis() as u64)
        .unwrap_or(0);

    FileInfo {
        kind,
        size: meta.len(),
        mtime_ms,
        mode: meta.mode(),
        uid: meta.uid(),
        gid: meta.gid(),
        nlink: meta.nlink(),
    }
}

/// Resolves a program name to an executable path.
///
/// Names containing `/` are used verbatim; bare names are searched on `PATH`.
fn resolve_program(program: &str) -> Option<PathBuf> {
    let path = Path::new(program);
    if program.contains('/') {
        return path.is_file().then(|| path.to_path_buf());
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(program))
            .find(|candidate| candidate.is_file())
    })
}

/// Runs an allowlisted program with timeout and output caps.
///
/// The child starts from an **empty environment** plus exactly the request's
/// `env` entries (the host does not leak its own environment). stdout/stderr are
/// drained on dedicated threads so a chatty child cannot deadlock on a full
/// pipe; the returned bytes are capped at [`Limits::exec_output_cap`].
fn run_exec(request: ExecRequest, limits: &Limits) -> Result<ExecResult, String> {
    let ExecRequest {
        program,
        args,
        cwd,
        env,
        stdin,
        timeout_ms,
    } = request;

    let program_path = resolve_program(&program)
        .ok_or_else(|| format!("exec: cannot find program `{program}`"))?;

    let mut command = Command::new(&program_path);
    command
        .args(&args)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in &env {
        command.env(key, value);
    }
    if let Some(cwd) = &cwd {
        command.current_dir(cwd);
    }

    let mut child = command
        .spawn()
        .map_err(|err| format!("exec: cannot spawn `{program}`: {err}"))?;

    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let child_stdin = child.stdin.take();

    let stdin_writer = match (child_stdin, stdin) {
        (Some(mut handle), Some(data)) => Some(std::thread::spawn(move || {
            let _ = handle.write_all(&data);
            let _ = handle.flush();
        })),
        (Some(handle), None) => {
            drop(handle);
            None
        }
        (None, _) => None,
    };

    let cap = limits.exec_output_cap;
    let out_reader = std::thread::spawn(move || read_capped(stdout, cap));
    let err_reader = std::thread::spawn(move || read_capped(stderr, cap));

    let timeout = timeout_ms.map_or(limits.exec_timeout_ms, u64::from);
    let deadline = Instant::now() + Duration::from_millis(timeout);
    let mut timed_out = false;
    let code;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                code = status.code().unwrap_or(-1);
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    code = -1;
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("exec: waiting for `{program}` failed: {err}"));
            }
        }
    }

    let stdout = out_reader.join().unwrap_or_default();
    let stderr = err_reader.join().unwrap_or_default();
    if let Some(handle) = stdin_writer {
        let _ = handle.join();
    }

    Ok(ExecResult {
        code,
        stdout,
        stderr,
        timed_out,
    })
}

/// Reads a stream to EOF, keeping at most `cap` bytes but draining the rest.
fn read_capped<R: Read>(mut reader: R, cap: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if out.len() < cap {
                    let take = (cap - out.len()).min(n);
                    out.extend_from_slice(&buf[..take]);
                }
            }
        }
    }
    out
}

/// Performs an allowlisted HTTP/1.1 request over plain TCP.
///
/// Only `http` is implemented (no TLS dependency yet); the scheme, host, port
/// and path prefix must match an allowlist origin, and the localhost-only
/// posture is enforced before connecting. **Redirects are never followed** — a
/// 3xx response is returned verbatim — so a response can never send the host to
/// a non-allowlisted origin.
fn run_http(
    request: HttpRequest,
    policy: &CapabilityPolicy,
    limits: &Limits,
) -> Result<HttpResponse, String> {
    let url = Url::parse(&request.url)
        .map_err(|err| format!("http: invalid URL `{}`: {err}", request.url))?;
    if url.scheme() != "http" {
        return Err(format!(
            "http: unsupported scheme `{}` (only plain `http` is implemented)",
            url.scheme()
        ));
    }
    if !policy.allows_http(&url) {
        return Err(format!("http: URL `{}` is not allowlisted", request.url));
    }

    let host = url
        .host_str()
        .ok_or_else(|| format!("http: URL `{}` has no host", request.url))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| format!("http: URL `{}` has no port", request.url))?;
    let address = (host, port)
        .to_socket_addrs()
        .map_err(|err| format!("http: cannot resolve `{host}`: {err}"))?
        .next()
        .ok_or_else(|| format!("http: `{host}` resolved to no address"))?;

    let timeout = Duration::from_millis(limits.http_timeout_ms);
    let mut stream = TcpStream::connect_timeout(&address, timeout)
        .map_err(|err| format!("http: cannot connect to {address}: {err}"))?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));

    let target = match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    };
    let host_header = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };

    let mut head = format!(
        "{} {} HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\n",
        request.method, target
    );
    for (key, value) in &request.headers {
        head.push_str(key);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    if let Some(body) = &request.body {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");

    stream
        .write_all(head.as_bytes())
        .map_err(|err| format!("http: write failed: {err}"))?;
    if let Some(body) = &request.body {
        stream
            .write_all(body)
            .map_err(|err| format!("http: write failed: {err}"))?;
    }
    stream
        .flush()
        .map_err(|err| format!("http: flush failed: {err}"))?;

    let raw = read_capped(&mut stream, limits.http_body_cap.saturating_add(64 * 1024));
    parse_http_response(&raw, limits.http_body_cap)
}

/// Splits a raw HTTP/1.1 response into status, headers and a capped body.
fn parse_http_response(raw: &[u8], body_cap: usize) -> Result<HttpResponse, String> {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n".as_slice())
        .ok_or_else(|| "http: malformed response (no header terminator)".to_owned())?;
    let (head, rest) = raw.split_at(split);
    let body = &rest[4..];

    let head = String::from_utf8_lossy(head);
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let mut parts = status_line.split_whitespace();
    let _version = parts.next();
    let status: u16 = parts
        .next()
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| format!("http: malformed status line `{status_line}`"))?;

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.push((key.trim().to_owned(), value.trim().to_owned()));
        }
    }

    let body = if body.len() > body_cap {
        log::warn!("http: response body truncated to {body_cap} bytes");
        body[..body_cap].to_vec()
    } else {
        body.to_vec()
    };

    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

/// A persistent per-plugin key/value cache rooted at a directory.
///
/// Keys become file names through [`percent_encode`], so arbitrary keys are
/// safe and the directory stays human-readable. Writes go through a temporary
/// file plus `rename` so a reader never sees a half-written value.
///
/// # Single-flight hook (§9)
///
/// Section 9 asks for a single-flight guard so N workers do not repeat an
/// expensive shared preparation (e.g. `pacman -Qkk`) when the first `cache-get`
/// misses. That guard spans `cache-get`→compute→`cache-set`, i.e. state across
/// two guest calls, and only matters once crawling is parallel (M4). The hook is
/// therefore documented here, not implemented: the M4 work adds a keyed
/// `Mutex`/`Condvar` around the miss path.
struct PluginCache {
    dir: PathBuf,
    enabled: bool,
}

impl PluginCache {
    fn new(dir: PathBuf, enabled: bool) -> Self {
        Self { dir, enabled }
    }

    fn dir(&self) -> &Path {
        &self.dir
    }

    fn entry_path(&self, key: &str) -> PathBuf {
        self.dir.join(percent_encode(key))
    }

    fn get(&self, key: &str) -> Option<Vec<u8>> {
        if !self.enabled {
            log::debug!("cache: capability not granted; treating `{key}` as a miss");
            return None;
        }
        match fs::read(self.entry_path(key)) {
            Ok(bytes) => Some(bytes),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                log::warn!("cache: cannot read entry `{key}`: {err}");
                None
            }
        }
    }

    fn set(&self, key: &str, value: &[u8]) -> Result<(), String> {
        if !self.enabled {
            return Err("cache: capability not granted by the manifest".to_owned());
        }
        fs::create_dir_all(&self.dir)
            .map_err(|err| format!("cache: cannot create {}: {err}", self.dir.display()))?;

        let path = self.entry_path(key);
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let tmp = self
            .dir
            .join(format!(".{file_name}.{}.tmp", std::process::id()));

        fs::write(&tmp, value)
            .map_err(|err| format!("cache: cannot write {}: {err}", tmp.display()))?;
        fs::rename(&tmp, &path)
            .map_err(|err| format!("cache: cannot persist {}: {err}", path.display()))?;
        Ok(())
    }
}

/// Encodes arbitrary bytes as a file-system-safe ASCII component.
fn percent_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') {
            out.push(char::from(*byte));
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    fn state(policy: CapabilityPolicy, cache_root: &Path) -> HostState {
        HostState::new("fixture", policy, cache_root, &Limits::default()).expect("host state")
    }

    #[test]
    fn policy_from_manifest_mirrors_declared_capabilities() {
        let manifest = Manifest::parse(
            br#"
            [plugin]
            name = "syncthing"
            version = "0.1.0"
            api = "1"

            [capabilities]
            exec = ["chezmoi"]
            http = ["http://127.0.0.1:8384"]
            cache = true
            "#,
        )
        .expect("manifest");

        let policy = CapabilityPolicy::from_manifest(&manifest);
        assert_eq!(policy.exec_allowlist(), ["chezmoi"]);
        assert_eq!(policy.http_allowlist(), ["http://127.0.0.1:8384"]);
        assert!(policy.cache_enabled());
        assert!(policy.is_http_localhost_only());
        assert!(policy.allows_program("/usr/bin/chezmoi"));
        assert!(!policy.allows_program("git"));
    }

    #[test]
    fn exec_policy_matches_full_path_and_basename() {
        let policy = CapabilityPolicy::none().with_exec(["git", "/usr/bin/chezmoi"]);
        assert!(policy.allows_program("git"));
        assert!(policy.allows_program("/bin/git"));
        assert!(policy.allows_program("/usr/bin/chezmoi"));
        assert!(!policy.allows_program("/usr/bin/chezmoi2"));
        assert!(!policy.allows_program("rm"));
    }

    #[test]
    fn http_policy_checks_scheme_host_port_and_path_prefix() {
        let url = |s: &str| Url::parse(s).unwrap();

        let policy = CapabilityPolicy::none()
            .with_http(["http://127.0.0.1:8384/rest/db"])
            .with_http_localhost_only(false);
        assert!(policy.allows_http(&url("http://127.0.0.1:8384/rest/db/status?folder=x")));
        assert!(!policy.allows_http(&url("http://127.0.0.1:8384/other")));
        assert!(!policy.allows_http(&url("http://127.0.0.1:9999/rest/db")));
        assert!(!policy.allows_http(&url("https://127.0.0.1:8384/rest/db")));
        assert!(!policy.allows_http(&url("http://example.com/rest/db")));
    }

    #[test]
    fn http_posture_defaults_to_localhost_only() {
        let policy =
            CapabilityPolicy::none().with_http(["http://localhost/", "http://example.com/"]);
        assert!(policy.is_http_localhost_only());

        // A loopback host is allowed when it is also allowlisted.
        assert!(policy.allows_http(&Url::parse("http://localhost/").unwrap()));
        // A loopback host that is not allowlisted is still refused.
        assert!(!policy.allows_http(&Url::parse("http://127.0.0.1/").unwrap()));
        // A non-loopback host is refused even though it is allowlisted.
        assert!(!policy.allows_http(&Url::parse("http://example.com/").unwrap()));
    }

    #[test]
    fn gated_imports_refuse_without_the_capability() {
        let dir = tempfile::tempdir().unwrap();
        let mut host = state(CapabilityPolicy::none(), dir.path());

        let err = host
            .exec(ExecRequest {
                program: "/bin/echo".to_owned(),
                args: Vec::new(),
                cwd: None,
                env: Vec::new(),
                stdin: None,
                timeout_ms: None,
            })
            .expect_err("exec must be refused");
        assert!(err.contains("not granted"), "got: {err}");

        assert!(
            host.cache_set("k".to_owned(), b"v".to_vec())
                .expect_err("cache must be refused")
                .contains("not granted")
        );
        assert_eq!(host.cache_get("k".to_owned()), None);

        let err = host
            .http(HttpRequest {
                method: "GET".to_owned(),
                url: "http://127.0.0.1/".to_owned(),
                headers: Vec::new(),
                body: None,
            })
            .expect_err("http must be refused");
        assert!(err.contains("not granted"), "got: {err}");

        // `stat` and `log` are never gated.
        let file = dir.path().join("f");
        fs::write(&file, b"hello").unwrap();
        let info = host.stat(file.display().to_string()).unwrap();
        assert_eq!(info.kind, FileKind::File);
        assert_eq!(info.size, 5);
        host.log("info".to_owned(), "ungated".to_owned());
    }

    #[test]
    fn stat_reports_unix_metadata_and_symlink_kind() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data");
        fs::write(&file, b"abc").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();

        let mut host = state(CapabilityPolicy::none(), dir.path());

        let info = host.stat(file.display().to_string()).unwrap();
        let meta = fs::metadata(&file).unwrap();
        assert_eq!(info.kind, FileKind::File);
        assert_eq!(info.size, 3);
        assert_eq!(info.mode, meta.mode());
        assert_eq!(info.uid, meta.uid());
        assert_eq!(info.gid, meta.gid());
        assert_eq!(info.nlink, meta.nlink());
        assert!(info.mtime_ms > 0);

        let link_info = host.stat(link.display().to_string()).unwrap();
        assert_eq!(link_info.kind, FileKind::Symlink);

        assert!(host.stat("/definitely/not/here".to_owned()).is_err());
    }

    #[test]
    fn exec_reports_exit_code_and_streams() {
        if !Path::new("/bin/sh").exists() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut host = state(CapabilityPolicy::none().with_exec(["sh"]), dir.path());

        let result = host
            .exec(ExecRequest {
                program: "/bin/sh".to_owned(),
                args: vec![
                    "-c".to_owned(),
                    "printf out; printf err 1>&2; exit 3".to_owned(),
                ],
                cwd: None,
                env: Vec::new(),
                stdin: None,
                timeout_ms: None,
            })
            .unwrap();

        assert_eq!(result.code, 3);
        assert_eq!(result.stdout, b"out");
        assert_eq!(result.stderr, b"err");
        assert!(!result.timed_out);
    }

    #[test]
    fn exec_times_out() {
        if !Path::new("/bin/sleep").exists() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut host = state(CapabilityPolicy::none().with_exec(["sleep"]), dir.path());

        let result = host
            .exec(ExecRequest {
                program: "/bin/sleep".to_owned(),
                args: vec!["5".to_owned()],
                cwd: None,
                env: Vec::new(),
                stdin: None,
                timeout_ms: Some(100),
            })
            .unwrap();

        assert!(result.timed_out);
        assert_eq!(result.code, -1);
    }

    #[test]
    fn cache_round_trips_and_persists_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        let policy = CapabilityPolicy::none().with_cache(true);

        let mut first = state(policy.clone(), dir.path());
        assert_eq!(first.cache_get("answer".to_owned()), None);
        first
            .cache_set("answer".to_owned(), b"42".to_vec())
            .unwrap();
        assert_eq!(first.cache_get("answer".to_owned()), Some(b"42".to_vec()));

        // A fresh instance with the same cache root sees the persisted value.
        let mut second = state(policy, dir.path());
        assert_eq!(second.cache_get("answer".to_owned()), Some(b"42".to_vec()));

        // Keys with separators are encoded, not treated as paths.
        second
            .cache_set("../escape".to_owned(), b"x".to_vec())
            .unwrap();
        assert_eq!(
            second.cache_get("../escape".to_owned()),
            Some(b"x".to_vec())
        );
    }

    #[test]
    fn http_get_and_no_redirect_following() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for body in [
                "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
                "HTTP/1.1 302 Found\r\nLocation: http://evil.example/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut scratch = [0u8; 1024];
                let _ = stream.read(&mut scratch);
                let _ = stream.write_all(body.as_bytes());
            }
        });

        let dir = tempfile::tempdir().unwrap();
        let mut host = state(
            CapabilityPolicy::none().with_http([format!("http://{addr}/")]),
            dir.path(),
        );

        let response = host
            .http(HttpRequest {
                method: "GET".to_owned(),
                url: format!("http://{addr}/status"),
                headers: Vec::new(),
                body: None,
            })
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"hello");
        assert!(
            response
                .headers
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("content-length") && v == "5")
        );

        // A redirect is returned as-is, never followed.
        let redirect = host
            .http(HttpRequest {
                method: "GET".to_owned(),
                url: format!("http://{addr}/redirect"),
                headers: Vec::new(),
                body: None,
            })
            .unwrap();
        assert_eq!(redirect.status, 302);
        assert!(
            redirect
                .headers
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("location") && v == "http://evil.example/")
        );

        server.join().unwrap();
    }

    #[test]
    fn http_rejects_non_allowlisted_and_non_http() {
        let dir = tempfile::tempdir().unwrap();
        let mut host = state(
            CapabilityPolicy::none().with_http(["http://127.0.0.1:1/"]),
            dir.path(),
        );

        let err = host
            .http(HttpRequest {
                method: "GET".to_owned(),
                url: "http://127.0.0.1:2/".to_owned(),
                headers: Vec::new(),
                body: None,
            })
            .expect_err("non-allowlisted port");
        assert!(err.contains("not allowlisted"), "got: {err}");

        let err = host
            .http(HttpRequest {
                method: "GET".to_owned(),
                url: "https://127.0.0.1:1/".to_owned(),
                headers: Vec::new(),
                body: None,
            })
            .expect_err("https unsupported");
        assert!(err.contains("unsupported scheme"), "got: {err}");
    }
}
