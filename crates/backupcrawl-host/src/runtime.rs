//! wasmtime detector runtime and the `WasmDetector` façade.
//!
//! [`Runtime`] owns the shared wasmtime [`Engine`] (fuel + epoch interruption
//! configured once), the component [`Linker`], the resource [`Limits`], the
//! cancellation flag and the epoch watchdog. [`Runtime::instantiate`] compiles a
//! detector component and builds a [`WasmDetector`]: **one [`Store`] per
//! detector** (a store is not `Sync`, section 9), with read-only preopens for
//! the granted filesystem roots ([`crate::fs`]) and an environment limited to
//! the path variables (`HOME`, `STCONFDIR`, `XDG_*`) detectors need to resolve
//! their config roots.
//!
//! [`WasmDetector`] implements the core
//! [`Detector`](backupcrawl_core::detector::Detector) trait. Its inherent API
//! (`init`, `check_dir`, `check_file`, `check_symlink`, `check_files`,
//! `finish`) maps 1:1 onto the trait, and both use the transport-neutral records
//! defined in `backupcrawl_core::detector` (this crate re-exports them). It owns
//! the lifecycle and failure accounting from section 5.4: a guest `Err`/trap
//! becomes "no claim", consecutive failures trip a circuit breaker, an `init`
//! error disables the plugin for the scan, and [`Detector::report`] surfaces
//! errors, notes and the disable reason for the run report.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{Cache, CacheConfig};
use wasmtime::{Config as WasmConfig, Engine, Store, UpdateDeadline};
use wasmtime_wasi::p2;

use backupcrawl_core::Status;
use backupcrawl_core::detector::{
    Attr, Claim, Detector, DetectorError, DetectorReport, LinkInfo, ScanInfo,
};

use crate::bindings::{Plugin, detector_exports, wit_types};
use crate::fs::{FsGrants, FsReadScope};
use crate::imports::{CapabilityPolicy, HostState};
use crate::limits::Limits;

/// Shared runtime: engine, linker, limits and the epoch watchdog.
///
/// A `Runtime` outlives the [`WasmDetector`]s instantiated from it. Dropping the
/// runtime stops the epoch watchdog; detectors still run, but cooperative
/// cancellation no longer advances the epoch.
pub struct Runtime {
    engine: Engine,
    linker: Linker<HostState>,
    limits: Limits,
    cache_root: PathBuf,
    cancel: Arc<AtomicBool>,
    /// Compiled components keyed by the SHA-256 of their wasm bytes.
    ///
    /// Compiling a detector component is expensive (seconds for the git
    /// component), so the same plugin is compiled once per [`Runtime`] and
    /// every [`Runtime::instantiate`] reuses the compiled artifact. Without
    /// this, each instantiation — and each of the many detector instances a
    /// scan or a test binary builds — pays the full Cranelift cost again.
    components: Mutex<HashMap<[u8; 32], Arc<Component>>>,
    /// Kept alive for its `Drop`; the watchdog stops when the runtime drops.
    _watchdog: Watchdog,
}

impl Runtime {
    /// Creates a runtime with the given limits and the standard cache root
    /// (`$XDG_CACHE_HOME/backupcrawl/plugins`).
    pub fn new(limits: Limits) -> Result<Self> {
        Self::with_cache_root(limits, default_cache_root())
    }

    /// Creates a runtime whose per-plugin cache lives under `cache_root` and
    /// whose compiled-component cache lives under `cache_root/_components`.
    pub fn with_cache_root(limits: Limits, cache_root: impl Into<PathBuf>) -> Result<Self> {
        let cache_root = cache_root.into();
        let component_cache = cache_root.join("_components");
        Self::with_cache_dirs(limits, cache_root, component_cache)
    }

    /// Creates a runtime with explicit cache locations.
    ///
    /// `cache_root` holds the per-plugin KV caches; `component_cache` holds the
    /// wasmtime on-disk compilation cache. Keeping them separable lets callers
    /// (for example tests) isolate per-plugin state while still sharing the
    /// expensive compiled artifacts.
    pub fn with_cache_dirs(
        limits: Limits,
        cache_root: impl Into<PathBuf>,
        component_cache: impl Into<PathBuf>,
    ) -> Result<Self> {
        let cache_root = cache_root.into();
        let mut config = WasmConfig::new();
        config.consume_fuel(true);
        config.epoch_interruption(true);
        // Enable wasmtime's on-disk component cache. Compiling the bundled git
        // component takes seconds in a debug build, and a fresh process (every
        // `backupcrawl` run, every CLI test) would otherwise pay that cost
        // again. The cache is keyed by the wasm bytes and the engine
        // configuration, so a stale or foreign entry is never used. A failure
        // to open the cache is not fatal: the in-memory cache and recompilation
        // still work.
        match Cache::new(component_cache_config(component_cache.into())) {
            Ok(cache) => {
                config.cache(Some(cache));
            }
            Err(err) => log::warn!("wasmtime on-disk component cache disabled: {err:#}"),
        }
        let engine = Engine::new(&config)
            .map_err(anyhow::Error::from)
            .context("creating the wasmtime engine")?;

        let mut linker = Linker::new(&engine);
        p2::add_to_linker_sync(&mut linker)
            .map_err(anyhow::Error::from)
            .context("linking WASI")?;
        Plugin::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
            .map_err(anyhow::Error::from)
            .context("linking the detector host imports")?;

        let cancel = Arc::new(AtomicBool::new(false));
        let watchdog = Watchdog::start(
            &engine,
            Duration::from_millis(limits.epoch_interval_ms.max(1)),
        );

        Ok(Self {
            engine,
            linker,
            limits,
            cache_root,
            cancel,
            components: Mutex::new(HashMap::new()),
            _watchdog: watchdog,
        })
    }

    /// The configured limits.
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// The cache root shared by detectors from this runtime.
    pub fn cache_root(&self) -> &Path {
        &self.cache_root
    }

    /// Requests cancellation of running detector calls.
    ///
    /// The epoch watchdog checks this flag roughly every
    /// [`Limits::epoch_interval_ms`], so a runaway call traps shortly after; the
    /// runtime records it as a per-plugin error.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Clears a previous [`Runtime::cancel`] request.
    pub fn reset_cancel(&self) {
        self.cancel.store(false, Ordering::Relaxed);
    }

    /// Compiles and instantiates `wasm` as a detector named `name`.
    ///
    /// The compiled component is cached on the runtime (keyed by the wasm
    /// bytes), so repeated instantiations of the same plugin — and the many
    /// instances a scan or test builds — reuse one compilation.
    ///
    /// Filesystem content is granted from the policy's declared `fs-read` specs
    /// (resolved against the live filesystem). This entry point has no crawl
    /// context, so it does **not** grant a crawl root; the CLI scan uses
    /// [`Runtime::instantiate_plugin`] instead, and tests that need an explicit
    /// root use [`Runtime::instantiate_with_fs`].
    pub fn instantiate(
        &self,
        wasm: &[u8],
        name: &str,
        policy: CapabilityPolicy,
    ) -> Result<WasmDetector> {
        let scope = FsReadScope::resolve(policy.fs_read_specs());
        log_unresolved(name, &scope);
        self.instantiate_with_fs(wasm, name, policy, &FsGrants::for_scope(&scope))
    }

    /// Compiles and instantiates `wasm` with explicit filesystem grants.
    ///
    /// `grants` replaces the policy-derived roots entirely, so a test can mount
    /// exactly the directories it needs (for example a temp fixture root). The
    /// policy still carries the non-filesystem capabilities.
    pub fn instantiate_with_fs(
        &self,
        wasm: &[u8],
        name: &str,
        policy: CapabilityPolicy,
        grants: &FsGrants,
    ) -> Result<WasmDetector> {
        let component = self.compiled_component(wasm, name)?;

        let state = HostState::with_grants(name, policy, &self.cache_root, &self.limits, grants)?;
        let mut store = Store::new(&self.engine, state);
        store.limiter(|state| state.limiter());
        store
            .set_fuel(self.limits.fuel_per_call)
            .map_err(anyhow::Error::from)
            .context("arming the detector fuel budget")?;

        let cancel = Arc::clone(&self.cancel);
        store.epoch_deadline_callback(move |_ctx| {
            if cancel.load(Ordering::Relaxed) {
                Err(wasmtime::format_err!("detector cancelled"))
            } else {
                // The watchdog advances the epoch every interval; extend the
                // deadline so normal calls are never interrupted, but keep
                // checking (cooperative cancellation).
                Ok(UpdateDeadline::Continue(1))
            }
        });
        store.set_epoch_deadline(1);

        let plugin = Plugin::instantiate(&mut store, component.as_ref(), &self.linker)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("instantiating detector `{name}`"))?;

        Ok(WasmDetector::new(
            name.to_owned(),
            store,
            plugin,
            self.limits.clone(),
        ))
    }

    /// Returns the compiled component for `wasm`, compiling it on first use.
    fn compiled_component(&self, wasm: &[u8], name: &str) -> Result<Arc<Component>> {
        let key: [u8; 32] = Sha256::digest(wasm).into();
        let mut components = self
            .components
            .lock()
            .expect("the component cache mutex is never poisoned");
        if let Some(component) = components.get(&key) {
            return Ok(Arc::clone(component));
        }
        let component = Arc::new(
            Component::new(&self.engine, wasm)
                .map_err(anyhow::Error::from)
                .with_context(|| format!("compiling detector `{name}`"))?,
        );
        components.insert(key, Arc::clone(&component));
        Ok(component)
    }

    /// Instantiates a discovered plugin for a scan rooted at `crawl_root`.
    ///
    /// The capability policy comes from the manifest; the filesystem grants are
    /// the absolute `crawl_root` plus every resolved `fs-read` spec, so ancestor
    /// or system-wide reads need an explicit broad spec (`git` and `pacman`
    /// declare `/`). Unresolved specs are logged as warnings and the plugin
    /// still loads.
    pub fn instantiate_plugin(
        &self,
        plugin: &crate::discovery::DiscoveredPlugin,
        crawl_root: &Path,
    ) -> Result<WasmDetector> {
        let policy = CapabilityPolicy::from_manifest(&plugin.manifest);
        let scope = FsReadScope::resolve(policy.fs_read_specs());
        log_unresolved(&plugin.name, &scope);
        self.instantiate_with_fs(
            &plugin.wasm_bytes,
            &plugin.name,
            policy,
            &FsGrants::for_scan(crawl_root, &scope),
        )
    }
}

/// Warns about `fs-read` specs that resolved to nothing; the plugin still loads.
fn log_unresolved(name: &str, scope: &FsReadScope) {
    for spec in &scope.unresolved {
        log::warn!("plugin `{name}`: fs-read spec `{spec}` resolved to no existing directory");
    }
}

/// The standard per-plugin cache root.
pub fn default_cache_root() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("backupcrawl")
        .join("plugins")
}

/// The wasmtime on-disk component cache configuration.
///
/// Component compilation is the expensive part of a scan and the entries are
/// content-addressed, so keeping them around is cheap insurance for the next
/// run.
fn component_cache_config(directory: PathBuf) -> CacheConfig {
    let mut config = CacheConfig::new();
    config.with_directory(directory);
    config
}

/// A wasmtime component instance driving one detector.
pub struct WasmDetector {
    name: String,
    store: Store<HostState>,
    plugin: Plugin,
    limits: Limits,
    initialized: bool,
    finished: bool,
    error_count: u32,
    consecutive_failures: u32,
    call_count: u64,
    disabled_reason: Option<String>,
    notes: Vec<String>,
}

impl WasmDetector {
    fn new(name: String, store: Store<HostState>, plugin: Plugin, limits: Limits) -> Self {
        Self {
            name,
            store,
            plugin,
            limits,
            initialized: false,
            finished: false,
            error_count: 0,
            consecutive_failures: 0,
            call_count: 0,
            disabled_reason: None,
            notes: Vec::new(),
        }
    }

    /// The detector name (manifest `name`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether the plugin has been disabled for the rest of the scan.
    pub fn is_disabled(&self) -> bool {
        self.disabled_reason.is_some()
    }

    /// The disable reason, if any.
    pub fn disable_reason(&self) -> Option<&str> {
        self.disabled_reason.as_deref()
    }

    /// Number of failed guest calls recorded so far.
    pub fn errors(&self) -> u32 {
        self.error_count
    }

    /// Human-readable notes accumulated during the lifecycle.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// A snapshot for the run report.
    pub fn report(&self) -> DetectorReport {
        DetectorReport {
            name: self.name.clone(),
            calls: self.call_count,
            errors: self.error_count,
            disabled: self.disabled_reason.clone(),
            notes: self.notes.clone(),
        }
    }

    /// The cache directory this detector uses.
    pub fn cache_dir(&self) -> &Path {
        self.store.data().cache_dir()
    }

    /// Calls the guest `init` exactly once.
    pub fn init(&mut self, info: &ScanInfo) -> Result<(), DetectorError> {
        if self.initialized {
            return Err(DetectorError::Host(format!(
                "detector `{}` was already initialized",
                self.name
            )));
        }
        let wit_info = detector_exports::ScanInfo {
            root: info.root.to_string_lossy().into_owned(),
            user: info.user.clone(),
            follow_symlinks: info.follow_symlinks,
            config: info
                .config
                .iter()
                .map(|(key, value)| wit_types::Attr {
                    key: key.clone(),
                    value: value.clone(),
                })
                .collect(),
        };

        self.refill()?;
        self.call_count += 1;
        let detector = self.detector();
        match detector.call_init(&mut self.store, &wit_info) {
            Ok(Ok(())) => {
                self.initialized = true;
                Ok(())
            }
            Ok(Err(message)) => {
                let reason = format!("init failed: {message}");
                self.disable(reason.clone());
                Err(DetectorError::Disabled {
                    name: self.name.clone(),
                    reason,
                })
            }
            Err(trap) => {
                let reason = format!("init trapped: {trap:#}");
                self.disable(reason.clone());
                Err(DetectorError::Disabled {
                    name: self.name.clone(),
                    reason,
                })
            }
        }
    }

    /// Calls the guest `check-dir`.
    pub fn check_dir(&mut self, path: &Path) -> Result<Option<Claim>, DetectorError> {
        self.ready()?;
        let path = path.to_string_lossy().into_owned();
        self.refill()?;
        let detector = self.detector();
        let outcome = detector.call_check_dir(&mut self.store, &path);
        Ok(self.record(outcome))
    }

    /// Calls the guest `check-file`.
    pub fn check_file(&mut self, path: &Path) -> Result<Option<Claim>, DetectorError> {
        self.ready()?;
        let path = path.to_string_lossy().into_owned();
        self.refill()?;
        let detector = self.detector();
        let outcome = detector.call_check_file(&mut self.store, &path);
        Ok(self.record(outcome))
    }

    /// Calls the guest `check-symlink`.
    pub fn check_symlink(&mut self, link: &LinkInfo) -> Result<Option<Claim>, DetectorError> {
        self.ready()?;
        let wit_link = detector_exports::LinkInfo {
            path: link.path.to_string_lossy().into_owned(),
            target: link.target.to_string_lossy().into_owned(),
            dangling: link.dangling,
        };
        self.refill()?;
        let detector = self.detector();
        let outcome = detector.call_check_symlink(&mut self.store, &wit_link);
        Ok(self.record(outcome))
    }

    /// Checks a batch of files.
    ///
    /// The frozen M1 decision is that the host keeps using singular calls until
    /// M4, so this simply loops [`WasmDetector::check_file`] and therefore
    /// behaves exactly like N singular calls.
    pub fn check_files(&mut self, paths: &[PathBuf]) -> Result<Vec<Option<Claim>>, DetectorError> {
        let mut claims = Vec::with_capacity(paths.len());
        for path in paths {
            claims.push(self.check_file(path)?);
        }
        Ok(claims)
    }

    /// Calls the guest `finish`.
    ///
    /// A disabled plugin skips the guest call. Guest errors are counted and
    /// recorded as notes (and returned) so the run report can surface them.
    pub fn finish(&mut self) -> Result<(), DetectorError> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        if self.is_disabled() {
            return Ok(());
        }
        self.refill()?;
        self.call_count += 1;
        let detector = self.detector();
        match detector.call_finish(&mut self.store) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(message)) => {
                let note = format!("finish failed: {message}");
                self.error_count += 1;
                self.notes.push(note.clone());
                self.consecutive_failures += 1;
                Err(DetectorError::Host(note))
            }
            Err(trap) => {
                let note = format!("finish trapped: {trap:#}");
                self.error_count += 1;
                self.notes.push(note.clone());
                self.consecutive_failures += 1;
                Err(DetectorError::Host(note))
            }
        }
    }

    /// A cheap clone of the generated detector export handle.
    ///
    /// The handle is a set of component function references; cloning it avoids
    /// holding a borrow on `self.plugin` across the `&mut self.store` call.
    fn detector(&self) -> detector_exports::Guest {
        self.plugin.backupcrawl_detector_detector().clone()
    }

    /// Refills the per-call fuel budget and re-arms the epoch deadline.
    fn refill(&mut self) -> Result<(), DetectorError> {
        self.store
            .set_fuel(self.limits.fuel_per_call)
            .map_err(|err| {
                DetectorError::Host(format!("arming the detector fuel budget: {err:#}"))
            })?;
        self.store.set_epoch_deadline(1);
        Ok(())
    }

    /// Rejects calls on a disabled or not-yet-initialized detector.
    fn ready(&self) -> Result<(), DetectorError> {
        if let Some(reason) = &self.disabled_reason {
            return Err(DetectorError::Disabled {
                name: self.name.clone(),
                reason: reason.clone(),
            });
        }
        if !self.initialized {
            return Err(DetectorError::Host(format!(
                "detector `{}` was not initialized",
                self.name
            )));
        }
        if self.finished {
            return Err(DetectorError::Host(format!(
                "detector `{}` has already finished",
                self.name
            )));
        }
        Ok(())
    }

    /// Records the outcome of a check call and maps guest failures to "no
    /// claim".
    fn record(
        &mut self,
        outcome: wasmtime::Result<Result<Option<detector_exports::Claim>, String>>,
    ) -> Option<Claim> {
        self.call_count += 1;
        match outcome {
            Ok(Ok(claim)) => {
                self.consecutive_failures = 0;
                claim.map(convert_claim)
            }
            Ok(Err(message)) => {
                self.fail(format!("guest error: {message}"));
                None
            }
            Err(trap) => {
                self.fail(format!("trap: {trap:#}"));
                None
            }
        }
    }

    /// Applies the section 5.4 failure accounting and circuit breaker.
    fn fail(&mut self, message: String) {
        self.error_count += 1;
        self.consecutive_failures += 1;
        log::warn!("detector `{}`: {message}", self.name);
        if !self.is_disabled() && self.consecutive_failures >= self.limits.max_consecutive_failures
        {
            let reason = format!(
                "disabled after {} consecutive failures",
                self.consecutive_failures
            );
            self.disable(reason);
        }
    }

    /// Disables the plugin for the rest of the scan, recording a visible note.
    fn disable(&mut self, reason: String) {
        log::warn!("detector `{}` disabled: {reason}", self.name);
        self.notes.push(format!("disabled: {reason}"));
        self.disabled_reason = Some(reason);
    }
}

/// The wasm host is the production implementation of the core façade.
///
/// The inherent methods above already return core-shaped records, so the trait
/// implementation is a thin delegation; the fully-qualified calls avoid
/// recursing into the trait methods.
impl Detector for WasmDetector {
    fn init(&mut self, info: &ScanInfo) -> Result<(), DetectorError> {
        WasmDetector::init(self, info)
    }

    fn check_dir(&mut self, path: &Path) -> Result<Option<Claim>, DetectorError> {
        WasmDetector::check_dir(self, path)
    }

    fn check_file(&mut self, path: &Path) -> Result<Option<Claim>, DetectorError> {
        WasmDetector::check_file(self, path)
    }

    fn check_symlink(&mut self, link: &LinkInfo) -> Result<Option<Claim>, DetectorError> {
        WasmDetector::check_symlink(self, link)
    }

    fn finish(&mut self) -> Result<(), DetectorError> {
        WasmDetector::finish(self)
    }

    fn report(&self) -> DetectorReport {
        WasmDetector::report(self)
    }
}

/// Converts a generated claim into the core-shaped [`Claim`].
fn convert_claim(claim: detector_exports::Claim) -> Claim {
    let status = match claim.status {
        wit_types::Status::Clean => Status::Clean,
        wit_types::Status::Dirty => Status::Dirty,
        wit_types::Status::Ahead => Status::Ahead,
        wit_types::Status::Unknown => Status::Unknown,
        wit_types::Status::Ignored => Status::Ignored,
    };
    Claim {
        status,
        attrs: claim
            .attrs
            .into_iter()
            .map(|attr| Attr {
                key: attr.key,
                value: attr.value,
            })
            .collect(),
        prune: claim.prune,
    }
}

/// Advances the engine epoch on a bounded interval so epoch-based cancellation
/// can interrupt a runaway detector call.
struct Watchdog {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
    thread: thread::Thread,
}

impl Watchdog {
    fn start(engine: &Engine, interval: Duration) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let engine = engine.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = thread::spawn(move || {
            let _ = tx.send(thread::current());
            while !thread_stop.load(Ordering::Relaxed) {
                thread::park_timeout(interval);
                if thread_stop.load(Ordering::Relaxed) {
                    break;
                }
                engine.increment_epoch();
            }
        });
        let thread = rx.recv().expect("watchdog thread handle");
        Self {
            stop,
            handle: Some(handle),
            thread,
        }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.unpark();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The core `Detector` trait is `Send` and the M1.6 walker may share the
    /// runtime across worker threads, so lock in the auto-trait guarantees here.
    #[test]
    fn runtime_is_send_sync_and_detector_is_send() {
        fn assert_send<T: Send>() {}
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<Runtime>();
        assert_send::<WasmDetector>();
    }
}
