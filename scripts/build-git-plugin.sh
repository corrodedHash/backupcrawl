#!/usr/bin/env bash
# Build the bundled git detector plugin and refresh `plugins/git/dist/git.wasm`.
#
# The plugin is a `cdylib` built with `cargo component build --target
# wasm32-wasip2`; the component lands in
# `<CARGO_TARGET_DIR>/wasm32-wasip2/<profile>/backupcrawl_plugin_git.wasm`.
# The committed artifact is embedded into the binary via `include_bytes!`
# (`crates/backupcrawl-host/src/bundled.rs`) and loaded by the golden tests.
#
# The committed `plugin.toml` is source and is never overwritten here.
# A single release codegen unit keeps the artifact reproducible across
# checkouts (mirrors `scripts/build-test-components.sh`). Honors
# CARGO_TARGET_DIR; never writes into the source tree's `target/`.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

# Trim machine-specific embedded paths so the committed artifact is
# reproducible across checkouts.
# shellcheck source=scripts/wasm-build-env.sh
. "$root/scripts/wasm-build-env.sh"
wasm_build_env "$root"

dist="plugins/git/dist"

# The committed artifact is the release component; default to it so a bare
# invocation cannot silently overwrite it with a debug blob.
build_args=("$@")
profile="debug"
for arg in "$@"; do
    if [[ "$arg" == "--release" ]]; then
        profile="release"
    fi
done
if [[ "$profile" != "release" ]]; then
    build_args+=("--release")
    profile="release"
fi

export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1

cargo component build -p backupcrawl-plugin-git --target wasm32-wasip2 "${build_args[@]}"

target_dir="${CARGO_TARGET_DIR:-$root/target}"
built="$target_dir/wasm32-wasip2/$profile/backupcrawl_plugin_git.wasm"
if [[ ! -f "$built" ]]; then
    echo "error: expected component artifact not found: $built" >&2
    exit 1
fi

cp "$built" "$dist/git.wasm"
chmod 644 "$dist/git.wasm"
echo "git plugin component: $dist/git.wasm"
