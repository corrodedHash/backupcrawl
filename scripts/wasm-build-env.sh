#!/usr/bin/env bash
# Shared environment for reproducible WebAssembly component builds.
#
# rustc embeds absolute paths (crate registry sources, the checkout, the target
# directory) in panic locations and debug sections. That makes a committed
# `.wasm` artifact differ between machines even when the toolchain and sources
# are identical, which breaks the CI "rebuild and `git diff --exit-code`"
# staleness checks. Every machine-specific prefix is therefore remapped to a
# stable placeholder.
#
# Usage: source this file from a build script after `root` is set, then call
# `wasm_build_env "$root"`. The function is idempotent, so a script that calls
# another build script does not append the flags twice.

wasm_build_env() {
    if [[ -n "${BACKUPCRAWL_WASM_BUILD_ENV:-}" ]]; then
        return 0
    fi
    local root="$1"
    local target_dir="${CARGO_TARGET_DIR:-$root/target}"
    local cargo_home="${CARGO_HOME:-$HOME/.cargo}"
    local rustup_home="${RUSTUP_HOME:-$HOME/.rustup}"
    # Most specific prefixes first: when the target directory lives inside the
    # checkout, its paths must map to `/target`, not to `/src/target`.
    export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=${cargo_home}=/cargo --remap-path-prefix=${rustup_home}=/rustup --remap-path-prefix=${target_dir}=/target --remap-path-prefix=${root}=/src"
    # A single release codegen unit keeps symbol names from depending on the
    # checkout/target path (see build-test-components.sh).
    export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
    export BACKUPCRAWL_WASM_BUILD_ENV=1
}
