#!/usr/bin/env bash
# Build the host smoke-test fixture component.
#
# The host smoke test loads `crates/backupcrawl-host/tests/fixtures/test-detector.wasm`,
# which is now generated (gitignored) rather than committed. This script is
# called by `scripts/build-components.sh` (from
# `crates/backupcrawl-host/build.rs`) so `cargo test` builds it as needed; run it
# by hand after changing `crates/test-components` or the WIT contract.
#
# Requires `cargo component` (0.21.1) and the toolchain from rust-toolchain.toml.
# Honors CARGO_TARGET_DIR; never writes into the source tree's `target/`.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

# Trim machine-specific embedded paths (and pick a single codegen unit) so the
# committed artifact is reproducible across checkouts.
# shellcheck source=scripts/wasm-build-env.sh
. "$root/scripts/wasm-build-env.sh"
wasm_build_env "$root"

target_dir="${CARGO_TARGET_DIR:-$root/target}"
artifact="$target_dir/wasm32-wasip1/release/backupcrawl_test_components.wasm"
fixture="$root/crates/backupcrawl-host/tests/fixtures/test-detector.wasm"

cargo component build --release -p backupcrawl-test-components

if [[ ! -f "$artifact" ]]; then
    echo "error: expected component artifact not found: $artifact" >&2
    echo "error: cargo-component 0.21.1 builds wasm32-wasip1 into <target-dir>/wasm32-wasip1/release/" >&2
    exit 1
fi

mkdir -p "$(dirname "$fixture")"
cp "$artifact" "$fixture"
echo "fixture artifact: $fixture"
