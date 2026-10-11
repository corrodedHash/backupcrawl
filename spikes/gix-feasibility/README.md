# M1.7 gix feasibility spike

Standalone spike proving that [`gix`](https://crates.io/crates/gix) (pure Rust
git) can inspect repositories inside a `wasm32-wasip2` WebAssembly component
with a read-only `/` preopen and **no `git` binary**, and measuring its cost.
Results and the recommendation are recorded in
`docs/rust-rewrite-plan.md` §7.1 ("M1.7 spike results").

This directory is a separate Cargo workspace; the main workspace excludes
`spikes/`. Build output must go to an external target directory (the worktree
filesystem is a small tmpfs):

```console
$ export CARGO_TARGET_DIR=/var/lib/opencode-agent/targets/m1-gix-spike
$ ./build.sh                 # cargo component build + cargo test
$ ./build.sh --release       # optimized component (for the benchmark)
```

## Layout

- `component/` — `cargo-component` crate `gix-feasibility`, built with
  `cargo component build --target wasm32-wasip2`. Its WIT world (`component/wit/spike.wit`)
  exports `run: func(root: string) -> result<string, string>` and returns a
  JSON summary (HEAD state, upstream ahead/behind, status counts, errors).
- `runner/` — native `wasmtime` 49 + `wasmtime-wasi` 49 runner. `src/lib.rs`
  is the reusable host (read-only `/` preopen, empty env);
  `src/main.rs` is the `gix-spike-run` CLI; `tests/` contains the git-CLI
  fixture suite and the benchmark.
- `vendor/memmap2-shim/` — **required on wasm**: gix 0.89 uses `memmap2`
  unconditionally for the index, loose objects, packs and packed refs, and
  `memmap2` has no wasm implementation. The shim replaces mapping with an
  eager read into memory; see §7.1 for caveats. It is wired in through
  `[patch.crates-io]` in the workspace `Cargo.toml`.
- `gen-large-repo.sh` — synthetic ~50k-file repository generator used by the
  large-repo benchmark.

## Fixture tests

`cargo test` builds fixture repositories with the `git` CLI (hermetic env, no
system/global config) and compares the component's JSON with
`git status --porcelain=v2 --branch --ignored=matching` where the semantics
are meant to be identical: clean, dirty (modified + untracked), ahead, behind,
no upstream, detached HEAD, unborn branch, linked worktree (`.git` file), bare
repository, ignored files (`--ignored=matching`), nested repository inside a
dirty parent, ancestor discovery from a subdirectory, staged rename, merge
conflict, packed objects/refs, and a non-repository path.

The component artifact is looked up via `$GIX_SPIKE_COMPONENT`, falling back
to `$CARGO_TARGET_DIR/wasm32-wasip2/debug/gix_feasibility.wasm`.

## Large-repo benchmark

```console
$ CARGO_TARGET_DIR=... cargo test --release --test large_repo -- --ignored --nocapture
```

generates a 50k-file repository with `gen-large-repo.sh` (or uses
`$GIX_SPIKE_LARGE_REPO`) and prints cold instantiation and warm `run` times.
Point `GIX_SPIKE_COMPONENT` at the release-built component for realistic
numbers.
