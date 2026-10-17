#!/usr/bin/env bash
# Build the bundled pacman detector plugin and refresh `plugins/pacman/dist/pacman.wasm`.
#
# The plugin is a `cdylib` built with `cargo component build --target
# wasm32-wasip2`; the component lands in
# `<CARGO_TARGET_DIR>/wasm32-wasip2/<profile>/backupcrawl_plugin_pacman.wasm`.
# The committed artifact is embedded into the binary via `include_bytes!`
# (`crates/backupcrawl-host/src/bundled.rs`) and loaded by the golden tests.
#
# The committed `plugin.toml` is source and is never overwritten here.
# A single release codegen unit keeps the artifact reproducible across
# checkouts (mirrors `scripts/build-git-plugin.sh`). Honors CARGO_TARGET_DIR;
# never writes into the source tree's `target/`.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

dist="plugins/pacman/dist"

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

cargo component build -p backupcrawl-plugin-pacman --target wasm32-wasip2 "${build_args[@]}"

target_dir="${CARGO_TARGET_DIR:-$root/target}"
built="$target_dir/wasm32-wasip2/$profile/backupcrawl_plugin_pacman.wasm"
if [[ ! -f "$built" ]]; then
    echo "error: expected component artifact not found: $built" >&2
    exit 1
fi

cp "$built" "$dist/pacman.wasm"
chmod 644 "$dist/pacman.wasm"
echo "pacman plugin component: $dist/pacman.wasm"
