# Test resource metrics

The test suite is intentionally heavier than a normal unit suite: it runs
wasmtime detector components, spawns the `backupcrawl` binary in the CLI tests
and creates real git repositories in the git golden tests. `scripts/test-stats.py`
measures that cost so regressions are visible in review instead of only felt
locally.

## What is measured

Every test binary (and the doctests) runs under `wait4(2)`, so the figures
include the process's waited-for descendants — the spawned `backupcrawl`
processes and `git` invocations. For each suite the script records:

| Metric | Meaning |
| --- | --- |
| `wall_s` | wall-clock time of the whole suite process |
| `cpu_s` | user + system CPU time (summed over all threads and reaped children) |
| `max_rss_mib` | peak resident set size, in MiB (the maximum across the process and its reaped children) |
| `read_mib` / `write_mib` | block-layer I/O (`ru_inblock`/`ru_oublock`, 512-byte units); page-cache hits show as 0 |
| `minflt` / `majflt` | minor / major page faults |
| `nvcsw` / `nivcsw` | voluntary / involuntary context switches |

It also records:

* the **build** step (`cargo test --workspace --no-run`) with the same metrics;
* **cold-JIT probes**: one test from each bundled-plugin suite (`git_plugin`,
  `pacman_plugin`, `chezmoi_plugin`, `syncthing_plugin`) with a fresh `TMPDIR`,
  measuring the one-time wasmtime JIT compile of each component (the git/gix
  component alone is ~9 s);
* the **size of the target directory** and every generated `.wasm` artifact.

Peak RSS is a maximum, not a sum, for the same reason `getrusage` reports a
maximum: concurrent test threads share the process tree. The suite wall/CPU/IO
are sums of the per-suite rows; RSS is the maximum row.

Measurements use the repository's normal test component cache
(`$TMPDIR/backupcrawl-test-cache`), so they reflect steady-state local runs. The
cold probe deliberately uses a fresh cache and is reported separately.

## Usage

    scripts/test-stats.py                    # collect and print a table
    scripts/test-stats.py --check            # collect and fail on regressions
    scripts/test-stats.py --check --force    # compare timings on any host
    scripts/test-stats.py --update-baseline  # refresh docs/test-metrics/baseline.json
    scripts/test-stats.py --skip-cold        # skip the ~10 s cold-JIT probe
    scripts/test-stats.py --test-threads 4   # cap libtest fan-out

`--check` compares the run against `docs/test-metrics/baseline.json` and exits
non-zero when a metric grows beyond its threshold (defaults: 25% wall and CPU,
20% peak RSS, 50% block I/O, 40% cold JIT, 25% target size). Deltas below the
noise floors (50 ms, 8 MiB RSS, 0.5 MiB I/O) are ignored.

Wall time, CPU time, peak RSS, block I/O and cold-JIT cost are machine-dependent,
so they are only compared when the current machine matches the baseline host
fingerprint. On a different host `--check` prints the fields that differ, skips
those comparisons, and still runs the deterministic checks:

* every baseline suite still runs the same number of tests (or more);
* no baseline suite has disappeared;
* the generated `.wasm` artifacts are still present (and have not grown by more
  than 25%);
* the target directory has not grown by more than 25%, but only when the run
  used the same `target_dir` as the baseline.

`--force` removes the host gate and compares all the host-sensitive metrics
anyway. It is useful after a deliberate hardware or toolchain change, when the
absolute numbers are what you want to inspect.

The full run and a Markdown table are written to
`target/test-stats/latest.json` and `target/test-stats/latest.md` (gitignored).

## Policy

* During development, run the tests of the crate you changed. Run the full
  suite with `scripts/test-stats.py --check` at merge time (on the baseline host,
  or against a local baseline; see below).
* Never `cargo clean` between iterations: keeping the component cache warm is
  what turns a ~9 s cold run into a ~3 s warm one.
* Run the git/CLI suites with `--test-threads 4` when memory or process fan-out
  matters.
* Update the committed baseline in the same commit as an intentional change to
  the test suite (new tests, a new plugin component, a slower fixture). An
  unexplained regression is a bug, not a baseline update.

## Baseline and CI

`docs/test-metrics/baseline.json` uses schema 2. It records a **host
fingerprint** under `host`:

| Field | Meaning |
| --- | --- |
| `os` / `arch` | operating system and architecture, from `uname(2)` |
| `cpu` | CPU model name |
| `logical_cpus` | logical CPU count |
| `mem_total_mib` | total memory (a sub-1% difference is treated as the same host) |
| `rustc` | exact `rustc --version` |
| `target_dir` | target directory used for the run |

`--check` compares the current fingerprint with that record. If any field
differs, the host-sensitive comparisons (wall, CPU, RSS, I/O and cold JIT) are
skipped with an explanation instead of reporting false regressions; the
deterministic checks still run. The target-size check keeps its own
`target_dir` guard, so it also stays quiet across worktrees.

Schema 1 baselines have no OS/architecture and no `host` object; their
`environment` record is used as a partial fingerprint, so the check still works
until the baseline is refreshed.

### A local baseline

`--baseline` accepts an alternate path. To get a meaningful timing baseline on
a machine that is not the committed-baseline host, keep one outside the
repository — for example under the gitignored `target/`:

    scripts/test-stats.py --update-baseline \
        --baseline target/test-metrics/baseline.json
    scripts/test-stats.py --check \
        --baseline target/test-metrics/baseline.json

Refresh the local baseline when the test suite or toolchain changes
intentionally. Leave the committed baseline alone unless you are on its host.

### CI

CI replaces the plain `cargo test --workspace` step with
`scripts/test-stats.py`, so every run uploads a `test-stats` artifact with the
same JSON and Markdown. CI does not run `--check`: GitHub runners differ from
developer machines, so the baseline comparison is a local tool. Use the uploaded
artifact to compare runs over time.
