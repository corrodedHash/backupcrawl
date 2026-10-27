#!/usr/bin/env bash
# Build every WebAssembly component the workspace needs and place it where the
# consuming crate expects to find it. This replaces the old flow where the
# components were committed and only *verified* in CI.
#
# Outputs (all gitignored; see the matching entries in .gitignore):
#   - the four bundled detectors -> plugins/<name>/dist/<name>.wasm
#     embedded into `backupcrawl-host` by `include_bytes!`.
#   - the host smoke-test detector ->
#     crates/backupcrawl-host/tests/fixtures/test-detector.wasm
#
# The CLI's own fixture (`dotbak`) is built by
# `scripts/build-cli-fixture-plugin.sh`, which `backupcrawl-cli`'s build script
# calls; it is kept separate so this script only touches `backupcrawl-host`'s
# inputs.
#
# Invoked by `crates/backupcrawl-host/build.rs` (which pins CARGO_TARGET_DIR to
# a directory other than the outer build's, so the nested cargo cannot deadlock
# on the outer target-dir lock), but it is also usable stand-alone.
#
# Requires `cargo-component` (0.21.1) and the `wasm32-wasip1`/`wasm32-wasip2`
# targets from rust-toolchain.toml. Extra arguments are forwarded to
# `cargo component build` (e.g. `--release`, though the components are always
# release-built here).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

# Trim machine-specific embedded paths (and pick a single codegen unit) so the
# generated components are stable across checkouts and the shared target cache.
# shellcheck source=scripts/wasm-build-env.sh
. "$root/scripts/wasm-build-env.sh"
wasm_build_env "$root"

export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1

bundled=(git pacman chezmoi syncthing)

pkg_args=()
for name in "${bundled[@]}"; do
    pkg_args+=("-p" "backupcrawl-plugin-$name")
done

# One invocation for all four bunded detectors: they share the SDK and most of
# their dependency tree, so this compiles those crates once instead of four
# times.
cargo component build --release --target wasm32-wasip2 "${pkg_args[@]}" "$@"

target_dir="${CARGO_TARGET_DIR:-$root/target}"
for name in "${bundled[@]}"; do
    built="$target_dir/wasm32-wasip2/release/backupcrawl_plugin_$name.wasm"
    if [[ ! -f "$built" ]]; then
        echo "error: expected component artifact not found: $built" >&2
        exit 1
    fi
    mkdir -p "plugins/$name/dist"
    cp "$built" "plugins/$name/dist/$name.wasm"
    chmod 644 "plugins/$name/dist/$name.wasm"
done
echo "bundled detector components: plugins/{${bundled[*]}}/dist/*.wasm"

# The host smoke-test fixture is the bare-WIT test component; it uses the
# default (wasm32-wasip1 + preview1 adapter) target that cargo-component
# componentizes.
cargo component build --release -p backupcrawl-test-components "$@"

test_built="$target_dir/wasm32-wasip1/release/backupcrawl_test_components.wasm"
if [[ ! -f "$test_built" ]]; then
    echo "error: expected test component not found: $test_built" >&2
    exit 1
fi
fixture="crates/backupcrawl-host/tests/fixtures/test-detector.wasm"
mkdir -p "$(dirname "$fixture")"
cp "$test_built" "$fixture"
chmod 644 "$fixture"
echo "host test component: $fixture"
