#!/usr/bin/env bash
# Build the SDK example detector as a WebAssembly component.
#
# This is the reference for how a plugin built with the SDK is compiled: a
# `cdylib` with `[package.metadata.component]` plus a `wit` symlink to the
# canonical contract, built with `cargo component build`. cargo-component
# 0.21.1 compiles `wasm32-wasip1` and componentizes it with the preview1
# adapter, so the artifact lands under `wasm32-wasip1` — do NOT pass
# `--target wasm32-wasip2` to `cargo component build`.
#
# Pass extra cargo-component flags (e.g. `--release`) through as arguments.
# Honors CARGO_TARGET_DIR; never writes into the source tree's `target/`.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

cargo component build -p backupcrawl-plugin-example "$@"

target_dir="${CARGO_TARGET_DIR:-$root/target}"
profile="debug"
for arg in "$@"; do
    if [[ "$arg" == "--release" ]]; then
        profile="release"
    fi
done
echo "example component: $target_dir/wasm32-wasip1/$profile/backupcrawl_plugin_example.wasm"
