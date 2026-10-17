# M2/4a orchestration plan and status

This file tracks the revised post-M1 sequence described in
`docs/rust-rewrite-plan.md` §12 (ordering decision, 2026-10-17). The task definitions and
acceptance criteria there are the source of truth; this file records the orchestration
decisions, worktrees and current state. M4b and M5 are intentionally out of scope for this
sequence.

## Task graph and waves

```
M2.0 contract hardening (grants + concurrency rules)
 └── M2 pacman + symlinks
      └── M4a parallel crawl + progress + init single-flight
           └── M3 chezmoi + syncthing (two independent tasks, one wave)
```

Each task branches from `feat/rust-rewrite`, is implemented in its own worktree, and is merged
with `git merge --no-ff` only after the acceptance suite is green on the merged tree. M3.1 and
M3.2 are dispatched in parallel from the same post-M4a tip.

## Worktrees and branches

Integration branch: `feat/rust-rewrite` in `/var/lib/opencode-agent/coding/backupcrawl`.

| Task | Branch | Status |
| --- | --- | --- |
| M2.0 contract hardening | `feat/m2.0-contract-hardening` | done, merged as `fbc5dd2` |
| M2 pacman + symlinks | `feat/m2-pacman-symlinks` | done, merged as `72d8f3c` |
| M4a parallel + progress | `feat/m4a-parallel-progress` | implemented on branch, awaiting review |
| M3.1 chezmoi | `feat/m3.1-chezmoi` | not started |
| M3.2 syncthing | `feat/m3.2-syncthing` | not started |

## Frozen decisions for this sequence

### Grant model (M2.0)

- Every detector instance gets a read-only WASI preopen for the **crawl root**, plus every
  resolved `[capabilities].fs-read` spec.
- Specs support `~` expansion and shell-style globs; each resolves to existing directories.
  Unresolved specs are warnings, not load errors. Nested roots are deduplicated to the broadest.
- Each root is mapped at its **own absolute guest path**, so `std::fs`/`gix` absolute paths work.
- `stat` stays unconditional and metadata-only. Content outside the granted roots is inaccessible.
- Bundled plugins may declare broad roots honestly: git declares `fs-read = ["/"]` so ancestor
  repository discovery keeps working across the crawl-root boundary; pacman also declares `/`
  because package-owned files are system-wide.
- `plugins info` and the trust prompt print the resolved scope (`fs: read <roots>`), replacing
  the old "documented intent" wording.
- Per-path read auditing and path-scoped `stat` are explicitly deferred.

### Concurrency and cache rules (frozen now, implemented in M4a)

- One detector instance per worker. `init`/`finish` bracket that instance's lifetime, so a scan
  with N workers calls each plugin's `init`/`finish` N times.
- Instances share no mutable state. Global preparation belongs in the host cache; cache-write
  races between workers must be benign.
- The first worker's `init` is serialized and warms the cache; the remaining workers' `init`
  calls then hit it (the M4a single-flight guard).
- There are no cross-worker ordering guarantees. First-claim-wins is still evaluated per path,
  and every path is handled by exactly one worker.
- Output stays deterministic: per-worker claims are merged and sorted in the host after the walk,
  and loose-path collapsing happens bottom-up after traversal.
- Plugins must not assume one instance per process, process-global mutable state, or hook order
  across unrelated paths.

## Integration procedure

1. Inspect the task branch diff; check out or merge it into `feat/rust-rewrite` with
   `git merge --no-ff`.
2. Run acceptance on the merged tree:
   `cargo fmt --check`,
   `cargo clippy --workspace --all-targets -- -D warnings`,
   `scripts/test-stats.py --check` (runs the workspace tests and compares
   wall/CPU/RSS/IO/cold-JIT against `docs/test-metrics/baseline.json`; see
   `docs/test-metrics.md`). During task development, run only the touched
   crate's tests; full stats are for merge review.
3. Rebuild every changed plugin artifact and confirm byte-identical output:
   `scripts/build-git-plugin.sh`, `scripts/build-pacman-plugin.sh`,
   `scripts/build-chezmoi-plugin.sh`, `scripts/build-syncthing-plugin.sh` (as they land).
4. Only then dispatch the next wave.
5. Never push; leave branches for review.

## Environment notes

- Build with `CARGO_TARGET_DIR=/var/lib/opencode-agent/targets/<task>` to keep generated
  artifacts off the repository filesystem.
- `cargo component` 0.21.1 is installed; plugin builds default to `wasm32-wasip1` and the git
  plugin uses `wasm32-wasip2`.
- The git plugin build must stay byte-reproducible
  (`CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1`); copy the same pattern for new plugins.
- Multi-worker tests must not depend on thread scheduling or timing for correctness. Use
  output-equality assertions between `--jobs 1` and `--jobs N`.

## Status log

- 2026-10-17: sequence recorded in `docs/rust-rewrite-plan.md` §12; M2.0/M2/M4a/M3 task
  breakdowns added; M4b and M5 marked deferred. Worktrees to be created per wave.
- 2026-10-17: **M2 merged** (`72d8f3c`, branch `feat/m2-pacman-symlinks`: `6d4ee61`,
  `364e6dc`, `ccde53d`). Core reports symlinks instead of dropping them
  (`SymlinkEntry`/`CrawlResult.symlinks`, console panel, JSON array, the target-in-managed-tree
  rule over clean pruning dir claims). New bundled pacman detector: native `files`/`desc`/gzipped
  BSD `mtree` parsing with `/set` defaults and octal escapes, metadata + optional md5/sha256
  verification, reasons in the `reason` attr, a cached index keyed by db path + `local/` mtime,
  `fs-read = ["/"]`, `cache = true`, priority 20; `scripts/build-pacman-plugin.sh` and CI
  reproduce `plugins/pacman/dist/pacman.wasm` byte-identically. Acceptance on the merged tree:
  fmt, clippy `-D warnings`, `scripts/test-stats.py --check` green — **250 tests**, ~5.6 s warm
  wall, ~20.6 s CPU, 178 MiB peak RSS; baseline refreshed. Deviations: absolute symlink targets
  are read through the walker's `LinkInfo` because cap-std refuses `read_link` on them; deleted
  package files are only observable when asked directly (the walker only asks about existing
  paths) and remain a follow-up; `backend = "pacman"` is rejected as not implemented.
- 2026-10-17: **M2.0 merged** (`fbc5dd2`, branch `feat/m2.0-contract-hardening`:
  `2ec51a6`, `1cabe1a`, `a614168`). New `backupcrawl-host::fs` resolves `fs-read` specs
  (`~` + shell globs), deduplicates nested roots to the broadest and preopens the crawl root plus
  the resolved roots read-only at their absolute guest paths; unresolved specs warn. `stat` stays
  unconditional and metadata-only. `Runtime::instantiate_with_fs`/`instantiate_plugin(root)` carry
  the grants; the fixture component gained a `read` scenario proving inside/outside access; the
  committed fixture was rebuilt reproducibly (139,443 bytes). `plugins info`/the trust prompt now
  render the real scope; the bundled git manifest declares `fs-read = ["/"]`. The per-worker
  lifecycle/cache contract is documented in the core trait, SDK, plugin-authoring guide and plan
  §9. Acceptance on the merged tree: fmt, clippy `-D warnings`, 196 tests green, cold-JIT probe
  9.1 s. Baseline refreshed in `docs/test-metrics/baseline.json` (196 tests, ~3.7 s warm wall,
  ~17.9 s CPU, 175 MiB peak RSS, 14.7 GiB target). The new test metrics harness
  (`scripts/test-stats.py`, `docs/test-metrics.md`) landed just before the merge and CI now runs
  the suite through it.
