//! Detector authoring SDK for backupcrawl plugins.
//!
//! A detector is written in native Rust against the [`Detector`] trait and the
//! [`Host`] service trait, then turned into a WebAssembly component with
//! [`bind_detector!`]. Because the logic only touches native records and the
//! [`Host`] trait, it is unit-testable with [`MockHost`] under plain
//! `cargo test` — no wasm toolchain required.
//!
//! # Writing a detector
//!
//! ```ignore
//! use std::path::Path;
//! use backupcrawl_plugin_sdk::{
//!     bind_detector, Claim, Detector, DetectorError, Host, LinkInfo, ScanInfo,
//! };
//!
//! #[derive(Default)]
//! struct Git {
//!     report_ignored: bool,
//! }
//!
//! impl Detector for Git {
//!     fn init(&mut self, info: &ScanInfo, _host: &mut dyn Host) -> Result<(), DetectorError> {
//!         self.report_ignored = info.config.get("report-ignored") == Some("true");
//!         Ok(())
//!     }
//!
//!     fn check_file(
//!         &mut self,
//!         path: &Path,
//!         _host: &mut dyn Host,
//!     ) -> Result<Option<Claim>, DetectorError> {
//!         if path.file_name().and_then(|n| n.to_str()) == Some(".git") {
//!             Ok(Some(Claim::clean().attr("repo", "true")))
//!         } else {
//!             Ok(None)
//!         }
//!     }
//! }
//!
//! bind_detector!(Git);
//! ```
//!
//! # Plugin crate setup
//!
//! The macro expands to
//! `wit_bindgen::generate!({ path: "wit", world: "plugin" })`, resolving `wit`
//! relative to the plugin crate's `CARGO_MANIFEST_DIR`. Guest crates therefore
//! carry a `wit` symlink to the canonical contract, exactly like
//! `crates/test-components`:
//!
//! ```text
//! ln -s ../../crates/backupcrawl-wit/wit wit
//! ```
//!
//! and depend only on this SDK:
//!
//! ```toml
//! [dependencies]
//! backupcrawl-plugin-sdk = { path = "../../crates/backupcrawl-plugin-sdk" }
//! ```
//!
//! The crate should be a `cdylib` and declare the component target:
//!
//! ```toml
//! [lib]
//! crate-type = ["cdylib"]
//!
//! [package.metadata.component]
//! target = { path = "wit", world = "plugin" }
//! ```
//!
//! Build with `cargo component build --release` (cargo-component 0.21.1 builds
//! `wasm32-wasip1` and componentizes it with the preview1 adapter; the artifact
//! lands in `<CARGO_TARGET_DIR>/wasm32-wasip1/release/<crate>.wasm`). Do not
//! pass `--target wasm32-wasip2` to `cargo component build`.
//!
//! [`bind_detector!`]: crate::bind_detector

mod bind;
pub mod detector;
pub mod error;
pub mod host;
pub mod mock;
pub mod types;

pub use detector::Detector;
pub use error::{DetectorError, HostError};
pub use host::Host;
pub use mock::MockHost;
pub use types::{
    Attr, Claim, Config, ExecRequest, ExecResult, FileInfo, FileKind, HttpRequest, HttpResponse,
    LinkInfo, LogLevel, ScanInfo, Status, UnknownStatus,
};

/// Re-export of the pinned wit-bindgen runtime used by the generated glue.
///
/// The [`bind_detector!`](crate::bind_detector) macro points wit-bindgen's
/// `runtime_path` here so a plugin crate does not need a direct `wit-bindgen`
/// dependency. Because the path is spelled in generated code as
/// `backupcrawl_plugin_sdk::rt`, a plugin must depend on this crate under its
/// canonical name (no `package = "..."` rename).
#[doc(hidden)]
pub use wit_bindgen::rt;

/// Re-export of the pinned `wit_bindgen::generate!` used by the macro.
#[doc(hidden)]
pub use wit_bindgen::generate as _wit_bindgen_generate;
