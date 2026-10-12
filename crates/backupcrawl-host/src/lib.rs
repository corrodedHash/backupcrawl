//! wasmtime-based plugin host.
//!
//! Module status by M1 task:
//!
//! - [`manifest`], [`discovery`] and [`bundled`] — M1.3: `plugin.toml` parsing,
//!   plugin discovery order and the embedded registry.
//! - [`runtime`], [`imports`] and [`limits`] — M1.4: wasmtime instantiation, host
//!   imports (`stat`/`exec`/`cache`/`http`/`log`) and resource limits.
//! - `trust` — M1.5: sha256 manifest hashing and the trust list.
//! - [`WasmDetector`] — M1.4/M1.6: the core `Detector` façade backed by a wasm
//!   instance.
//!
//! The WIT contract lives in `crates/backupcrawl-wit/wit`; [`bindings`] generates
//! the host bindings from there with [`wasmtime::component::bindgen!`].
//!
//! The M1.4 entry points are [`Runtime`] (shared engine/limits/cancellation),
//! [`WasmDetector`] (one store per detector) and [`CapabilityPolicy`] (the
//! per-manifest import grants). [`WasmDetector`] implements the core
//! [`Detector`] trait; the shared records are re-exported from
//! `backupcrawl_core::detector` so downstream code has one type set.

mod bindings;

pub mod bundled;
pub mod discovery;
pub mod imports;
pub mod limits;
pub mod manifest;
pub mod runtime;
pub mod trust;

pub use backupcrawl_core::detector::{
    Attr, Claim, Detector, DetectorError, DetectorReport, Hooks, LinkInfo, ScanInfo,
};
pub use imports::CapabilityPolicy;
pub use limits::Limits;
pub use runtime::{Runtime, WasmDetector};
