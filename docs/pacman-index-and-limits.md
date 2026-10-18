# Pacman index cost and host limits

Investigation and fix, 2026-10-18 · relates to `docs/rust-rewrite-plan.md` §7.2 and §9

## Symptom

Scanning a small tree (a project directory, a temporary fixture) disabled the
bundled pacman detector after emitting ~1,400 `path ... is claimed by both ...`
warnings:

```
detector `pacman` disabled: init trapped: ... all fuel consumed by WebAssembly
```

The scan itself was correct apart from the missing pacman claims, but the plugin
was unusable on a normal Arch installation.

## Measurements

Host used for the numbers below: Arch Linux, 1,691 packages in
`/var/lib/pacman/local`, 520,494 lines across the package `files` databases,
479,302 unique owned paths after deduplication. The bundled release component
was used throughout.

| Scenario | Fuel consumed by `init` | Wall time (cold) | Result |
| --- | ---: | ---: | --- |
| Full index (crawl root `/`) | ~77.3 billion | 8–10 s | succeeds with the new limits |
| `/usr/bin` (scoped) | ~32.8 billion | ~3.4 s | succeeds |
| Project/temp root (scoped, no owned paths) | ~12 billion | ~1 s | succeeds after reading the `files` lists |
| Any root, before the fix | >10 billion | — | traps, plugin disabled |

Memory for the full index:

| Guest memory cap | Result |
| --- | --- |
| 256 MiB (old default) | OOM inside `serde_json` while writing the cache |
| 384 MiB | OOM |
| 448 MiB | OOM |
| 512 MiB | succeeds |

The serialized full index is 128,472,003 bytes (~123 MiB). Peak live memory is
roughly 450 MiB: the parsed index plus the JSON buffer exist at the same time.

## Root causes

1. **The index was not scoped to the crawl root.** `init` read every package's
   `files` and parsed every gzipped `mtree`, even when the scan could never ask
   about `/usr` or `/etc`. That is the dominant cost for ordinary scans and the
   reason the plugin was disabled for a project-tree crawl.
2. **One fuel budget covered every call.** `fuel_per_call` was 10 billion, while
   preparation legitimately needs ~77 billion on a full system.
3. **The memory cap was 256 MiB.** It was an example value in the plan, not a
   measured requirement, and the eager full-system index does not fit.
4. **Duplicate ownership was logged per path.** Shared directories are normal
   (`/usr`, `/usr/bin`, ... are owned by every package), so a full build emitted
   thousands of warnings. Meta packages with an empty `files` file (`base`,
   `base-devel`, `ca-certificates`, ...) were also reported as parse errors.
5. **Fuel exhaustion cannot be retried.** The first attempted fix caught
   `Trap::OutOfFuel`, refilled the budget and retried `init`. Wasmtime rejects
   the second call with `cannot enter component instance` because the component
   instance is still considered entered after a trap. A soft retry is therefore
   not possible without changing the component lifecycle (new instance, replay
   from the start), which would rebuild everything anyway.

## Implemented fix

### Pacman: scope the index

- `Index` contains only paths at or below the canonical crawl root.
- `files` is read first; `desc` and `mtree` are only parsed for packages that
  own at least one relevant path.
- The host cache key includes the crawl root, so different scopes do not
  invalidate or corrupt each other. `INDEX_FORMAT` was bumped to 2.
- Empty `files` databases are a normal empty path list.
- Duplicate ownership is counted and logged once at debug level.

### Host: separate `init` budget

- `Limits::fuel_per_call` stays at 10 billion for `check_*` and `finish`, so a
  runaway check is still cut off quickly.
- `Limits::init_fuel` is 200 billion (about 2.5× the measured full-system
  need), so a runaway `init` is still bounded but legitimate preparation does
  not fail.
- A successful `init` that uses more than half of `init_fuel` logs a warning and
  records a note. Exceeding the ordinary per-call budget is expected for pacman
  and is only shown at info/debug level.
- An `OutOfFuel` trap during `init` is reported as
  `init exhausted its <n>-fuel budget: ...` rather than a bare trap.
- `max_memory` is 512 MiB. This is a ceiling, not a reservation; scoped indexes
  stay far below it.

## Behavior after the fix

- A project-tree scan reports `plugins: git ok, pacman ok, ...` with no pacman
  warnings.
- `/usr/bin` indexes 5,085 path claims in ~3.4 s cold and ~0.1 s warm.
- A full `/` scan can build the index (~77 billion fuel, ~450 MiB); it is still
  the most expensive pacman case.
- `--jobs N` creates one detector instance per worker. Scoped indexes are
  cheap; a full-system index is replicated per worker that misses the host cache
  after the first worker warms it. Treat full `/` scans with many workers as a
  memory-heavy operation.

## Follow-ups (not done here)

- The cache is still JSON: the 128 MiB serialized full index and the parsed
  image coexist during `cache_set`, which is what pushes peak memory to ~450 MiB.
  A compact binary format (and/or streaming the write) would lower both the peak
  and the on-disk size.
- The index is still eager. A lazy per-package `mtree` lookup would make
  `init` nearly free, at the cost of more file reads during the scan.
- Per-plugin memory/fuel budgets (rather than host-wide values) would let
  pacman keep a larger ceiling without granting it to every detector.

## Reproduction

```sh
# unit/host suites
cargo test -p backupcrawl-plugin-pacman -p backupcrawl-host

# real-system smoke runs
cargo build --release -p backupcrawl-cli
./target/release/backupcrawl ~ --plugins pacman
./target/release/backupcrawl /usr/bin --plugins pacman --format json
```

To measure fuel on another machine, set `Limits::init_fuel` high, instantiate
the bundled pacman component, call `init` with the desired root and compare
`WasmDetector::fuel_remaining()` with `init_fuel`. `cargo test` must run
somewhere with a real pacman database; CI has no Arch host, so these numbers are
not a regression test.
