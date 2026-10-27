//! Generated wasmtime host bindings for the canonical `plugin` world.
//!
//! The contract lives in `crates/backupcrawl-wit/wit/detector.wit`; the `path` in
//! [`wasmtime::component::bindgen!`] is relative to this crate's
//! `CARGO_MANIFEST_DIR`, as fixed by the M1 orchestration plan.
//!
//! The generated code is not ours to lint, so the whole module is wrapped in
//! `#[allow(warnings)]` (the same pattern the M1.1 smoke test used). Everything
//! the rest of the crate (and the fixture smoke test) needs is re-exported from
//! here rather than reached through the module path.

#[allow(warnings)]
mod generated {
    wasmtime::component::bindgen!({
        path: "../backupcrawl-wit/wit",
        world: "plugin",
        additional_derives: [PartialEq],
    });
}

pub(crate) use generated::Plugin;
pub(crate) use generated::backupcrawl::detector::host as host_imports;
pub(crate) use generated::backupcrawl::detector::types as wit_types;
pub(crate) use generated::exports::backupcrawl::detector::detector as detector_exports;
