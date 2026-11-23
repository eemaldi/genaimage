# Performance budgets

These budgets are targets for keeping GenAImage a thin, fast local harness;
they are not claims about the current implementation. The measurements below
are informational. Threshold enforcement is deliberately deferred to [issue
#77](https://github.com/eemaldi/genaimage/issues/77).

| Measure | Budget | What it measures | Measurement method and current status |
| --- | ---: | --- | --- |
| Cold start | < 150 ms | Time for the `genaimage` process to start and handle a cheap command. | `scripts/perf_check.py` runs `genaimage --version` repeatedly and reports the median wall-clock time. This is a process-start proxy; it does not clear the operating-system file cache and is not server startup. |
| Idle RSS | < 40 MB | Resident memory of an otherwise idle GenAImage process. | The script reports the median peak RSS of the same short-lived `--version` child processes. This is an approximation, not a true idle-server measurement; the server does not exist yet. |
| Release binary size | < 25 MB | Size of the release `genaimage` executable. | File size in decimal megabytes (bytes divided by 1,000,000), measured from the release executable produced by Cargo. |
| Portal initial JavaScript | < 150 KB gzip | Compressed JavaScript required for the portal's initial load. | Not measurable yet: the portal does not exist yet. |
| p95 non-ComfyUI API latency | < 20 ms | 95th-percentile latency for GenAImage API requests that do not call ComfyUI. | Not measurable yet: the server/API does not exist yet. |

The script uses the median of 5 launches in quick mode and 15 otherwise. On Linux
it obtains each child's peak RSS from the operating system's per-child resource
usage. Results are compared with the listed budgets, but over-budget results do
not fail the script or the CI workflow. Missing portal and API measurements are
reported as `NOT MEASURED`, not treated as passing results.

Run the focused Criterion benchmarks with:

```sh
cargo bench --locked -p genaimage-core --bench performance -- --quick
```

Run the release measurements with:

```sh
python3 scripts/perf_check.py --quick
```

The Criterion harness measures workflow binding through the public binding API
and run insertion in SQLite. Its inputs and database live in a temporary
subdirectory of Cargo's target directory and are removed after the benchmark.
The performance CI job runs both commands as an informational, non-blocking
job and appends the measurements to the GitHub Actions job summary. Future
budget enforcement belongs in #77 once the relevant server and portal exist.
