#!/usr/bin/env bash
# Build the fixture detector component and refresh the checked-in artifact.
#
# The host smoke test loads `crates/backupcrawl-host/tests/fixtures/test-detector.wasm`,
# which is committed so `cargo test` works without the wasm toolchain. Run this
# script after changing `crates/test-components` (or the WIT contract) and
# commit the refreshed artifact; CI fails if it is stale.
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
