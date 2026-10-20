//! Build-time preparation of the bundled detector components.
//!
//! The four bundled detectors are no longer committed as `.wasm` artifacts.
//! This script compiles them from source with `scripts/build-components.sh`
//! (which drives `cargo component build`) into the now-gitignored
//! `plugins/<name>/dist/<name>.wasm` paths, then zstd-compresses each into
//! `OUT_DIR`, where [`crate::bundled`] embeds it with `include_bytes!` and
//! decompresses lazily. The raw components are ~3.8 MB and compress to roughly
//! a quarter of that, a direct saving in the shipped binary. zstd is already
//! linked into the binary through wasmtime's on-disk component cache, so the
//! runtime side adds no new dependency weight.
//!
//! # Rebuilding
//!
//! The build is delegated to `scripts/build-components.sh`, so the components
//! are produced exactly the way a developer builds them by hand. The script
//! reuses cargo's own fingerprints inside its own target directory: on an
//! up-to-date tree the nested `cargo component build` is a no-op, otherwise it
//! rebuilds only what changed. `cargo:rerun-if-changed` is emitted for every
//! source that feeds a component, so this script re-runs when one changes and
//! the nested build then picks the change up.
//!
//! # Why a separate target directory
//!
//! A build script must not invoke cargo against the target directory the outer
//! build already holds the lock on: cargo serialises builds with a target-dir
//! lock, so that would deadlock. The nested build therefore uses
//! `BACKUPCRAWL_COMPONENT_TARGET_DIR` (default `<workspace>/target/wasm-components`),
//! which is passed to the script as `CARGO_TARGET_DIR`.
//!
//! # Prerequisites
//!
//! `cargo-component` 0.21.1 plus the `wasm32-wasip1`/`wasm32-wasip2` targets
//! (pinned in `rust-toolchain.toml`). Packagers that pre-place the built
//! artifacts can set `BACKUPCRAWL_SKIP_COMPONENT_BUILD=1` to skip the nested
//! build entirely; on a machine without `cargo-component` the existing
//! artifacts are reused with a warning when they are all present.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Bundled detector names; each maps to `plugins/<name>/dist/<name>.wasm`.
///
/// Keep in sync with the `REGISTRY` in `src/bundled.rs`.
const PLUGINS: &[&str] = &["git", "pacman", "chezmoi", "syncthing"];

/// Sources that feed at least one bundled component, relative to the workspace
/// root. A change in any of them must re-run this script so the nested build
/// sees it.
const WATCHED: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "crates/backupcrawl-wit/wit",
    "crates/backupcrawl-plugin-sdk/Cargo.toml",
    "crates/backupcrawl-plugin-sdk/src",
    "crates/test-components/Cargo.toml",
    "crates/test-components/src",
    "scripts/build-components.sh",
    "scripts/wasm-build-env.sh",
];

fn main() {
    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is always set by cargo"),
    );
    let out_dir = PathBuf::from(
        std::env::var_os("OUT_DIR").expect("OUT_DIR is always set for build scripts"),
    );
    // `CARGO_MANIFEST_DIR` is `crates/backupcrawl-host`, so the workspace root
    // is two levels up.
    let root = manifest_dir.join("../..");

    for rel in WATCHED {
        println!("cargo:rerun-if-changed={}", root.join(rel).display());
    }
    for name in PLUGINS {
        println!(
            "cargo:rerun-if-changed={}",
            root.join("plugins").join(name).join("src").display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            root.join("plugins").join(name).join("Cargo.toml").display()
        );
    }
    // NOTE: the generated `plugins/*/dist/*.wasm` and
    // `tests/fixtures/test-detector.wasm` are deliberately *not* watched. The
    // build scripts refresh them with `cp` on every run, which bumps their mtime
    // even when the bytes are unchanged and would make cargo re-run this script
    // (and recompile the crate) forever. Rebuilding a component by hand without a
    // source change is rare; touch a watched source (or `cargo clean`) to force
    // it.

    if std::env::var_os("BACKUPCRAWL_SKIP_COMPONENT_BUILD").is_none() {
        build_components(&root, &manifest_dir);
    } else {
        println!(
            "cargo:warning=BACKUPCRAWL_SKIP_COMPONENT_BUILD is set; reusing the existing \
             WebAssembly components without rebuilding them"
        );
    }

    for name in PLUGINS {
        let src = dist_wasm(&root, name);
        let wasm = std::fs::read(&src).unwrap_or_else(|err| {
            panic!(
                "reading bundled plugin {}: {err}\n\
                 The component has not been built. Run `cargo-component` 0.21.1 (`cargo install \
                 cargo-component --version 0.21.1 --locked`) or drop \
                 BACKUPCRAWL_SKIP_COMPONENT_BUILD and provide prebuilt components.",
                src.display()
            )
        });

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

/// `plugins/<name>/dist/<name>.wasm` under the workspace root.
fn dist_wasm(root: &Path, name: &str) -> PathBuf {
    root.join("plugins")
        .join(name)
        .join("dist")
        .join(format!("{name}.wasm"))
}

/// The target directory the nested component build uses.
///
/// Must differ from the outer build's target directory (see the module docs);
/// `BACKUPCRAWL_COMPONENT_TARGET_DIR` overrides it.
fn component_target_dir(root: &Path) -> PathBuf {
    std::env::var_os("BACKUPCRAWL_COMPONENT_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target").join("wasm-components"))
}

/// Run the component build for every bundled detector and the host test fixture.
fn build_components(root: &Path, manifest_dir: &Path) {
    let fixture = manifest_dir.join("tests/fixtures/test-detector.wasm");
    let all_present =
        PLUGINS.iter().all(|name| dist_wasm(root, name).is_file()) && fixture.is_file();

    if !cargo_component_available() {
        if all_present {
            println!(
                "cargo:warning=`cargo-component` was not found on PATH; reusing the existing \
                 WebAssembly components. Install it with `cargo install cargo-component \
                 --version 0.21.1 --locked` to rebuild them from source."
            );
            return;
        }
        panic!(
            "`cargo-component` 0.21.1 is required to build the bundled detectors, but it was \
             not found on PATH.\n\
             Install it with `cargo install cargo-component --version 0.21.1 --locked` and make \
             sure `$CARGO_HOME/bin` (usually `~/.cargo/bin`) is on PATH, or provide the built \
             components and set BACKUPCRAWL_SKIP_COMPONENT_BUILD=1."
        );
    }

    let script = root.join("scripts/build-components.sh");
    let status = Command::new("bash")
        .arg(&script)
        .current_dir(root)
        .env("CARGO_TARGET_DIR", component_target_dir(root))
        .status()
        .unwrap_or_else(|err| panic!("running {}: {err}", script.display()));
    if !status.success() {
        panic!(
            "building the bundled WebAssembly components failed ({} exited with {status}).\n\
             `cargo-component` 0.21.1 and the wasm32-wasip1/wasm32-wasip2 targets are required; \
             see the `plugin-authoring` docs.",
            script.display()
        );
    }
}

/// Whether a `cargo-component` executable can be found, checking `PATH` plus
/// the two cargo locations that are commonly not on a non-login shell's `PATH`.
fn cargo_component_available() -> bool {
    let exe = std::env::consts::EXE_SUFFIX;
    let name = format!("cargo-component{exe}");

    if std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(&name).is_file()))
    {
        return true;
    }
    let cargo_bin = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
        .map(|home| home.join("bin"));
    cargo_bin.is_some_and(|dir| dir.join(&name).is_file())
}
