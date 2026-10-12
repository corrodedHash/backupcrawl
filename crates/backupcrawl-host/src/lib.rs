//! wasmtime-based plugin host.
//!
//! Module status by M1 task:
//!
//! - [`manifest`], [`discovery`] and [`bundled`] — M1.3: `plugin.toml` parsing,
//!   plugin discovery order and the embedded registry.
//! - `runtime`, `imports` and `limits` — M1.4: wasmtime instantiation, host
//!   imports (`stat`/`exec`/`cache`/`http`/`log`) and resource limits.
//! - `trust` — M1.5: sha256 manifest hashing and the trust list.
//! - `WasmDetector` — M1.4/M1.6: the core `Detector` façade backed by a wasm
//!   instance.
//!
//! The WIT contract lives in `crates/backupcrawl-wit/wit`; the host generates
//! its bindings from there with [`wasmtime::component::bindgen!`].

pub mod bundled;
pub mod discovery;
pub mod manifest;
pub mod trust;
