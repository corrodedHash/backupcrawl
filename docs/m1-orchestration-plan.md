# M1 orchestration plan and status

This file tracks how M1 of the Rust rewrite is being implemented in parallel subagent
worktrees. The task definitions and acceptance criteria are in `docs/rust-rewrite-plan.md`
(section 12, "M1 task breakdown") and remain the source of truth; this file records the
orchestration decisions, the wave plan, and where work currently stands.

## Task graph and waves

```
M1.1 foundation (WIT + CI + scaffolding)
 ├── M1.2 plugin SDK          ──┐
 ├── M1.3 manifest/discovery  ──┤ wave 1 (parallel)
 ├── M1.7 gix spike (independent, runs in parallel with M1.1)
 └── M1.4 host runtime        ──┐  (needs M1.2, M1.3)
     M1.5 trust flow          ──┤ wave 2 (parallel)  (needs M1.3)
     M1.8 git plugin on gix   ──┘  (needs M1.2, M1.7)
         M1.6 façade + walker integration   ──┐ wave 3 (parallel) (needs M1.4)
         M1.9 git exec fallback (backend=exec)┘ (needs M1.8)
             M1.10 wrap-up                   wave 4 (needs everything)
```

## Worktrees and branches

Integration branch: `feat/rust-rewrite` in `/run/opencode-agent/coding/worktrees/rust-rewrite`.

| Task | Worktree | Branch | Status |
| --- | --- | --- | --- |
| M1.1 foundation | `worktrees/m1-foundation` | `feat/m1.1-foundation` | done, merged as `de193d5` |
| M1.7 gix spike | `worktrees/m1-gix-spike` | `feat/m1.7-gix-spike` | done, merged as `44cbe19` (gix workable on wasm) |
| M1.2 SDK | `worktrees/m1-sdk` | `feat/m1.2-plugin-sdk` | done, merged as `efb468c` |
| M1.3 manifest/discovery | `worktrees/m1-manifest` | `feat/m1.3-manifest-discovery` | done, merged as `d468f46` |
| M1.4 host runtime | `worktrees/m1-host` | `feat/m1.4-wasm-host` | done, merged as `69a8680` |
| M1.5 trust flow | `worktrees/m1-trust` | `feat/m1.5-trust-flow` | done, merged as `10a5529` |
| M1.8 git plugin | `worktrees/m1-git` | `feat/m1.8-git-plugin` | done, merged as `d7f1519` |
| M1.6 walker integration | `worktrees/m1-integration` | `feat/m1.6-walker-integration` | done, merged as `bcbdebf` |
| M1.9 git exec fallback | `worktrees/m1-git-exec` (to create) | `feat/m1.9-git-exec-fallback` | not started |
| M1.10 wrap-up | `worktrees/m1-wrapup` (to create) | `feat/m1.10-wrap-up` | not started |

Wave 1 tasks branch from `feat/rust-rewrite` after M1.1 is merged; wave 2 after wave 1 is
merged; and so on. Every task is dispatched as a `general` subagent with: the worktree path,
the task text quoted from the plan, the frozen interface decisions below, the build commands
with `CARGO_TARGET_DIR`, and a final-report requirement (commits, evidence, deviations).

## Frozen interface decisions (established for M1.1; later tasks must follow)

- **WIT contract**: canonical single file `crates/backupcrawl-wit/wit/detector.wit`,
  package `backupcrawl:detector@1.0.0`, interfaces `types`, `host`, `detector`, world
  `plugin` (`import host; export detector;`). One world is enough: the host embedder
  generates bindings for the guest world `plugin`.
- **Bindgen paths**: host uses
  `wasmtime::component::bindgen!({ path: "../backupcrawl-wit/wit", world: "plugin" })`
  (relative to `CARGO_MANIFEST_DIR`); guest crates carry a `wit` symlink to the canonical
  directory and the SDK macro emits `wit_bindgen::generate!({ path: "wit", world: "plugin" })`.
- **Crate layout**: `crates/backupcrawl-wit`, `crates/backupcrawl-host` (manifest/discovery/
  bundled = M1.3, runtime/imports/limits = M1.4, trust = M1.5, WasmDetector = M1.4/M1.6),
  `crates/backupcrawl-plugin-sdk` (M1.2), `crates/test-components` (fixture source),
  `plugins/git` (M1.8/M1.9).
- **Fixture strategy**: prebuilt `.wasm` artifacts are committed; test fixture lives at
  `crates/backupcrawl-host/tests/fixtures/test-detector.wasm`, built from
  `crates/test-components` by `scripts/build-test-components.sh`; CI fails if the artifact is
  stale. The git plugin artifact (M1.8) is expected at `plugins/git/dist/git.wasm` and embedded
  via `include_bytes!` into the bundled registry (initially empty in M1.3).
- **`check-files`**: stays in the WIT `@1` contract and must behave exactly like N singular
  calls; the SDK provides the default loop; the host keeps using singular calls until M4.
- **Detector trait**: defined in `backupcrawl-core` in M1.6; M1.4 exposes an inherent
  `WasmDetector` API that maps 1:1 onto it. Trait carries `check_symlink`, but the walker keeps
  skipping symlinks until M2.
- **CLI config skeleton**: `[plugins]` gains `enabled`, `order`, `trust` and a flattened map of
  per-plugin tables (`[plugins.git]`, ...). `[ignore]` behavior is unchanged.
- **Bundled plugins**: auto-trusted (documented in M1.5); third-party plugins prompt until
  trusted.

## Integration procedure (orchestrator)

1. When a task subagent reports done: inspect the branch diff, check out its branch in the
   integration worktree or merge it directly (`git merge --no-ff`), resolve conflicts.
2. `Cargo.lock` conflicts: take either side, then run `cargo metadata`/`cargo check` so cargo
   re-adds the missing packages.
3. Run acceptance on the merged result:
   `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`.
4. Only then dispatch the next wave, branching from the updated `feat/rust-rewrite`.
5. Never push; leave branches and worktrees in place for review.

## Tooling reality (verified while implementing M1.1)

- cargo-component 0.21.1 builds and componentizes **wasm32-wasip1** (bundled preview1
  adapter), not wasm32-wasip2; artifact path is
  `<CARGO_TARGET_DIR>/wasm32-wasip1/{debug,release}/<crate_name>.wasm`. Do not pass
  `--target wasm32-wasip2` to `cargo component build`.
- `wit-bindgen` is pinned to **0.41** (the version cargo-component 0.21.1 generates with).
  `cargo component add` is for WIT component dependencies, not for adding the Rust crate.
- Host bindgen that works with wasmtime 49.0.2:
  `wasmtime::component::bindgen!({ path: "../backupcrawl-wit/wit", world: "plugin", additional_derives: [PartialEq] })`
  (path relative to `CARGO_MANIFEST_DIR`); generated: `Plugin` world struct, host import trait
  `backupcrawl::detector::host::Host`, export accessor `plugin.backupcrawl_detector_detector()`.
  WASI must be linked (`wasmtime_wasi::p2::add_to_linker_sync`) or instantiation fails.
- Workspace `rust-version` was bumped to 1.96 (wasmtime 49 MSRV).
- The fixture artifact is toolchain-sensitive: after a stable-toolchain bump, rerun
  `scripts/build-test-components.sh` and commit the refreshed `.wasm` or CI fails.
- `crates/test-components` stays on raw wit-bindgen (not the SDK) so the host smoke test
  exercises the contract directly; `plugins/git` is a stub with no detector exports until M1.8
  (discovery/validation must handle that).

## Environment notes

- Toolchain: rust 1.99 stable; cargo-component 0.21.1 installed via `cargo install`;
  targets `wasm32-wasip2` and `wasm32-wasip1` installed; git 2.56.
- All builds must use `CARGO_TARGET_DIR=/var/lib/opencode-agent/targets/<task>` because the
  worktree filesystem (`/run/opencode-agent`) is a 6 GB tmpfs.
- Baseline (M0) verified green before dispatch: `cargo fmt --check`,
  `cargo clippy --all-targets -- -D warnings`, `cargo test` (16 tests).
- Subagents were told not to trust model memory for cargo-component/wasmtime/gix APIs and to
  consult the installed crate sources under `~/.cargo/registry/src/`.

## Status log

- 2026-10-11: M0 verified green. M1.1 (foundation) and M1.7 (gix spike) dispatched in parallel.
  Plan frozen in this file. Wave 1 (M1.2, M1.3) intentionally NOT started yet — resume after
  the two running tasks are reviewed and merged.
- 2026-10-11: M1.1 reviewed and merged into `feat/rust-rewrite` (`de193d5`). Acceptance on the
  merged tree: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test --workspace` all green. Frozen decisions are now in the code; tooling reality
  recorded above.
- 2026-10-11: M1.7 paused on request after its subagent run was interrupted by a transient
  model-availability error. Partial work committed as `8e8bd21` on `feat/m1.7-gix-spike`
  with `spikes/gix-feasibility/STATUS.md` (what works, what is broken, resume commands).
  State: component builds and loads; 3/16 fixture tests pass; blocker is an `UnexpectedEof`
  during status iteration for repos with tracked files. Root-cause breadcrumb: status →
  `gix_status` `index_as_worktree::HashEq::compare_blobs` → `stream_worktree_file` →
  `gix_object::compute_stream_hash` → `gix_hash::io::bytes_with_hasher`, which does
  `read_exact` for the reported stream length; next step is to find why length and stream
  disagree on wasm (probe `read_exact` on a WASI file; inspect `Stream::size()`).
  Resume M1.7 in parallel with wave 1 (it is independent); when merging it into the
  integration branch, expect a small conflict in the root `[workspace]` table
  (`exclude = ["spikes"]` vs the M1.1 members rewrite) — the spike is a standalone workspace
  with its own lockfile, so the conflict is trivial. Wave 1 (M1.2, M1.3) can start next
  session and does not depend on M1.7; M1.8 does.
- 2026-10-12: Environment was reset between sessions: no Rust toolchain, no worktrees, no
  `cargo-component`, and the `targets/` directories gone. Rebuilt: rust 1.99 stable +
  `wasm32-wasip1`/`wasm32-wasip2`, `cargo-component` 0.21.1 (compiled from source; the
  GitHub prebuilt asset was unavailable), and the four worktrees. Baseline on `feat/rust-rewrite`
  (`f018422`) re-verified green.
- 2026-10-12: M1.2 (SDK) and M1.3 (manifest/discovery) implemented by parallel subagents, then
  reviewed and merged into `feat/rust-rewrite` (`efb468c`, `d468f46`); M1.7 resumed in parallel.
  Acceptance on the merged tree all green: `cargo fmt --check`, `cargo clippy --all-targets -- -D
  warnings`, `cargo test --workspace` (23 → 62 tests). The SDK example component builds with
  `scripts/build-example-plugin.sh --release` (wasm32-wasip1). M1.2 delivered the mockable
  `Detector`/`Host` traits, native records, a `MockHost`, and the `bind_detector!` macro
  (WIT↔native glue, `thread_local` state, `check-files` default); M1.3 delivered `plugin.toml`
  parsing (unknown keys warn, unknown capability names are load errors, API major = 1),
  discovery with explicit > user > system > bundled precedence, an empty `bundled::REGISTRY`,
  and `backupcrawl plugins list|info`. Integrator notes: M1.6 must add the crawl-level
  `--plugin-dir` plumbing and consume `discovery::discover`/`PluginSource::Bundled`; M1.5 owns
  manifest hashing/trust and bundled auto-trust. Root `Cargo.lock`/`Cargo.toml` and
  `crates/backupcrawl-host/src/lib.rs` are the shared conflict points for later tasks.
- 2026-10-12: M1.5 (trust flow) merged into `feat/rust-rewrite` (`10a5529`); acceptance green
  (`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`,
  82 tests). The `trust` module hashes raw `plugin.toml` bytes (sha256), auto-trusts bundled
  plugins without prompting, renders capabilities with `exec` first and `fs: read /` plainly,
  and persists hashes into `[plugins.trust]` with `toml_edit` (other tables/comments preserved).
  `plugins list|info` show trust state; `plugins verify` exits 2 when a third-party plugin is
  untrusted. M1.6 should reuse the CLI `apply_trust`/`CliPrompt` on the crawl path.
- 2026-10-12: M1.4 (wasmtime runtime, host imports, limits) merged into `feat/rust-rewrite`
  (`69a8680`); conflict was the expected one-line module list in
  `crates/backupcrawl-host/src/lib.rs` (kept `trust` + `runtime` and the re-exports). Acceptance
  green (111 tests). `Runtime`/`WasmDetector`/`CapabilityPolicy`/`Limits` are the M1.6 entry
  points; capability gating is manifest-driven; HTTP is localhost-only, plain `http` only;
  cache is a per-plugin KV under `$XDG_CACHE_HOME/backupcrawl/plugins`. The test-components
  fixture was rebuilt (scenario-driven) with a single codegen unit for reproducible artifacts.
- 2026-10-12: M1.6 (detector façade + walker integration) implemented on
  `feat/m1.6-walker-integration` (not merged). Core owns the shared records
  (`backupcrawl-core::detector`) and the host re-exports them; `WasmDetector` implements the
  core `Detector` trait (plus an optional `report()` accessor) with no parallel type set. Walker:
  caller order (manifest `hooks.priority`, overridden by `[plugins] order`), first-claim-wins,
  `prune` stops the descent while non-prune continues, `init`/`finish` bracket the scan, loose
  collapsing unchanged. CLI: discovery + trust + `[plugins] enabled/order` + `Runtime`, new
  `--plugins`/`--no-plugins`/`--plugin-dir`/`--trust-plugin`, per-plugin panels, JSON entries and
  the `plugins: git ok, ...` run report line; `plugins list|info|verify` unchanged. Symlinks
  remain skipped until M2. New committed CLI fixture: SDK example component under
  `crates/backupcrawl-cli/tests/fixtures/detectors/dotbak` (rebuild with
  `scripts/build-cli-fixture-plugin.sh`). Acceptance on the worktree: `cargo fmt --check`, clippy
  `-D warnings`, `cargo test --workspace` all green (125 tests). Shared hotspots touched:
  `crates/backupcrawl-core/src/{lib,walker}.rs`, `crates/backupcrawl-host/src/{lib,runtime}.rs`,
  `crates/backupcrawl-cli/src/{main,printer}.rs`.
- 2026-10-12: M1.6 merged (`bcbdebf`); M1.7 (gix spike) finished and merged (`44cbe19`); the only
  conflict was the expected root `Cargo.toml` `[workspace]` one — kept the full members list plus
  `exclude = ["spikes"]`. Final acceptance on `feat/rust-rewrite`: `cargo fmt --check`, `cargo
  clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` all green (126
  tests); `scripts/build-test-components.sh` and `scripts/build-cli-fixture-plugin.sh` both
  reproduce their committed artifacts byte-identically.
- **M1.7 result:** gix 0.89 is workable on `wasm32-wasip2`; the `UnexpectedEof` was the memmap2
  shim advancing the shared `File` cursor (real `mmap` does not), plus two further wasm gaps
  (`filetime` and the Unix exec bit). All 16 porcelain-v2 fixture cases pass; release status on a
  synthetic 50k-file repo is ~3.6 s (native `git status` ~31 ms), so the `exec` backend stays
  selectable. **Recommendation: gix-default for M1.8**, `backend = "exec"` as fallback. Details in
  `docs/rust-rewrite-plan.md` §7.1 and `spikes/gix-feasibility/STATUS.md`.
- 2026-10-12: **Wrap-up.** Wave 1 (M1.1–M1.7) is complete and merged into `feat/rust-rewrite`
  (`4971888` + M1.7 merge `44cbe19`); the tree is green and pushable. Remaining work is parked for
  a next session: **M1.8** (git plugin on gix — fill `bundled::REGISTRY` with
  `plugins/git/dist/{plugin.toml,git.wasm}`; the two spike shims are the reference implementations),
  **M1.9** (exec fallback behind `backend = "exec"`), **M1.10** (plugin-authoring docs + real-scan
  smoke). The Python tree still lives on `main` (`aca128e`); `feat/rust-rewrite` has not been
  merged there or pushed.
- 2026-10-12: **M1.8 merged** (`d7f1519`). Implemented on `feat/m1.8-git-plugin` (`9e5644e`,
  `4559f5e`, `0f21fb4`, `6a0002e`, `a0e3311`). Git detector on gix: `.git` dir/file, bare, linked
  worktree/submodule, ancestor discovery; porcelain-v2 status with `branch.ab`, `no-upstream`,
  `report-ignored`/`ignored-mode`, detached/unborn, submodules; per-repo status cached in the
  instance and served to the file hook. The M1.7 shims are vendored at
  `vendor/{memmap2-shim,filetime}` with `[patch.crates-io]` in the root `Cargo.toml`;
  `plugins/git/dist/{plugin.toml,git.wasm}` is embedded in `bundled::REGISTRY` (bundled →
  auto-trusted). Acceptance on the merged tree: `cargo fmt --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --workspace` green (162 tests: 23 git golden + 13
  native git unit); `scripts/build-git-plugin.sh` reproduces `dist/git.wasm` byte-identically
  (release, `CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1`). `backend = "exec"` is parsed and reserved
  for M1.9 (returns a clear error). Deviations recorded in plan §12: repositories are claimed and
  pruned by default, descending only when `report-ignored` is on; `timeout-ms` is parsed but not
  enforced by gix (host fuel/epoch limits protect); `ancestor-repos` defaults true. Note: the
  earlier "not pushed" wrap-up note is stale — `feat/rust-rewrite` is on `origin` (this branch's
  merges happened after the remote branch was first pushed); no network in this session, so
  nothing was pushed now.
- 2026-10-12: **Perf fix merged** (`fda45f7`, branch `perf/host-component-cache`). `Runtime` now
  caches compiled components keyed by the SHA-256 of the wasm bytes, and the `git_plugin` tests
  share one `Runtime` (`OnceLock`), so the 23 golden tests compile the gix component once instead
  of 23 times. `cargo test --workspace` 2m07s → 27s; `git_plugin` ~2min → 4.6s. This closes the
  §7.1 M1.4 note that the host must cache the compiled component. Remaining test time is dominated
  by the CLI suite (one process per test, each compiling the bundled component).
- 2026-10-12 (session wrap-up): M1.8 and the perf fix are merged; `feat/rust-rewrite` is at
  `89573a1` (plus this note) and green — `cargo fmt --check`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo test --workspace` → **162 passed**. M1.9 was dispatched
  twice and both subagent runs were interrupted by the operator before producing any commits;
  `feat/m1.9-git-exec-fallback` is still at the integration tip and its worktree
  (`worktrees/m1-git-exec`) is clean, so M1.9 can be dispatched as-is next session (the task text
  and frozen decisions are unchanged from the M1.8/M1.9 notes above). Remaining: **M1.9** (git exec
  fallback behind `backend = "exec"`; add `exec = ["git"]`, parity tests) and **M1.10**
  (plugin-authoring docs + real-scan smoke). `feat/rust-rewrite` is **10 commits ahead of
  `origin/feat/rust-rewrite`** and not pushed — the sandbox has no network, so push when
  connectivity returns. Live worktrees: `rust-rewrite` (integration), `m1-git` (M1.8, merged),
  `m1-git-exec` (M1.9, ready), `host-cache` (perf, merged); branches left in place for review.
