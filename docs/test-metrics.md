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
* a **cold-JIT probe**: one git test with a fresh `TMPDIR`, which measures the
  ~9 s one-time wasmtime JIT compile of the bundled 3.3 MB gix component;
* the **size of the target directory** and every committed `.wasm` artifact.

Peak RSS is a maximum, not a sum, for the same reason `getrusage` reports a
maximum: concurrent test threads share the process tree. The suite wall/CPU/IO
are sums of the per-suite rows; RSS is the maximum row.

Measurements use the repository's normal test component cache
(`$TMPDIR/backupcrawl-test-cache`), so they reflect steady-state local runs. The
cold probe deliberately uses a fresh cache and is reported separately.

## Usage

    scripts/test-stats.py                    # collect and print a table
    scripts/test-stats.py --check            # collect and fail on regressions
    scripts/test-stats.py --update-baseline  # refresh docs/test-metrics/baseline.json
    scripts/test-stats.py --skip-cold        # skip the ~10 s cold-JIT probe
    scripts/test-stats.py --test-threads 4   # cap libtest fan-out

`--check` compares the run against `docs/test-metrics/baseline.json` and exits
non-zero when a metric grows beyond its threshold (defaults: 25% wall and CPU,
20% peak RSS, 50% block I/O, 40% cold JIT, 25% target size). Deltas below the
noise floors (50 ms, 8 MiB RSS, 0.5 MiB I/O) are ignored.

The full run and a Markdown table are written to
`target/test-stats/latest.json` and `target/test-stats/latest.md` (gitignored).

## Policy

* During development, run the tests of the crate you changed. Run the full
  suite with `scripts/test-stats.py --check` at merge time.
* Never `cargo clean` between iterations: keeping the component cache warm is
  what turns a ~9 s cold run into a ~3 s warm one.
* Run the git/CLI suites with `--test-threads 4` when memory or process fan-out
  matters.
* Update the baseline in the same commit as an intentional change to the test
  suite (new tests, a new plugin component, a slower fixture). An unexplained
  regression is a bug, not a baseline update.

## Baseline and CI

`docs/test-metrics/baseline.json` is committed. It is machine-specific in two
places (the CPU model in `environment` and the absolute target size); the suite
metrics themselves are what regressions are compared against.

CI replaces the plain `cargo test --workspace` step with
`scripts/test-stats.py`, so every run uploads a `test-stats` artifact with the
same JSON and Markdown. CI does not run `--check`: GitHub runners differ from
developer machines, so the baseline comparison is a local tool. Use the uploaded
artifact to compare runs over time.
