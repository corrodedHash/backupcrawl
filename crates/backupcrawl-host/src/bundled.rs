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
//! pacman detector, by M3.1 with the chezmoi detector and by M3.2 with the
//! syncthing detector. `plugins/git`, `plugins/pacman`, `plugins/chezmoi` and
//! `plugins/syncthing` are built with `cargo component build --target
//! wasm32-wasip2 --release` (see the matching `scripts/build-*-plugin.sh`), the
//! component is copied to `<plugin>/dist/<name>.wasm` and its manifest to
//! `<plugin>/dist/plugin.toml`, and all are embedded below.
//!
//! `include_bytes!` paths are relative to this source file
//! (`crates/backupcrawl-host/src/bundled.rs`), so `../../../plugins/...`
//! resolves to the workspace's `plugins/` directory.
//!
//! # Compression
//!
//! The wasm components are embedded zstd-compressed: [`build.rs`][build] writes
//! `OUT_DIR/<name>.wasm.zst` from the committed `plugins/*/dist/*.wasm`
//! artifacts, and [`BundledPlugin::wasm`] decompresses on first use. That keeps
//! the committed `.wasm` files as the reproducible source of truth while cutting
//! ~3 MB of embedded data. zstd is already linked through wasmtime's compilation
//! cache, so the runtime side adds no new dependency.
//!
//! [build]: https://doc.rust-lang.org/cargo/reference/build-scripts.html

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// A detector shipped inside the binary.
#[derive(Debug, Clone, Copy)]
pub struct BundledPlugin {
    /// Registry key; must match the manifest's `[plugin].name`.
    pub name: &'static str,
    /// Raw `plugin.toml` bytes.
    pub manifest: &'static [u8],
    /// zstd-compressed wasm component bytes; see [`BundledPlugin::wasm`].
    compressed_wasm: &'static [u8],
}

impl BundledPlugin {
    /// The raw wasm component bytes, decompressed on first use.
    ///
    /// The decompressed buffer is cached for the lifetime of the process, so
    /// discovery and later scans only pay for one decompression per plugin.
    pub fn wasm(&self) -> &'static [u8] {
        static DECOMPRESSED: OnceLock<Mutex<HashMap<&'static str, &'static [u8]>>> =
            OnceLock::new();
        let cache = DECOMPRESSED.get_or_init(|| Mutex::new(HashMap::new()));
        let mut cache = cache
            .lock()
            .expect("the bundled wasm cache mutex is never poisoned");

        if let Some(&wasm) = cache.get(self.name) {
            return wasm;
        }

        let wasm = zstd::stream::decode_all(self.compressed_wasm).unwrap_or_else(|err| {
            panic!(
                "bundled plugin `{}` is not valid zstd data: {err}",
                self.name
            )
        });
        // Cached for the process lifetime, mirroring a plain `include_bytes!`.
        let wasm: &'static [u8] = Box::leak(wasm.into_boxed_slice());
        cache.insert(self.name, wasm);
        wasm
    }
}

/// All bundled detectors, in a stable order.
pub const REGISTRY: &[BundledPlugin] = &[
    BundledPlugin {
        name: "git",
        manifest: include_bytes!("../../../plugins/git/dist/plugin.toml"),
        compressed_wasm: include_bytes!(concat!(env!("OUT_DIR"), "/git.wasm.zst")),
    },
    BundledPlugin {
        name: "pacman",
        manifest: include_bytes!("../../../plugins/pacman/dist/plugin.toml"),
        compressed_wasm: include_bytes!(concat!(env!("OUT_DIR"), "/pacman.wasm.zst")),
    },
    BundledPlugin {
        name: "chezmoi",
        manifest: include_bytes!("../../../plugins/chezmoi/dist/plugin.toml"),
        compressed_wasm: include_bytes!(concat!(env!("OUT_DIR"), "/chezmoi.wasm.zst")),
    },
    BundledPlugin {
        name: "syncthing",
        manifest: include_bytes!("../../../plugins/syncthing/dist/plugin.toml"),
        compressed_wasm: include_bytes!(concat!(env!("OUT_DIR"), "/syncthing.wasm.zst")),
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

    #[test]
    fn every_bundled_component_decompresses_to_wasm() {
        for plugin in all() {
            let wasm = plugin.wasm();
            assert!(
                wasm.starts_with(b"\0asm"),
                "bundled plugin `{}` did not decompress to a wasm binary",
                plugin.name
            );
            // The cache must return the same buffer on a second call.
            assert_eq!(wasm.as_ptr(), plugin.wasm().as_ptr());
        }
    }
}
