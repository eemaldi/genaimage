#!/usr/bin/env python3
"""Report informational performance-budget measurements for the release CLI."""

import argparse
import os
import statistics
import subprocess
import sys
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
COLD_START_BUDGET_MS = 150.0
IDLE_RSS_BUDGET_MB = 40.0
BINARY_SIZE_BUDGET_MB = 25.0


def release_binary():
    target_dir = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")).expanduser()
    name = "genaimage.exe" if os.name == "nt" else "genaimage"
    return target_dir / "release" / name


def launch_sample(binary):
    """Return process-start milliseconds and per-child peak RSS in decimal MB."""
    start = time.perf_counter_ns()
    if os.name == "posix" and hasattr(os, "wait4"):
        null_fd = os.open(os.devnull, os.O_WRONLY)
        pid = os.fork()
        if pid == 0:
            try:
                os.dup2(null_fd, 1)
                os.dup2(null_fd, 2)
                os.execv(str(binary), [str(binary), "--version"])
            except OSError:
                os._exit(127)
        os.close(null_fd)
        _, status, usage = os.wait4(pid, 0)
        elapsed_ms = (time.perf_counter_ns() - start) / 1_000_000
        if not os.WIFEXITED(status) or os.WEXITSTATUS(status) != 0:
            raise subprocess.CalledProcessError(os.waitstatus_to_exitcode(status), [str(binary), "--version"])
        rss_bytes = usage.ru_maxrss if sys.platform == "darwin" else usage.ru_maxrss * 1024
        return elapsed_ms, rss_bytes / 1_000_000

    completed = subprocess.run(
        [str(binary), "--version"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=True,
    )
    del completed
    elapsed_ms = (time.perf_counter_ns() - start) / 1_000_000
    return elapsed_ms, None


def budget_status(value, budget):
    if value is None:
        return "NOT MEASURED"
    return "OVER BUDGET" if value >= budget else "WITHIN BUDGET"


def render_report(cold_start_ms, idle_rss_mb, binary_size_mb, samples):
    rows = [
        "## Performance budget (informational)",
        "",
        "| Metric | Result | Budget | Status |",
        "| --- | ---: | ---: | --- |",
        f"| Cold start (median of {samples}) | {cold_start_ms:.2f} ms | < 150 ms | {budget_status(cold_start_ms, COLD_START_BUDGET_MS)} |",
        (
            f"| Idle RSS approximation (median peak of {samples}) | {idle_rss_mb:.2f} MB | < 40 MB | {budget_status(idle_rss_mb, IDLE_RSS_BUDGET_MB)} |"
            if idle_rss_mb is not None
            else "| Idle RSS approximation | Not available on this platform | < 40 MB | NOT MEASURED |"
        ),
        f"| Release binary size | {binary_size_mb:.2f} MB | < 25 MB | {budget_status(binary_size_mb, BINARY_SIZE_BUDGET_MB)} |",
        "| Portal initial JavaScript | Not measurable yet (portal does not exist yet) | < 150 KB gzip | NOT MEASURED |",
        "| p95 non-ComfyUI API latency | Not measurable yet (server/API does not exist yet) | < 20 ms | NOT MEASURED |",
        "",
        "Cold start and RSS use `genaimage --version`; RSS is a short-lived CLI proxy, not idle server memory. Over-budget measurements are informational and do not fail this run.",
    ]
    return "\n".join(rows)


def append_summary(report, summary_path):
    with summary_path.open("a", encoding="utf-8") as summary:
        summary.write("\n")
        summary.write(report)
        summary.write("\n")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--quick",
        action="store_true",
        help="use five samples instead of the default fifteen",
    )
    parser.add_argument(
        "--no-build",
        action="store_true",
        help="use an existing release binary without building it",
    )
    args = parser.parse_args(argv)
    binary = release_binary()

    try:
        if not args.no_build:
            subprocess.run(
                ["cargo", "build", "--locked", "--release", "-p", "genaimage"],
                cwd=ROOT,
                check=True,
            )
        if not binary.is_file():
            raise FileNotFoundError(f"release executable not found: {binary}")
        samples = 5 if args.quick else 15
        measurements = [launch_sample(binary) for _ in range(samples)]
        cold_start_ms = statistics.median(sample[0] for sample in measurements)
        rss_samples = [sample[1] for sample in measurements if sample[1] is not None]
        idle_rss_mb = statistics.median(rss_samples) if rss_samples else None
        binary_size_mb = binary.stat().st_size / 1_000_000
        report = render_report(cold_start_ms, idle_rss_mb, binary_size_mb, samples)
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"Performance measurement failed: {error}", file=sys.stderr)
        return 1

    print(report)
    summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary_path:
        append_summary(report, Path(summary_path))
    return 0


if __name__ == "__main__":
    sys.exit(main())
