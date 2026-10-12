# M1.7 gix feasibility spike — status

Date: 2026-10-12. Branch: `feat/m1.7-gix-spike` (worktree
`/run/opencode-agent/coding/worktrees/m1-gix-spike`). All work is committed on
this branch; nothing was pushed.

**Outcome: complete. gix is workable on `wasm32-wasip2` with two small shims;
all 16 fixture tests pass and the spike recommends `gix`-default for M1.8.**
Full results and the recommendation are in `docs/rust-rewrite-plan.md` §7.1
("M1.7 spike results").

## What is implemented

- `spikes/gix-feasibility/` standalone Cargo workspace (root `Cargo.toml` has
  `exclude = ["spikes"]`).
- `component/` — `cargo-component` 0.21.1 crate `gix-feasibility`:
  - WIT world `backupcrawl:spike@0.1.0` with
    `export run: func(root: string) -> result<string, string>`
    (`component/wit/spike.wit`); JSON summary via serde_json.
  - gix 0.89.0 (`default-features = false`, features `basic`, `status`, `sha1`):
    `gix::discover_opts` with `core.fileMode=false`, HEAD kind/name/oid, upstream
    tracking ref + ahead/behind (`merge_base` + `rev_walk(..).with_hidden(..)`),
    status counts via `repo.status()` with `UntrackedFiles::Files` and
    `EmitIgnored(Matching)`, conflicts from index stages, and gix's own index
    loader `repo.index_or_empty()`.
- `runner/` — native wasmtime 49 + wasmtime-wasi 49 host (`SpikeHost`, read-only
  `/` preopen, empty env, `add_to_linker_sync`), CLI `gix-spike-run`, and the
  fixture/benchmark tests.
- `vendor/memmap2-shim/` — **required**: gix 0.89 uses `memmap2` unconditionally
  and `memmap2` has no wasm backend. Maps become eager reads (`Vec<u8>`) and the
  shim **preserves the file cursor** (see root cause below).
- `vendor/filetime/` — **required**: upstream `filetime`'s wasm reader accessors
  are `unimplemented!()`; the shim implements them via
  `Metadata::{modified,accessed,created}`.
- `gen-large-repo.sh` (50k-file synthetic repo), `build.sh`, `README.md`.

## Root cause of the original `UnexpectedEof` (resolved)

The earlier handoff hypothesised racily-clean worktree hashing
(`gix-status` → `compute_stream_hash`). That was **wrong**. Instrumenting the
(vendored) `gix-hash` and reading the wasm trap backtrace showed the failing
`read_exact` was in `gix_hash::io::bytes_with_hasher` called from
`gix_index::File::at` → `Repository::open_index` → `Repository::submodules`,
i.e. **opening/checksum-verifying `.git/index`**, not hashing worktree files:

- `gix_index::File::at` maps the index with `memmap2::MmapOptions::map_copy_read_only`.
  The old shim emulated a map with `read_to_end`, which **advanced the shared
  `File` cursor to EOF**. `File::at` then hashes the file content with
  `gix_hash::bytes(&mut file, len)` on the same handle; with the cursor at EOF
  the `read_exact` hit `UnexpectedEof`. Real `mmap` never moves the cursor.
- Fix: the shim saves the cursor, seeks to the requested offset, reads, and
  restores the cursor. (Also discovered `unborn_branch`/`bare_repo` passed only
  because the index path was never reached.)

Fixing that exposed the **next** blocker on that path (previously unreachable):
`filetime::FileTime::from_last_modification_time` is `unimplemented!()` on wasm,
so `File::at` panicked. Fix: the `vendor/filetime` reader shim.

A third adaptation was needed for status parity: **WASI exposes no Unix
executable bit**, so gix flagged every tracked `0755` file as modified (14 in
`ahead_of_upstream`, e.g. the hook samples). Fix: open with
`core.fileMode = false`.

With both shims, the hand-built index/`filetime` workarounds were removed and
the component uses gix's normal `repo.index_or_empty()`.

## Verified

- **Fixture tests**: `cargo test --test fixtures` → **16 passed, 0 failed**.
- **Benchmark** (50k files, native `git status` = 31 ms):
  - debug component / debug host: small `run` ~20 ms, 50k `run` ~17.0 s.
  - release component / debug host: small ~12 ms, 50k ~14.5 s.
  - release component / **release host**: small ~3–6 ms, 50k ~3.6 s.
- **Main-workspace acceptance** (root, `exclude = ["spikes"]` is sufficient;
  fmt/clippy/test did not touch the spike sub-workspace):
  `cargo fmt --check` OK; `cargo clippy --all-targets -- -D warnings` OK;
  `cargo test` OK **only with a clean config**: the shared `$HOME` had a stale
  `~/.config/backupcrawl/config.toml` with a `[plugins]` table written by the
  M1.1/M1.2 sessions, which this branch's M0 CLI rejects. Running with
  `XDG_CONFIG_HOME` pointed at an empty dir gives 1+6+3+6 tests passing.

## Notes for the integrator

- Expected trivial conflict in the root `Cargo.toml` `[workspace]` table:
  `exclude = ["spikes"]` (this branch) vs the M1.1 members rewrite.
- Bundling/merging this spike into `feat/rust-rewrite`: keep
  `spikes/gix-feasibility/` as a standalone workspace with its own lockfile.
- The two shims (`memmap2` cursor fix, `filetime` reader) are the reference
  implementations for M1.8; the `core.fileMode=false` override and the
  exec-bit limitation are documented in §7.1.
- `component/src/bindings.rs` is generated by cargo-component and committed for
  now (M1.8 will depend on the real `backupcrawl:detector` WIT contract instead).
