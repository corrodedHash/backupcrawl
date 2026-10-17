#!/usr/bin/env python3
"""Run the workspace test suite and record resource metrics.

Measures every test binary (and the doctests) with ``wait4(2)`` so the figures
include the process's waited-for children (the CLI tests spawn ``backupcrawl``;
the git fixtures spawn ``git``). For each suite it records:

* wall-clock time;
* user + system CPU time;
* peak resident set size (max RSS across the process and its reaped children);
* block-layer read/write I/O (``ru_inblock`` / ``ru_oublock``, 512-byte units);
* page faults and context switches.

It also records the build cost, the size of the test artifacts, and the cold
cost of JIT-compiling the bundled git component. Results are compared with a
committed baseline so growth is visible in review.

Usage:
    scripts/test-stats.py                      # collect + print a table
    scripts/test-stats.py --check              # collect + fail on regressions
    scripts/test-stats.py --update-baseline    # write the committed baseline
    scripts/test-stats.py --skip-cold          # skip the ~10 s cold-JIT probe
    scripts/test-stats.py --test-threads 4     # cap libtest fan-out

The measurements are intentionally taken with the repository's normal test
cache (``$TMPDIR/backupcrawl-test-cache``), so the numbers reflect steady-state
local runs. The cold-JIT probe uses a fresh ``TMPDIR`` and is reported
separately.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_BASELINE = ROOT / "docs" / "test-metrics" / "baseline.json"
DEFAULT_OUTPUT = ROOT / "target" / "test-stats" / "latest.json"
DEFAULT_MARKDOWN = ROOT / "target" / "test-stats" / "latest.md"

# A test binary name is `<crate>_<test>-<16 hex chars>`.
HASH_SUFFIX = re.compile(r"-[0-9a-f]{16}$")
TEST_RESULT = re.compile(
    r"test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored"
)

# Ignore deltas below these absolute values: they are noise, not signal.
FLOORS = {
    "wall_s": 0.05,
    "cpu_s": 0.05,
    "max_rss_mib": 8.0,
    "read_mib": 0.5,
    "write_mib": 0.5,
}


@dataclass
class Usage:
    """One measured process tree."""

    wall_s: float
    user_s: float
    sys_s: float
    max_rss_kib: int
    inblock: int
    oublock: int
    minflt: int
    majflt: int
    nvcsw: int
    nivcsw: int
    exit_code: int

    @property
    def cpu_s(self) -> float:
        return self.user_s + self.sys_s

    def metrics(self) -> dict[str, float]:
        return {
            "wall_s": round(self.wall_s, 4),
            "user_s": round(self.user_s, 4),
            "sys_s": round(self.sys_s, 4),
            "cpu_s": round(self.cpu_s, 4),
            "max_rss_mib": round(self.max_rss_kib / 1024, 2),
            "read_mib": round(self.inblock * 512 / (1024 * 1024), 2),
            "write_mib": round(self.oublock * 512 / (1024 * 1024), 2),
            "minflt": self.minflt,
            "majflt": self.majflt,
            "nvcsw": self.nvcsw,
            "nivcsw": self.nivcsw,
            "exit_code": self.exit_code,
        }


def measured(
    argv: list[str],
    *,
    env: dict[str, str] | None = None,
    cwd: Path = ROOT,
    stdout_path: Path | None = None,
    stderr_path: Path | None = None,
) -> Usage:
    """Runs ``argv`` and returns wait4 resource usage.

    ``wait4`` reports the resource usage of the child plus every descendant it
    waited for, which is exactly what a test binary's lifespan covers.
    """

    env = env or os.environ.copy()
    out = open(stdout_path or os.devnull, "wb")
    err = open(stderr_path or os.devnull, "wb")
    started = time.monotonic()
    proc = subprocess.Popen(argv, cwd=cwd, env=env, stdout=out, stderr=err)
    try:
        _, status, usage = os.wait4(proc.pid, 0)
    finally:
        out.close()
        err.close()
    wall = time.monotonic() - started
    try:
        exit_code = os.waitstatus_to_exitcode(status)
    except ValueError:
        exit_code = 128 + (status & 0x7F)
    # subprocess.Popen reaps in wait()/poll(); we reaped it ourselves, so set
    # the code to keep Popen's destructor from warning.
    proc.returncode = exit_code
    return Usage(
        wall_s=wall,
        user_s=usage.ru_utime,
        sys_s=usage.ru_stime,
        max_rss_kib=usage.ru_maxrss,
        inblock=usage.ru_inblock,
        oublock=usage.ru_oublock,
        minflt=usage.ru_minflt,
        majflt=usage.ru_majflt,
        nvcsw=usage.ru_nvcsw,
        nivcsw=usage.ru_nivcsw,
        exit_code=exit_code,
    )


def parse_cargo_json(path: Path) -> list[dict[str, Any]]:
    """Parses compiler messages from a captured cargo JSON stream."""

    messages: list[dict[str, Any]] = []
    for line in path.read_text(errors="replace").splitlines():
        try:
            messages.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return messages


def executable_name(path: str) -> str:
    base = Path(path).name
    return HASH_SUFFIX.sub("", base)


def test_names(binary: Path) -> list[str]:
    completed = subprocess.run(
        [str(binary), "--list"],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        check=False,
    )
    return [
        line.rsplit(": test", 1)[0]
        for line in completed.stdout.splitlines()
        if line.endswith(": test")
    ]


def parse_results(text: str) -> tuple[int, int, int]:
    passed = failed = ignored = 0
    for match in TEST_RESULT.finditer(text):
        passed += int(match.group(2))
        failed += int(match.group(3))
        ignored += int(match.group(4))
    return passed, failed, ignored


def env_fingerprint() -> dict[str, Any]:
    cpu = "unknown"
    try:
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                cpu = line.split(":", 1)[1].strip()
                break
    except OSError:
        pass
    mem_kib = None
    try:
        for line in Path("/proc/meminfo").read_text().splitlines():
            if line.startswith("MemTotal:"):
                mem_kib = int(line.split()[1])
                break
    except OSError:
        pass
    rustc = subprocess.run(
        ["rustc", "--version"], capture_output=True, text=True, check=False
    ).stdout.strip()
    commit = subprocess.run(
        ["git", "rev-parse", "--short", "HEAD"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    ).stdout.strip()
    dirty = bool(
        subprocess.run(
            ["git", "status", "--porcelain"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        ).stdout.strip()
    )
    target = target_dir()
    return {
        "cpu": cpu,
        "logical_cpus": os.cpu_count(),
        "mem_total_mib": round(mem_kib / 1024, 1) if mem_kib else None,
        "kernel": os.uname().release,
        "rustc": rustc,
        "python": sys.version.split()[0],
        "commit": commit,
        "dirty": dirty,
        "target_dir": str(target),
        "test_threads": os.environ.get("RUST_TEST_THREADS", "auto"),
    }


def target_dir() -> Path:
    path = Path(os.environ.get("CARGO_TARGET_DIR", "target"))
    return path if path.is_absolute() else ROOT / path


def dir_size_bytes(path: Path) -> int | None:
    if not path.exists():
        return None
    completed = subprocess.run(
        ["du", "-sb", str(path)], capture_output=True, text=True, check=False
    )
    if completed.returncode != 0:
        return None
    try:
        return int(completed.stdout.split()[0])
    except (ValueError, IndexError):
        return None


def wasm_artifacts() -> list[dict[str, Any]]:
    patterns = [
        "plugins/*/dist/*.wasm",
        "crates/*/tests/fixtures/*.wasm",
        "crates/*/tests/fixtures/**/*.wasm",
    ]
    seen: set[Path] = set()
    artifacts = []
    for pattern in patterns:
        for path in ROOT.glob(pattern):
            resolved = path.resolve()
            if resolved in seen:
                continue
            seen.add(resolved)
            artifacts.append(
                {"path": str(path.relative_to(ROOT)), "bytes": path.stat().st_size}
            )
    return sorted(artifacts, key=lambda item: item["path"])


def cold_jit(git_binary: Path | None, names: list[str]) -> dict[str, Any] | None:
    """Measures a single git test with an empty component cache."""

    if git_binary is None or not names:
        return None
    with tempfile.TemporaryDirectory(prefix="backupcrawl-cold-") as tmp:
        env = os.environ.copy()
        env["TMPDIR"] = tmp
        out = Path(tmp) / "stdout.txt"
        err = Path(tmp) / "stderr.txt"
        usage = measured(
            [str(git_binary), "--exact", names[0], "--test-threads=1"],
            env=env,
            stdout_path=out,
            stderr_path=err,
        )
        passed, failed, _ = parse_results(out.read_text(errors="replace"))
        record = usage.metrics()
        record.update(
            {
                "test": names[0],
                "passed": passed,
                "failed": failed,
                "note": "single git test with a fresh TMPDIR/component cache",
            }
        )
        return record


def render_markdown(report: dict[str, Any]) -> str:
    lines = [
        "# Test resource metrics",
        "",
        f"Generated: {report['generated_at']}",
        f"Commit: `{report['environment']['commit']}`"
        + (" (dirty)" if report["environment"]["dirty"] else ""),
        f"CPU: {report['environment']['cpu']} "
        f"({report['environment']['logical_cpus']} logical)",
        f"Rust: {report['environment']['rustc']}",
        "",
        "| suite | tests | wall (s) | CPU (s) | peak RSS (MiB) | read (MiB) | write (MiB) |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for suite in report["suites"]:
        lines.append(
            "| {name} | {tests} | {wall_s:.3f} | {cpu_s:.3f} | {max_rss_mib:.1f} | "
            "{read_mib:.1f} | {write_mib:.1f} |".format(**suite)
        )
    totals = report["totals"]
    lines.append(
        "| **total** | {tests} | {wall_s:.3f} | {cpu_s:.3f} | {max_rss_mib:.1f} | "
        "{read_mib:.1f} | {write_mib:.1f} |".format(**totals)
    )
    lines += [
        "",
        f"- Build: {report['build']['wall_s']:.3f} s wall, "
        f"{report['build']['cpu_s']:.3f} s CPU, "
        f"{report['build']['max_rss_mib']:.1f} MiB peak RSS.",
        f"- Test artifacts: "
        f"{(report['artifacts']['target_bytes'] or 0) / 1024**3:.2f} GiB "
        f"under `{report['environment']['target_dir']}`.",
    ]
    cold = report.get("cold_jit")
    if cold:
        lines.append(
            f"- Cold JIT probe (`{cold['test']}`): {cold['wall_s']:.3f} s wall, "
            f"{cold['cpu_s']:.3f} s CPU, {cold['max_rss_mib']:.1f} MiB peak RSS."
        )
    return "\n".join(lines) + "\n"


def totals_for(suites: list[dict[str, Any]], doctests: dict[str, Any] | None) -> dict[str, Any]:
    rows = list(suites)
    if doctests:
        rows.append(doctests)
    return {
        "tests": sum(row["tests"] for row in rows),
        "wall_s": round(sum(row["wall_s"] for row in rows), 4),
        "cpu_s": round(sum(row["cpu_s"] for row in rows), 4),
        "max_rss_mib": round(max((row["max_rss_mib"] for row in rows), default=0), 2),
        "read_mib": round(sum(row["read_mib"] for row in rows), 2),
        "write_mib": round(sum(row["write_mib"] for row in rows), 2),
    }


def baseline_view(report: dict[str, Any]) -> dict[str, Any]:
    """The stable subset written to the committed baseline."""

    return {
        "schema": 1,
        "environment": {
            key: report["environment"][key]
            for key in ("cpu", "logical_cpus", "mem_total_mib", "rustc", "target_dir")
        },
        "totals": report["totals"],
        "build": {
            key: report["build"][key]
            for key in ("wall_s", "cpu_s", "max_rss_mib")
        },
        "cold_jit": report.get("cold_jit"),
        "suites": [
            {
                key: suite[key]
                for key in (
                    "name",
                    "tests",
                    "wall_s",
                    "cpu_s",
                    "max_rss_mib",
                    "read_mib",
                    "write_mib",
                )
            }
            for suite in report["suites"]
        ],
        "artifacts": {
            "target_bytes": report["artifacts"]["target_bytes"],
            "wasm": report["artifacts"]["wasm"],
        },
    }


def compare(
    report: dict[str, Any],
    baseline: dict[str, Any],
    *,
    wall_pct: float,
    cpu_pct: float,
    rss_pct: float,
    io_pct: float,
    cold_pct: float,
) -> list[str]:
    problems: list[str] = []
    base_suites = {suite["name"]: suite for suite in baseline.get("suites", [])}
    current_suites = {suite["name"]: suite for suite in report["suites"]}
    metrics = [
        ("wall_s", wall_pct, "wall"),
        ("cpu_s", cpu_pct, "cpu"),
        ("max_rss_mib", rss_pct, "rss"),
        ("read_mib", io_pct, "read"),
        ("write_mib", io_pct, "write"),
    ]
    for name, current in current_suites.items():
        base = base_suites.get(name)
        if base is None:
            print(f"  new suite: {name} ({_suite_line(current)})")
            continue
        for metric, threshold, label in metrics:
            old = base.get(metric, 0)
            new = current.get(metric, 0)
            if old < FLOORS[metric]:
                continue
            if new > old * (1 + threshold / 100):
                problems.append(
                    f"{name}: {label} {old:.3f} -> {new:.3f} "
                    f"(+{(new / old - 1) * 100:.0f}%)"
                )
    old_totals = baseline.get("totals", {})
    new_totals = report["totals"]
    for metric, threshold, label in (
        ("wall_s", wall_pct, "total wall"),
        ("cpu_s", cpu_pct, "total cpu"),
        ("read_mib", io_pct, "total read"),
        ("write_mib", io_pct, "total write"),
    ):
        old = old_totals.get(metric, 0)
        new = new_totals.get(metric, 0)
        if old < FLOORS[metric]:
            continue
        if new > old * (1 + threshold / 100):
            problems.append(
                f"{label}: {old:.3f} -> {new:.3f} (+{(new / old - 1) * 100:.0f}%)"
            )
    # Build cost is reported but not enforced: a source change legitimately
    # triggers a large incremental/cold rebuild, while a no-op run is ~0.2 s.
    old_cold = baseline.get("cold_jit")
    new_cold = report.get("cold_jit")
    if old_cold and new_cold and old_cold.get("wall_s", 0) > 0:
        if new_cold["wall_s"] > old_cold["wall_s"] * (1 + cold_pct / 100):
            problems.append(
                f"cold JIT wall: {old_cold['wall_s']:.3f} -> "
                f"{new_cold['wall_s']:.3f}"
            )
    # The target size is only comparable when the run used the same target dir;
    # worktrees and per-task target dirs legitimately differ.
    old_target = baseline.get("environment", {}).get("target_dir")
    if old_target == report["environment"].get("target_dir"):
        old_artifacts = baseline.get("artifacts", {}).get("target_bytes")
        new_artifacts = report["artifacts"].get("target_bytes")
        if old_artifacts and new_artifacts and new_artifacts > old_artifacts * 1.25:
            problems.append(
                f"target dir: {old_artifacts / 1024**3:.2f} -> "
                f"{new_artifacts / 1024**3:.2f} GiB (+25%)"
            )
    return problems


def _suite_line(suite: dict[str, Any]) -> str:
    return (
        f"{suite['tests']} tests, {suite['wall_s']:.3f}s, "
        f"{suite['cpu_s']:.3f}s CPU, {suite['max_rss_mib']:.1f} MiB"
    )


def print_table(report: dict[str, Any]) -> None:
    print()
    print(
        f"{'suite':24} {'tests':>6} {'wall s':>9} {'cpu s':>9} "
        f"{'rss MiB':>9} {'read MiB':>9} {'write MiB':>10}"
    )
    for suite in report["suites"]:
        print(
            f"{suite['name']:24} {suite['tests']:6d} {suite['wall_s']:9.3f} "
            f"{suite['cpu_s']:9.3f} {suite['max_rss_mib']:9.1f} "
            f"{suite['read_mib']:9.2f} {suite['write_mib']:10.2f}"
        )
    if report.get("doctests"):
        suite = report["doctests"]
        print(
            f"{'doctests':24} {suite['tests']:6d} {suite['wall_s']:9.3f} "
            f"{suite['cpu_s']:9.3f} {suite['max_rss_mib']:9.1f} "
            f"{suite['read_mib']:9.2f} {suite['write_mib']:10.2f}"
        )
    totals = report["totals"]
    print(
        f"{'TOTAL':24} {totals['tests']:6d} {totals['wall_s']:9.3f} "
        f"{totals['cpu_s']:9.3f} {totals['max_rss_mib']:9.1f} "
        f"{totals['read_mib']:9.2f} {totals['write_mib']:10.2f}"
    )
    build = report["build"]
    print(
        f"\nbuild: {build['wall_s']:.3f}s wall, {build['cpu_s']:.3f}s CPU, "
        f"{build['max_rss_mib']:.1f} MiB peak RSS"
    )
    cold = report.get("cold_jit")
    if cold:
        print(
            f"cold JIT ({cold['test']}): {cold['wall_s']:.3f}s wall, "
            f"{cold['cpu_s']:.3f}s CPU, {cold['max_rss_mib']:.1f} MiB peak RSS"
        )
    target_bytes = report["artifacts"].get("target_bytes")
    if target_bytes:
        print(f"target dir: {target_bytes / 1024**3:.2f} GiB")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, default=DEFAULT_BASELINE)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--markdown", type=Path, default=DEFAULT_MARKDOWN)
    parser.add_argument("--check", action="store_true", help="fail on regressions")
    parser.add_argument(
        "--update-baseline", action="store_true", help="overwrite the committed baseline"
    )
    parser.add_argument("--skip-cold", action="store_true", help="skip the cold-JIT probe")
    parser.add_argument(
        "--test-threads", type=int, default=None, help="passed to every test binary"
    )
    parser.add_argument("--max-wall", type=float, default=25.0, help="percent")
    parser.add_argument("--max-cpu", type=float, default=25.0, help="percent")
    parser.add_argument("--max-rss", type=float, default=20.0, help="percent")
    parser.add_argument("--max-io", type=float, default=50.0, help="percent")
    parser.add_argument("--max-cold", type=float, default=40.0, help="percent")
    parser.add_argument(
        "--allow-test-failures",
        action="store_true",
        help="collect metrics even when tests fail",
    )
    args = parser.parse_args()

    if args.test_threads is not None:
        os.environ["RUST_TEST_THREADS"] = str(args.test_threads)

    output_dir = args.output.parent
    output_dir.mkdir(parents=True, exist_ok=True)
    build_err = output_dir / "build.stderr"

    print("building test binaries ...", flush=True)
    build_out = output_dir / "build.jsonl"
    build = measured(
        [
            "cargo",
            "test",
            "--workspace",
            "--no-run",
            "--message-format=json",
        ],
        env={**os.environ, "CARGO_TERM_COLOR": "never"},
        stdout_path=build_out,
        stderr_path=build_err,
    )
    if build.exit_code != 0:
        print(f"build failed; see {build_err}", file=sys.stderr)
        return 1
    messages = parse_cargo_json(build_out)

    tests: list[tuple[str, Path]] = []
    seen_tests: set[Path] = set()
    for message in messages:
        if message.get("reason") != "compiler-artifact":
            continue
        executable = message.get("executable")
        if not executable:
            continue
        if not message.get("profile", {}).get("test"):
            continue
        path = Path(executable)
        if path in seen_tests:
            continue
        seen_tests.add(path)
        tests.append((executable_name(str(path)), path))

    tests.sort(key=lambda item: item[0])

    # Warm the shared component cache the same way the suite does, so steady
    # state metrics are not dominated by a one-off JIT compile.
    git_name, git_binary = next(
        ((name, path) for name, path in tests if name == "git_plugin"), ("", None)
    )
    git_tests = test_names(git_binary) if git_binary else []
    warmup = None
    if git_tests:
        print("warming the shared component cache ...", flush=True)
        warmup = measured(
            [str(git_binary), "--exact", git_tests[0], "--test-threads=1"],
            stdout_path=output_dir / "warmup.stdout",
            stderr_path=output_dir / "warmup.stderr",
        ).metrics()

    suites: list[dict[str, Any]] = []
    failed_tests = 0
    for index, (name, path) in enumerate(tests, 1):
        print(f"[{index}/{len(tests)}] {name} ...", flush=True)
        stdout_path = output_dir / f"{name}.stdout"
        stderr_path = output_dir / f"{name}.stderr"
        argv = [str(path)]
        if args.test_threads is not None:
            argv.append(f"--test-threads={args.test_threads}")
        usage = measured(argv, stdout_path=stdout_path, stderr_path=stderr_path)
        passed, failed, ignored = parse_results(
            stdout_path.read_text(errors="replace")
        )
        if failed:
            failed_tests += failed
        elif usage.exit_code != 0 and not args.allow_test_failures:
            # Crash, abort or a non-test exit status without a result line.
            failed_tests += 1
        record = {
            "name": name,
            "tests": passed + failed,
            "passed": passed,
            "failed": failed,
            "ignored": ignored,
            **usage.metrics(),
        }
        suites.append(record)

    doctests = None
    print("doctests ...", flush=True)
    doc_out = output_dir / "doctests.stdout"
    doc_err = output_dir / "doctests.stderr"
    doc_usage = measured(
        ["cargo", "test", "--workspace", "--doc", "--quiet"],
        stdout_path=doc_out,
        stderr_path=doc_err,
    )
    doc_passed, doc_failed, doc_ignored = parse_results(
        doc_out.read_text(errors="replace")
    )
    if doc_usage.exit_code != 0 and not args.allow_test_failures:
        failed_tests += max(doc_failed, 1)
    if doc_passed or doc_failed:
        doctests = {
            "name": "doctests",
            "tests": doc_passed + doc_failed,
            "passed": doc_passed,
            "failed": doc_failed,
            "ignored": doc_ignored,
            **doc_usage.metrics(),
        }

    target_bytes = dir_size_bytes(target_dir())
    report = {
        "schema": 1,
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "environment": env_fingerprint(),
        "totals": totals_for(suites, doctests),
        "build": build.metrics(),
        "warmup": warmup,
        "cold_jit": None if args.skip_cold else cold_jit(git_binary, git_tests),
        "suites": suites,
        "doctests": doctests,
        "artifacts": {"target_bytes": target_bytes, "wasm": wasm_artifacts()},
    }

    args.output.write_text(json.dumps(report, indent=2) + "\n")
    args.markdown.write_text(render_markdown(report))
    print_table(report)
    print(f"\nwrote {args.output} and {args.markdown}")

    if failed_tests and not args.allow_test_failures:
        print(f"{failed_tests} test(s) failed", file=sys.stderr)
        return 1

    if args.update_baseline:
        args.baseline.parent.mkdir(parents=True, exist_ok=True)
        args.baseline.write_text(json.dumps(baseline_view(report), indent=2) + "\n")
        print(f"wrote baseline {args.baseline}")
        return 0

    if args.check:
        if not args.baseline.exists():
            print(
                f"no baseline at {args.baseline}; run with --update-baseline",
                file=sys.stderr,
            )
            return 2
        baseline = json.loads(args.baseline.read_text())
        print(f"\ncomparing against {args.baseline} ...")
        problems = compare(
            report,
            baseline,
            wall_pct=args.max_wall,
            cpu_pct=args.max_cpu,
            rss_pct=args.max_rss,
            io_pct=args.max_io,
            cold_pct=args.max_cold,
        )
        if problems:
            print("regressions:", file=sys.stderr)
            for problem in problems:
                print(f"  - {problem}", file=sys.stderr)
            return 1
        print("no regressions beyond thresholds")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
