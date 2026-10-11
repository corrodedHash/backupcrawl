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
| M1.1 foundation | `worktrees/m1-foundation` | `feat/m1.1-foundation` | running (dispatched 2026-10-11) |
| M1.7 gix spike | `worktrees/m1-gix-spike` | `feat/m1.7-gix-spike` | running (dispatched 2026-10-11) |
| M1.2 SDK | `worktrees/m1-sdk` (to create) | `feat/m1.2-plugin-sdk` | not started |
| M1.3 manifest/discovery | `worktrees/m1-manifest` (to create) | `feat/m1.3-manifest-discovery` | not started |
| M1.4 host runtime | `worktrees/m1-host` (to create) | `feat/m1.4-wasm-host` | not started |
| M1.5 trust flow | `worktrees/m1-trust` (to create) | `feat/m1.5-trust-flow` | not started |
| M1.8 git plugin | `worktrees/m1-git` (to create) | `feat/m1.8-git-plugin` | not started |
| M1.6 walker integration | `worktrees/m1-integration` (to create) | `feat/m1.6-walker-integration` | not started |
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
