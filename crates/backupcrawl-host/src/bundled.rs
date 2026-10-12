//! Embedded registry of bundled detectors.
//!
//! Bundled detectors are compiled into the binary with `include_bytes!` so a
//! standalone `cargo install backupcrawl` works without a plugin directory.
//! External plugins with the same name override them (see
//! [`crate::discovery`]).
//!
//! # Status
//!
//! The registry is **empty as of M1.3**. M1.8 fills it with the git detector:
//! build `plugins/git` with `cargo component build --release`, copy the
//! component to the documented artifact path `plugins/git/dist/git.wasm` and
//! its manifest to `plugins/git/dist/plugin.toml`, then add an entry to
//! [`REGISTRY`]:
//!
//! ```ignore
//! BundledPlugin {
//!     name: "git",
//!     manifest: include_bytes!("../../../plugins/git/dist/plugin.toml"),
//!     wasm: include_bytes!("../../../plugins/git/dist/git.wasm"),
//! }
//! ```
//!
//! `include_bytes!` paths are relative to this source file
//! (`crates/backupcrawl-host/src/bundled.rs`), so `../../../plugins/...`
//! resolves to the workspace's `plugins/` directory.

/// A detector shipped inside the binary.
#[derive(Debug, Clone, Copy)]
pub struct BundledPlugin {
    /// Registry key; must match the manifest's `[plugin].name`.
    pub name: &'static str,
    /// Raw `plugin.toml` bytes.
    pub manifest: &'static [u8],
    /// Raw wasm component bytes.
    pub wasm: &'static [u8],
}

/// All bundled detectors, in a stable order.
pub const REGISTRY: &[BundledPlugin] = &[];

/// All bundled detectors, in a stable order.
pub fn all() -> &'static [BundledPlugin] {
    REGISTRY
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_consistent() {
        // Duplicate registry keys would silently shadow each other.
        let mut names: Vec<&str> = all().iter().map(|plugin| plugin.name).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "bundled registry has duplicate names");
    }
}
