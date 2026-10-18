//! Build-time preparation of the loadable CLI test detector.
//!
//! `crates/backupcrawl-cli/tests/fixtures/detectors/dotbak/dotbak.wasm` used to
//! be committed. It is the SDK example detector (`backupcrawl-plugin-example`)
//! compiled as a WebAssembly component and copied next to its manifest. This
//! script rebuilds it with `scripts/build-cli-fixture-plugin.sh` (a thin wrapper
//! over `scripts/build-example-plugin.sh`) into the now-gitignored fixture path,
//! so the CLI's integration tests get a freshly built component.
//!
//! The bundled detectors themselves are built by `backupcrawl-host`'s build
//! script; this only covers the CLI-only fixture. Both nested builds share
//! `BACKUPCRAWL_COMPONENT_TARGET_DIR` (default `<workspace>/target/wasm-components`)
//! so the example plugin and the bundled detectors reuse one cargo cache, and
//! both use a target directory other than the outer build's to avoid its lock.
//! `cargo-component` 0.21.1 is required; `BACKUPCRAWL_SKIP_COMPONENT_BUILD=1`
//! reuses existing artifacts without rebuilding.
//!
//! The build is unconditional (there is no stable way to tell a `cargo test`
//! build from a `cargo build` one in a build script). The fixture is cheap -- the
//! example plugin only depends on the SDK and cargo caches the result -- so this
//! is simpler than gating it.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Sources feeding the example component, relative to the workspace root.
const WATCHED: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "crates/backupcrawl-wit/wit",
    "crates/backupcrawl-plugin-sdk/Cargo.toml",
    "crates/backupcrawl-plugin-sdk/src",
    "crates/backupcrawl-plugin-sdk/example-plugin/Cargo.toml",
    "crates/backupcrawl-plugin-sdk/example-plugin/src",
    "scripts/build-example-plugin.sh",
    "scripts/build-cli-fixture-plugin.sh",
    "scripts/wasm-build-env.sh",
];

fn main() {
    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is always set by cargo"),
    );
    // `CARGO_MANIFEST_DIR` is `crates/backupcrawl-cli`, so the workspace root is
    // two levels up.
    let root = manifest_dir.join("../..");
    let fixture = manifest_dir.join("tests/fixtures/detectors/dotbak/dotbak.wasm");

    for rel in WATCHED {
        println!("cargo:rerun-if-changed={}", root.join(rel).display());
    }
    // NOTE: the generated `dotbak.wasm` fixture is deliberately not watched: the
    // build script refreshes it with `cp` on every run, which bumps its mtime
    // even when the bytes are unchanged and would make cargo re-run this script
    // (and recompile the crate) forever. Touch a watched source (or `cargo
    // clean`) to force a rebuild from a stale fixture.

    if std::env::var_os("BACKUPCRAWL_SKIP_COMPONENT_BUILD").is_some() {
        println!(
            "cargo:warning=BACKUPCRAWL_SKIP_COMPONENT_BUILD is set; reusing the existing \
             dotbak test component without rebuilding it"
        );
        return;
    }

    if !cargo_component_available() {
        if fixture.is_file() {
            println!(
                "cargo:warning=`cargo-component` was not found on PATH; reusing the existing \
                 dotbak test component. Install it with `cargo install cargo-component \
                 --version 0.21.1 --locked` to rebuild it from source."
            );
            return;
        }
        panic!(
            "`cargo-component` 0.21.1 is required to build the CLI test component, but it was \
             not found on PATH.\n\
             Install it with `cargo install cargo-component --version 0.21.1 --locked` and make \
             sure `$CARGO_HOME/bin` (usually `~/.cargo/bin`) is on PATH, or provide the built \
             component and set BACKUPCRAWL_SKIP_COMPONENT_BUILD=1."
        );
    }

    let script = root.join("scripts/build-cli-fixture-plugin.sh");
    let status = Command::new("bash")
        .arg(&script)
        .current_dir(&root)
        .env("CARGO_TARGET_DIR", component_target_dir(&root))
        .status()
        .unwrap_or_else(|err| panic!("running {}: {err}", script.display()));
    if !status.success() {
        panic!(
            "building the CLI test component failed ({} exited with {status}).\n\
             `cargo-component` 0.21.1 and the wasm32-wasip1 target are required; see the \
             `plugin-authoring` docs.",
            script.display()
        );
    }
}

/// The target directory the nested component build uses; kept in sync with
/// `crates/backupcrawl-host/build.rs` so both share one cache.
fn component_target_dir(root: &Path) -> PathBuf {
    std::env::var_os("BACKUPCRAWL_COMPONENT_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target").join("wasm-components"))
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
