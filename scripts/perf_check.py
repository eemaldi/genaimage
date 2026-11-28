#!/usr/bin/env python3
"""Report informational performance-budget measurements for the release CLI."""

import argparse
import gzip
import json
import math
import os
import statistics
import subprocess
import sys
import time
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit


ROOT = Path(__file__).resolve().parent.parent
COLD_START_BUDGET_MS = 150.0
IDLE_RSS_BUDGET_MB = 40.0
BINARY_SIZE_BUDGET_MB = 25.0
PORTAL_JS_BUDGET_KB = 150.0


def cargo_target_dir():
    target_dir = Path(os.environ.get("CARGO_TARGET_DIR") or "target").expanduser()
    return target_dir if target_dir.is_absolute() else ROOT / target_dir


def release_binary():
    name = "genaimage.exe" if os.name == "nt" else "genaimage"
    return cargo_target_dir() / "release" / name


def find_gnu_time():
    candidate = Path("/usr/bin/time")
    if not sys.platform.startswith("linux") or not candidate.is_file():
        return None
    try:
        result = subprocess.run(
            [str(candidate), "--version"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            check=False,
            text=True,
        )
    except OSError:
        return None
    return candidate if "GNU time" in result.stdout else None


def proc_vm_hwm_bytes(pid):
    try:
        with open(f"/proc/{pid}/status", encoding="ascii") as status:
            for line in status:
                if line.startswith("VmHWM:"):
                    return int(line.split()[1]) * 1024
    except (OSError, ValueError):
        return None
    return None


def peak_rss_with_gnu_time(binary, time_executable):
    completed = subprocess.run(
        [str(time_executable), "-f", "%M", str(binary), "--version"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        check=True,
        text=True,
    )
    try:
        peak_rss_kb = int(completed.stderr.strip().splitlines()[-1])
    except (IndexError, ValueError) as error:
        raise ValueError("GNU time did not report peak RSS") from error
    return peak_rss_kb * 1024 / 1_000_000


def peak_rss_from_proc(binary):
    command = [str(binary), "--version"]
    process = subprocess.Popen(
        command,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    peak_rss_bytes = 0
    while True:
        sample = proc_vm_hwm_bytes(process.pid)
        if sample is not None:
            peak_rss_bytes = max(peak_rss_bytes, sample)
        return_code = process.poll()
        if return_code is not None:
            break
        time.sleep(0.001)
    if return_code != 0:
        raise subprocess.CalledProcessError(return_code, command)
    return peak_rss_bytes / 1_000_000 if peak_rss_bytes else None


def launch_sample(binary, time_executable=None):
    """Return direct launch milliseconds and executable-only peak RSS in MB."""
    command = [str(binary), "--version"]
    start = time.perf_counter_ns()
    subprocess.run(
        command,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=True,
    )
    elapsed_ms = (time.perf_counter_ns() - start) / 1_000_000

    if time_executable:
        return elapsed_ms, peak_rss_with_gnu_time(binary, time_executable)
    if sys.platform.startswith("linux") and Path("/proc/self/status").is_file():
        return elapsed_ms, peak_rss_from_proc(binary)
    return elapsed_ms, None


class PortalAssetParser(HTMLParser):
    def __init__(self):
        super().__init__()
        self.assets = []
        self.has_entry = False

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag == "script" and attrs.get("type", "").lower() == "module" and attrs.get("src"):
            self.has_entry = True
            self.assets.append(attrs["src"])
        elif tag == "link" and "modulepreload" in attrs.get("rel", "").lower().split() and attrs.get("href"):
            self.assets.append(attrs["href"])


def portal_initial_js_gzip_bytes(dist):
    """Sum separately gzipped entry and modulepreload assets; return None if unbuilt."""
    try:
        html = (dist / "index.html").read_text(encoding="utf-8")
    except FileNotFoundError:
        return None
    parser = PortalAssetParser()
    parser.feed(html)
    if not parser.has_entry:
        raise ValueError("portal index.html references no module entry script")
    base = dist.resolve()
    assets = set()
    for reference in parser.assets:
        url = urlsplit(reference)
        if url.scheme or url.netloc:
            raise ValueError(f"external portal JavaScript cannot be measured: {reference}")
        path = (base / unquote(url.path).lstrip("/")).resolve()
        if base not in path.parents:
            raise ValueError(f"invalid portal asset path: {reference}")
        assets.add(path)
    return sum(len(gzip.compress(path.read_bytes(), mtime=0)) for path in assets)


def read_criterion_estimates(target_dir):
    results = []
    estimates = sorted((target_dir / "criterion").glob("*/new/estimates.json"))
    for estimates_path in estimates:
        try:
            data = json.loads(estimates_path.read_text(encoding="utf-8"))
            estimate_ns = float(data["median"]["point_estimate"])
        except (OSError, KeyError, TypeError, ValueError):
            continue
        if not math.isfinite(estimate_ns) or estimate_ns < 0:
            continue
        results.append((estimates_path.parents[1].name, estimate_ns))
    return results


def format_duration_ns(duration_ns):
    if duration_ns < 1_000:
        return f"{duration_ns:.2f} ns"
    if duration_ns < 1_000_000:
        return f"{duration_ns / 1_000:.2f} us"
    return f"{duration_ns / 1_000_000:.2f} ms"


def budget_status(value, budget):
    if value is None:
        return "NOT MEASURED"
    return "OVER BUDGET" if value >= budget else "WITHIN BUDGET"


def render_report(cold_start_ms, idle_rss_mb, binary_size_mb, samples, benchmarks=(), portal_js_bytes=None):
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
        (
            f"| Portal initial JavaScript | {portal_js_bytes / 1_000:.2f} KB gzip | < 150 KB gzip | {budget_status(portal_js_bytes / 1_000, PORTAL_JS_BUDGET_KB)} |"
            if portal_js_bytes is not None
            else "| Portal initial JavaScript | Not available (portal build not found) | < 150 KB gzip | NOT MEASURED |"
        ),
        "| p95 non-ComfyUI API latency | Not measured (API latency benchmark not implemented) | < 20 ms | NOT MEASURED |",
        "",
        "Cold start uses `genaimage --version`; RSS is the executable's peak, not idle server memory. Over-budget measurements are informational and do not fail this run.",
        "",
        "## Benchmarks",
        "",
        "| Criterion benchmark | Median estimate |",
        "| --- | ---: |",
    ]
    if benchmarks:
        rows.extend(f"| {name} | {format_duration_ns(estimate_ns)} |" for name, estimate_ns in benchmarks)
    else:
        rows.append("| No Criterion estimates found; run `cargo bench` first | Not measured |")
    return "\n".join(rows)


def budget_warnings(cold_start_ms, idle_rss_mb, binary_size_mb, portal_js_bytes):
    checks = [
        ("Cold start", cold_start_ms, COLD_START_BUDGET_MS, "ms"),
        ("Idle RSS approximation", idle_rss_mb, IDLE_RSS_BUDGET_MB, "MB"),
        ("Release binary size", binary_size_mb, BINARY_SIZE_BUDGET_MB, "MB"),
        (
            "Portal initial JavaScript",
            None if portal_js_bytes is None else portal_js_bytes / 1_000,
            PORTAL_JS_BUDGET_KB,
            "KB gzip",
        ),
    ]
    return [
        f"::warning title=Performance budget exceeded::{name} is {value:.2f} {unit}, over the {budget:g} {unit} budget"
        for name, value, budget, unit in checks
        if budget_status(value, budget) == "OVER BUDGET"
    ]


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
        time_executable = find_gnu_time()
        measurements = [launch_sample(binary, time_executable) for _ in range(samples)]
        cold_start_ms = statistics.median(sample[0] for sample in measurements)
        rss_samples = [sample[1] for sample in measurements if sample[1] is not None]
        idle_rss_mb = statistics.median(rss_samples) if rss_samples else None
        binary_size_mb = binary.stat().st_size / 1_000_000
        benchmarks = read_criterion_estimates(cargo_target_dir())
        portal_js_bytes = portal_initial_js_gzip_bytes(ROOT / "apps/portal/dist")
        report = render_report(cold_start_ms, idle_rss_mb, binary_size_mb, samples, benchmarks, portal_js_bytes)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"Performance measurement failed: {error}", file=sys.stderr)
        return 1

    print(report)
    for warning in budget_warnings(cold_start_ms, idle_rss_mb, binary_size_mb, portal_js_bytes):
        print(warning)
    summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary_path:
        append_summary(report, Path(summary_path))
    return 0


if __name__ == "__main__":
    sys.exit(main())
