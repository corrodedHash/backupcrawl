//! Compresses the bundled detector components at build time.
//!
//! The committed `plugins/*/dist/*.wasm` files stay the reproducible source of
//! truth (they are what CI rebuilds and diffs); this script only writes
//! zstd-compressed copies into `OUT_DIR`, which [`crate::bundled`] embeds with
//! `include_bytes!` and decompresses lazily. The components are ~4.4 MB raw and
//! compress to roughly a quarter of that, which is a direct saving in the
//! shipped binary.
//!
//! zstd is already linked into the binary through wasmtime's on-disk component
//! cache, so the runtime side adds no new dependency weight.

use std::path::PathBuf;

/// Bundled detector names; each maps to `plugins/<name>/dist/<name>.wasm`.
///
/// Keep in sync with the `REGISTRY` in `src/bundled.rs`.
const PLUGINS: &[&str] = &["git", "pacman", "chezmoi", "syncthing"];

fn main() {
    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is always set by cargo"),
    );
    let out_dir = PathBuf::from(
        std::env::var_os("OUT_DIR").expect("OUT_DIR is always set for build scripts"),
    );

    for name in PLUGINS {
        // `CARGO_MANIFEST_DIR` is `crates/backupcrawl-host`, so the workspace
        // root is two levels up.
        let src = manifest_dir
            .join("../..")
            .join("plugins")
            .join(name)
            .join("dist")
            .join(format!("{name}.wasm"));
        let src = src.canonicalize().unwrap_or_else(|err| {
            panic!("locating bundled plugin {name} at {}: {err}", src.display())
        });

        println!("cargo:rerun-if-changed={}", src.display());

        let wasm = std::fs::read(&src)
            .unwrap_or_else(|err| panic!("reading bundled plugin {}: {err}", src.display()));

        // Level 19 is a one-off build cost per artifact; the resulting blob is
        // embedded and decompressed once per process, so it is not on any hot
        // path. zstd is deterministic, so a given artifact always embeds the
        // same bytes.
        let compressed = zstd::stream::encode_all(wasm.as_slice(), 19)
            .unwrap_or_else(|err| panic!("compressing bundled plugin {}: {err}", src.display()));

        let dst = out_dir.join(format!("{name}.wasm.zst"));
        std::fs::write(&dst, compressed)
            .unwrap_or_else(|err| panic!("writing compressed plugin {}: {err}", dst.display()));
    }
}
