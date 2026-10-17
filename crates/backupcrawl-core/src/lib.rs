//! Core crawl logic: filesystem walker, ignore matching and the result model.
//!
//! This crate is transport-agnostic: detectors (WebAssembly plugins from M1 on)
//! are asked per path through the [`detector::Detector`] façade, which produces
//! [`PluginReport`]s, while the walker keeps reproducing the original
//! backupcrawl traversal semantics.

pub mod detector;
pub mod ignore;
pub mod model;
pub mod walker;

pub use detector::{
    Attr, Claim, Detector, DetectorError, DetectorReport, DetectorSlot, Hooks, LinkInfo, ScanInfo,
};
pub use model::{CrawlResult, Entry, PluginReport, Status, SymlinkEntry};
