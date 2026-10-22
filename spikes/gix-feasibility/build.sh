#!/usr/bin/env bash
# Build the M1.7 spike component and run the fixture tests.
#
# CARGO_TARGET_DIR must be set (the worktree filesystem is a small tmpfs and
# must never receive build output):
#
#   CARGO_TARGET_DIR=/var/lib/opencode-agent/targets/m1-gix-spike ./build.sh
#
# Pass `--release` to build the component with optimizations (recommended for
# the benchmark):
#
#   CARGO_TARGET_DIR=... ./build.sh --release
set -euo pipefail
cd "$(dirname "$0")"

if [ -z "${CARGO_TARGET_DIR:-}" ]; then
    echo "error: set CARGO_TARGET_DIR (e.g. /var/lib/opencode-agent/targets/m1-gix-spike)" >&2
    exit 1
fi

profile=()
if [ "${1:-}" = "--release" ]; then
    profile=(--release)
    shift
fi

(
    cd component
    cargo component build --target wasm32-wasip2 "${profile[@]}"
)
# `default_component_path` points at the debug artifact; for a release build,
# point the tests at the matching release component instead.
if [ "${#profile[@]}" -ne 0 ]; then
    export GIX_SPIKE_COMPONENT="$CARGO_TARGET_DIR/wasm32-wasip2/release/gix_feasibility.wasm"
fi
cargo test "$@"
