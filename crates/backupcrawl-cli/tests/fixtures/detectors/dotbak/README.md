# CLI crawl fixture: `dotbak`

This directory holds a real, loadable WebAssembly detector used by the
`backupcrawl` end-to-end crawl tests (`crates/backupcrawl-cli/tests/cli.rs`).

* `dotbak.wasm` — the SDK example component (`backupcrawl-plugin-example`),
  release build.
* `plugin.toml` — its manifest (`dir` + `file` hooks, `cache` capability).

The component is the one documented in `crates/backupcrawl-plugin-sdk/example-plugin`:
a directory containing a `.dotbak` marker file is claimed `clean` with
`prune = true`; files ending in `.bak` are claimed `dirty`; everything else is
unclaimed.

`plugin.toml` renames it to `dotbak` so the fixture is clearly separate from the
manifest-only fixtures in `../plugins` (whose golden `plugins list` output must
stay unchanged).

## Rebuilding

The artifact is toolchain-sensitive (component output depends on the exact
stable toolchain + `cargo-component`), so refresh it when either changes and
commit the result:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
scripts/build-cli-fixture-plugin.sh --release
```

The script builds `backupcrawl-plugin-example` and copies the component here.
