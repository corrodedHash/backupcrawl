#!/usr/bin/env bash
# Rebuild the loadable CLI crawl fixture component (tests/fixtures/detectors).
#
# The fixture is the SDK example detector (`backupcrawl-plugin-example`); this
# script builds it as a WebAssembly component and copies the artifact next to
# its manifest. Run it after a stable-toolchain or cargo-component bump and
# commit the refreshed `dotbak.wasm`.
#
# Honors CARGO_TARGET_DIR; extra arguments are forwarded to cargo-component
# (e.g. `--release`).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

# Resolve (and export) the target directory the same way the nested example
# build does, so the artifact lookup below honors a config-based
# `[build] target-dir` instead of assuming the in-tree `target/`.
# shellcheck source=scripts/wasm-build-env.sh
. "$root/scripts/wasm-build-env.sh"
wasm_build_env "$root"

fixture_dir="crates/backupcrawl-cli/tests/fixtures/detectors/dotbak"

# The generated fixture is the release component; default to it so a bare
# invocation cannot silently overwrite it with a much larger debug blob.
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

bash scripts/build-example-plugin.sh "${build_args[@]}"

target_dir="${CARGO_TARGET_DIR:-$root/target}"

built="$target_dir/wasm32-wasip1/$profile/backupcrawl_plugin_example.wasm"
dest="$fixture_dir/dotbak.wasm"
cp "$built" "$dest"
chmod 644 "$dest"
echo "fixture component: $dest"
