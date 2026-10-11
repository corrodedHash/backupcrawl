//! Core crawl logic: filesystem walker, ignore matching and the result model.
//!
//! This crate is transport-agnostic: detectors (WebAssembly plugins from M1 on)
//! will be asked per path through a façade that produces [`PluginReport`]s,
//! while the walker keeps reproducing the original backupcrawl traversal
//! semantics.

pub mod ignore;
pub mod model;
pub mod walker;

pub use model::{CrawlResult, Entry, PluginReport, Status};
