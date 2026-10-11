//! wasmtime-based plugin host.
//!
//! This crate is scaffolding as of M1.1; the modules land with later M1 tasks
//! and must not be implemented before their task starts:
//!
//! - `manifest`, `discovery` and `bundled` — M1.3: `plugin.toml` parsing,
//!   plugin discovery order and the embedded registry.
//! - `runtime`, `imports` and `limits` — M1.4: wasmtime instantiation, host
//!   imports (`stat`/`exec`/`cache`/`http`/`log`) and resource limits.
//! - `trust` — M1.5: sha256 manifest hashing and the trust list.
//! - `WasmDetector` — M1.4/M1.6: the core `Detector` façade backed by a wasm
//!   instance.
//!
//! The WIT contract lives in `crates/backupcrawl-wit/wit`; the host generates
//! its bindings from there with [`wasmtime::component::bindgen!`].
