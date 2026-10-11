# M1.7 gix feasibility spike — status / handoff (interrupted)

Date: 2026-10-11. Branch: `feat/m1.7-gix-spike` (worktree
`/run/opencode-agent/coding/worktrees/m1-gix-spike`). All work is committed on
this branch; nothing was pushed. This note records exactly what works, what
does not, and how to resume.

## What is implemented

- `spikes/gix-feasibility/` standalone Cargo workspace (root `Cargo.toml`
  gained `exclude = ["spikes"]`).
- `component/` — cargo-component crate `gix-feasibility`:
  - WIT world `backupcrawl:spike@0.1.0` with
    `export run: func(root: string) -> result<string, string>`
    (`component/wit/spike.wit`).
  - gix 0.89.0 inspection: `gix::discover`, HEAD kind/name/oid, upstream
    tracking ref + ahead/behind via `merge_base` + `rev_walk(...).with_hidden(...)`,
    status counts (staged/unstaged/renamed/conflicted/untracked/ignored) via
    `repo.status()` with `UntrackedFiles::Files` and
    `EmitIgnored(Matching)`, conflicts from index stages, JSON summary via
    serde_json. Returns a JSON `error` field for repository-level failures.
  - Manual index loading (`load_index`) to avoid mmap and filetime wasm stubs.
- `runner/` — native wasmtime 49 + wasmtime-wasi 49 host (`SpikeHost`,
  read-only `/` preopen, empty env, `add_to_linker_sync`), CLI `gix-spike-run`,
  and fixture tests.
- `vendor/memmap2-shim/` — memmap2 API shim wired via
  `[patch.crates-io] memmap2 = { path = "vendor/memmap2-shim" }` in the spike
  workspace. Required: gix 0.89 uses memmap2 unconditionally and memmap2 has
  no wasm implementation (its wasm stub returns `Unsupported`).
- `gen-large-repo.sh` (50k-file synthetic repo), `build.sh`, `README.md`.

## What is verified / working

- **Component builds** for both wasm targets:
  `cd spikes/gix-feasibility/component && CARGO_TARGET_DIR=/var/lib/opencode-agent/targets/m1-gix-spike cargo component build --target wasm32-wasip2`
  → succeeds; artifacts:
  `$CARGO_TARGET_DIR/wasm32-wasip2/debug/gix_feasibility.wasm` (used by the
  runner) and `$CARGO_TARGET_DIR/wasm32-wasip1/debug/gix_feasibility.wasm`
  (cargo-component 0.21.1 also builds this implicitly; both are components).
- **Runner builds** (`cargo build` in the spike workspace) and the CLI loads
  the wasm32-wasip2 component in wasmtime.
- **Fixture tests**: `cd spikes/gix-feasibility && CARGO_TARGET_DIR=... cargo test`
  → **3 passed, 13 failed** (debug component).
  - Passing: `unborn_branch`, `bare_repo`, `not_a_repository`.
  - Failing: `clean_repo`, `dirty_modified_and_untracked`, `ahead_of_upstream`,
    `behind_upstream`, `no_upstream`, `detached_head`, `linked_worktree`,
    `ignored_files_matching`, `nested_repository_in_dirty_parent`,
    `subdirectory_discovers_ancestor`, `staged_rename`, `packed_repository`,
    `merge_conflict`.
  - All failures share one error from the status iteration:
    `failed to iterate status: Error { kind: UnexpectedEof, message: "failed to fill whole buffer" }`
    (a raw `std::io::Error` from a `read_exact` inside gix status; no gix
    context chain). `ahead_of_upstream` shows `ahead: 1` correctly before the
    status error, so refs, config, merge-base and revision walking work on
    wasm.
- **Index workaround verified indirectly**: the previous "failed to read
  index" error is gone after loading the index manually with
  `gix_index::State::from_bytes` (unborn/bare/clean fixtures reach the status
  stage).

## What is broken / unfinished / unverified

1. **Status iteration on repos with tracked files fails** with `UnexpectedEof`
   ("failed to fill whole buffer"). Best hypothesis: racily-clean worktree file
   hashing (`gix-status` `HashEq`/`FastEq` → `compute_stream_hash` →
   `gix_hash::bytes` → `Read::read_exact`) fails on WASI files; an interrupted
   probe (read_exact on a plain file in the component) was not completed.
   `unborn_branch` (no index entries → no file hashing) passes, which supports
   this. Needs: a probe of `std::fs::File::read_exact` on wasm32-wasip2, then
   either a gix code path that avoids `compute_stream_hash`, a shim/fix for the
   read, or a different gix API for worktree comparison.
2. **Large-repo benchmark not run** (`tests/large_repo.rs` `large_synthetic_repo_benchmark`
   is `#[ignore]`d; generator script untested; `small_repo_timing` runs only
   when the suite gets that far).
3. **Release component** not built; no performance numbers collected.
4. **Main workspace acceptance not run**: `cargo fmt --check`,
   `cargo clippy --all-targets -- -D warnings`, `cargo test` from the worktree
   root (the `exclude = ["spikes"]` change is expected to be enough, but it is
   unverified).
5. **docs/rust-rewrite-plan.md §7.1 "M1.7 spike results" not written** (no
   gix-vs-exec recommendation recorded yet).
6. Fixture test expectations were not yet reconciled with observed gix
   behavior (e.g. untracked/ignored collapsing, `behind_upstream` was fixed to
   `git fetch` in the clone but never re-verified).
7. `spikes/gix-feasibility/component/src/bindings.rs` is generated by
   cargo-component; committed for now (decide whether to keep it).

## Decisions made so far

- **gix 0.89.0** (latest on crates.io), `default-features = false`, features
  `["basic", "status", "sha1"]`; `status` pulls `dirwalk` → `attributes` →
  `excludes`; `basic` pulls `revision` + `index`; `sha1` for racily-clean
  hashing. No `parallel`, `interrupt`, `command`, `credentials`, `progress`,
  `mailmap`, `worktree-stream`, `merge`, `blame`, `notes`. gix 0.89 uses
  zlib-rs via `gix-zlib` unconditionally (no zlib feature needed); there is no
  mmap feature anymore.
- **cargo-component 0.21.1 target: `wasm32-wasip2`** (runner loads that
  artifact). Quirk: `is_wasm_target` does not know wasip2, so cargo-component
  also adds an implicit `wasm32-wasip1` build and componentizes it with the
  preview1 adapter; both artifacts are components. Single-target workaround
  (`cargo component bindings` + `cargo build --target wasm32-wasip2`) not yet
  tested.
- **memmap2 shim required** (see above); M1.8 must either vendor it, patch
  upstream, or avoid gix paths that mmap. Shim caveat: eager full-file reads
  (memory ~ pack size; matters for big repos and the 256 MiB cap).
- **filetime wasm stubs must be avoided**: `FileTime::from_last_modification_time`
  and `FileTime::now()` (used by `gix::index::State::new`) are
  `unimplemented!()` on wasm → panic. The component uses `from_unix_time` from
  `SystemTime` and a hand-built empty index (`DIRC`, v2, 0 entries) instead.
- **WIT world / JSON schema** as in `component/src/lib.rs` docs (spike-local;
  the real `backupcrawl:detector@1.0.0` contract is a parallel task).
- **Runner**: wasmtime 49 (default-features=false: anyhow, component-model,
  cranelift, runtime, std), wasmtime-wasi 49, `p2::add_to_linker_sync`,
  `WasiCtx::builder().preopened_dir("/", "/", FsPerms::ReadOnly)`, empty env,
  `wasmtime::component::bindgen!` for the spike world.
- Hermetic fixture git env (`GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null`,
  fixed author/committer) and a `git status --porcelain=v2 --branch --ignored=matching`
  parser for parity assertions.

## Resume commands

```console
export CARGO_TARGET_DIR=/var/lib/opencode-agent/targets/m1-gix-spike

# 1. build component (debug) + run tests
cd spikes/gix-feasibility
./build.sh                      # cargo component build + cargo test
# or manually:
(cd component && cargo component build --target wasm32-wasip2)
cargo test

# 2. single failing test with output
cargo test --test fixtures clean_repo -- --nocapture

# 3. CLI on any repo
cargo run --bin gix-spike-run -- <repo-root>

# 4. release component + large benchmark (not run yet)
./build.sh --release
cargo test --release --test large_repo -- --ignored --nocapture

# 5. main workspace acceptance (from worktree root)
cd ../..
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

## Open questions / next steps for M1.7

1. Diagnose and fix the `UnexpectedEof` in status (probe `read_exact` on a
   WASI file; inspect `gix-status` `compute_stream_hash` path; possibly provide
   a `gix_hash`/`gix-status`-compatible workaround, or patch another crate).
2. Get all fixture tests green (then reconcile ignored/untracked collapsing
   and packed-repo behavior).
3. Run the 50k-file benchmark (debug + release) and a real shallow-cloned repo
   if network allows; record numbers.
4. Run the main-workspace fmt/clippy/test acceptance.
5. Write the "M1.7 spike results" subsection in `docs/rust-rewrite-plan.md`
   §7.1 with the exact feature set, target, APIs, behavior differences,
   performance, and the gix-default vs exec-default recommendation (currently
   leaning: gix is technically feasible on wasm **only with the memmap2 shim
   and filetime workarounds**; the remaining status read failure must be
   solved before recommending gix as the M1 default).
