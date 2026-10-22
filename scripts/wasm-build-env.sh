#!/usr/bin/env bash
# Shared environment for WebAssembly component builds.
#
# The components are no longer committed: they are built at build time (see
# `scripts/build-components.sh`). The environment set here is still applied so
# that the generated artifacts are reproducible and, more importantly, so that
# the shared component target cache (`target/wasm-components`) is keyed the same
# way on every build. rustc embeds absolute paths (crate registry sources, the
# checkout, the target directory) in panic locations and debug sections; every
# machine-specific prefix is therefore remapped to a stable placeholder.
#
# Usage: source this file from a build script after `root` is set, then call
# `wasm_build_env "$root"`. The function is idempotent, so a script that calls
# another build script does not append the flags twice.

# Resolve the target directory cargo will actually use. An explicit
# CARGO_TARGET_DIR wins; otherwise ask cargo, so a `[build] target-dir` in
# `.cargo/config.toml` (e.g. the machine-level config that points every
# worktree at the shared build cache) is honored instead of assuming the
# in-tree `target/`. Falls back to the in-tree default if cargo cannot be
# queried.
cargo_target_dir() {
    if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
        printf '%s' "$CARGO_TARGET_DIR"
        return 0
    fi
    local root="${1:-$PWD}"
    local resolved
    resolved="$(cargo metadata --no-deps --format-version 1 \
        --manifest-path "$root/Cargo.toml" 2>/dev/null \
        | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])' 2>/dev/null)"
    printf '%s' "${resolved:-$root/target}"
}

wasm_build_env() {
    if [[ -n "${BACKUPCRAWL_WASM_BUILD_ENV:-}" ]]; then
        return 0
    fi
    local root="$1"
    local target_dir
    target_dir="$(cargo_target_dir "$root")"
    local cargo_home="${CARGO_HOME:-$HOME/.cargo}"
    local rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"
    # `cargo install`ed subcommands (cargo-component) land in `$CARGO_HOME/bin`,
    # which is not necessarily on PATH in a non-login shell; the nested
    # `cargo component build` needs it.
    export PATH="$cargo_home/bin:$PATH"
    # Pin CARGO_TARGET_DIR to the resolved path so the artifact lookups below
    # and the nested cargo-component invocation agree with the remap prefixes.
    export CARGO_TARGET_DIR="$target_dir"
    # Most specific prefixes first: when the target directory lives inside the
    # checkout, its paths must map to `/target`, not to `/src/target`.
    export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=${cargo_home}=/cargo --remap-path-prefix=${rustup_home}=/rustup --remap-path-prefix=${target_dir}=/target --remap-path-prefix=${root}=/src"
    # A single release codegen unit keeps symbol names from depending on the
    # checkout/target path (see build-test-components.sh).
    export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
    # Strip the wasm `name` custom section. It is 13-17% of each component and
    # is only useful for symbolizing traps, which the host no longer does (the
    # `backtrace`/`demangle` wasmtime features are disabled). Applying it here
    # keeps the generated artifacts stable across checkouts.
    export CARGO_PROFILE_RELEASE_STRIP=symbols
    export BACKUPCRAWL_WASM_BUILD_ENV=1
}
