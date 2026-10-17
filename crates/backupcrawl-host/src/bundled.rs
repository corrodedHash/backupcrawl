//! Embedded registry of bundled detectors.
//!
//! Bundled detectors are compiled into the binary with `include_bytes!` so a
//! standalone `cargo install backupcrawl` works without a plugin directory.
//! External plugins with the same name override them (see
//! [`crate::discovery`]).
//!
//! # Status
//!
//! The registry is filled by M1.8 with the git detector, by M2.1 with the
//! pacman detector and by M3.1 with the chezmoi detector. `plugins/git`,
//! `plugins/pacman` and `plugins/chezmoi` are built with `cargo component build
//! --target wasm32-wasip2 --release` (see `scripts/build-git-plugin.sh`,
//! `scripts/build-pacman-plugin.sh` and `scripts/build-chezmoi-plugin.sh`), the
//! component is copied to `<plugin>/dist/<name>.wasm` and its manifest to
//! `<plugin>/dist/plugin.toml`, and both are embedded below.
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
pub const REGISTRY: &[BundledPlugin] = &[
    BundledPlugin {
        name: "git",
        manifest: include_bytes!("../../../plugins/git/dist/plugin.toml"),
        wasm: include_bytes!("../../../plugins/git/dist/git.wasm"),
    },
    BundledPlugin {
        name: "pacman",
        manifest: include_bytes!("../../../plugins/pacman/dist/plugin.toml"),
        wasm: include_bytes!("../../../plugins/pacman/dist/pacman.wasm"),
    },
    BundledPlugin {
        name: "chezmoi",
        manifest: include_bytes!("../../../plugins/chezmoi/dist/plugin.toml"),
        wasm: include_bytes!("../../../plugins/chezmoi/dist/chezmoi.wasm"),
    },
];

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
