# Performance budgets

These budgets are targets for keeping GenAImage a thin, fast local harness;
they are not claims about the current implementation. The measurements below
are informational. Threshold enforcement is deliberately deferred to [issue
#77](https://github.com/eemaldi/genaimage/issues/77).

| Measure | Budget | What it measures | Measurement method and current status |
| --- | ---: | --- | --- |
| Cold start | < 150 ms | Time for the `genaimage` process to start and handle a cheap command. | `scripts/perf_check.py` directly runs `genaimage --version` repeatedly and reports the median wall-clock time. This is a process-start proxy; it does not clear the operating-system file cache and is not server startup. |
| Idle RSS | < 40 MB | Resident memory of an otherwise idle GenAImage process. | On Linux, GNU `/usr/bin/time -f %M` measures the executable child's peak RSS; if unavailable, the script polls `/proc/<pid>/status` for that process's `VmHWM`. The Python parent is not included. This is still only a short-lived CLI approximation, not a true idle-server measurement. |
| Release binary size | < 25 MB | Size of the release `genaimage` executable. | File size in decimal megabytes (bytes divided by 1,000,000), measured from the release executable produced by Cargo. |
| Portal initial JavaScript | < 150 KB gzip | Compressed JavaScript required for the portal's initial load. | When `apps/portal/dist/index.html` exists, sum the independently gzipped sizes of its local module scripts and modulepreloads, counting each asset once. Build the portal first with `npm ci && npm run build` in `apps/portal`. |
| p95 non-ComfyUI API latency | < 20 ms | 95th-percentile latency for GenAImage API requests that do not call ComfyUI. | Not measured yet: an API latency benchmark has not been implemented. |

The script uses the median of 5 launches in quick mode and 15 otherwise. On Linux it obtains each executable's peak RSS from GNU `time` or polls that
child's `/proc` status; neither method uses the Python parent's RSS. If neither
measurement method is available, RSS is reported as `NOT MEASURED`. Results are
compared with the listed budgets, but over-budget results do not fail the script
or the CI workflow. A missing portal build and unavailable API measurements
are reported as
`NOT MEASURED`, not treated as passing results. Broken or external JavaScript
references fail the measurement rather than producing an incomplete total.
Sizes use decimal kilobytes (1 KB = 1,000 bytes), with one gzip stream per file;
dynamically loaded chunks not referenced from the initial HTML are excluded.

For server cold start, time a fresh release process from launch to the first
successful `/api/v1/health` response with ComfyUI stopped. This is separate from
the CLI proxy and need not clear the operating-system file cache. To measure
idle server RSS on Linux, read `VmRSS` in `/proc/<pid>/status` after that
response with no requests in flight.

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
After the benchmark, the report reads Criterion's median point estimates from
`target/criterion/*/new/estimates.json` and includes a separate `Benchmarks`
table in stdout and the GitHub Actions job summary.

`cargo test --workspace --all-targets` intentionally compiles and smoke-runs the
Criterion harness as regression coverage. The separate quick performance job is
still non-blocking, and budget enforcement belongs in #77 once the relevant
server and portal exist.
