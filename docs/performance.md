# Performance budgets

These budgets are targets for keeping GenAImage a thin, fast local harness;
they are not claims about the current implementation. The measurements below
are informational. Threshold enforcement is deliberately deferred to [issue
#77](https://github.com/eemaldi/genaimage/issues/77).

| Measure | Budget | What it measures | Measurement method and current status |
| --- | ---: | --- | --- |
| Cold start | < 150 ms | Time for the `genaimage` process to start and handle a cheap command. | `scripts/perf_check.py` directly runs `genaimage --version` repeatedly and reports the median wall-clock time. This is a process-start proxy; it does not clear the operating-system file cache and is not server startup. |
| Idle RSS | < 40 MB | Resident memory of an otherwise idle GenAImage process. | On Linux, GNU `/usr/bin/time -f %M` measures the executable child's peak RSS; if unavailable, the script polls `/proc/<pid>/status` for that process's `VmHWM`. The Python parent is not included. This is still only a short-lived CLI approximation, not a true idle-server measurement. |
| Release binary size | < 25 MB | Size of the release `genaimage` executable. | File size in decimal megabytes (bytes divided by 1,000,000), measured from the default release executable, including the portal and its compressed variants. |
| Portal initial JavaScript | < 150 KB gzip | Compressed JavaScript required for the portal's initial load. | When `apps/portal/dist/index.html` exists, sum the independently gzipped sizes of its local module scripts and modulepreloads, counting each asset once. Build the portal first with `npm ci && npm run build` in `apps/portal`. |
| p95 non-ComfyUI API latency | < 20 ms | 95th-percentile latency for GenAImage API requests that do not call ComfyUI. | Not measured yet: an API latency benchmark has not been implemented. |

The script uses the median of 5 launches in quick mode and 15 otherwise. On Linux it obtains each executable's peak RSS from GNU `time` or polls that
child's `/proc` status; neither method uses the Python parent's RSS. If neither
measurement method is available, RSS is reported as `NOT MEASURED`. Results are
compared with the listed budgets, but over-budget results do not fail the script
or the CI workflow. The default release build requires the portal. With
`--no-build`, a missing portal build is reported as `NOT MEASURED`, as are
unavailable API measurements. Broken or external JavaScript
references fail the measurement rather than producing an incomplete total.
Sizes use decimal kilobytes (1 KB = 1,000 bytes), with one gzip stream per file;
dynamically loaded chunks not referenced from the initial HTML are excluded.

## Binary size

The release profile sets `codegen-units = 1` alongside `strip = "symbols"`.
CI and the `Dockerfile` use the default release profile, so they share it.
Measurements on a 12-core Linux machine (decimal MB; the same source tree, the
`genaimage` binary with the embedded portal; `--version` cold start is the median
of 30 launches; latencies are p95 over 300 loopback requests to `genaimage serve`
with ComfyUI absent; idle server RSS is measured after the requests):

| Profile | Size (bytes) | Size | Build | Cold start | `/api/v1/health` p95 | `/api/v1/workflows` p95 | Server RSS |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Previous (`strip` only) | 25,480,344 | 25.48 MB | 56 s | 0.62 ms | 0.97 ms | 0.10 ms | 25.3 MB |
| `lto = "thin"` | 25,793,736 | 25.79 MB | 60 s | 0.67 ms | 1.22 ms | 0.22 ms | 26.6 MB |
| `lto = "fat"` | 22,687,080 | 22.69 MB | 120 s | 0.63 ms | 1.23 ms | 0.19 ms | 26.2 MB |
| `codegen-units = 1` (chosen) | 21,012,024 | 21.01 MB | 92 s | 0.64 ms | 1.01 ms | 0.14 ms | 23.0 MB |
| `lto = "thin"` + `codegen-units = 1` | 21,734,088 | 21.73 MB | 96 s | 0.62 ms | 0.96 ms | 0.14 ms | 23.3 MB |
| `lto = "fat"` + `codegen-units = 1` | 20,847,464 | 20.85 MB | 132 s | 0.63 ms | 1.18 ms | 0.20 ms | 24.3 MB |
| `codegen-units = 1` + `opt-level = "s"` | 16,462,936 | 16.46 MB | 51 s | 0.71 ms | 1.24 ms | 0.22 ms | 21.4 MB |

Latency differences of a few tenths of a millisecond are within run-to-run noise.
Builds were from a cold target directory.

`codegen-units = 1` removes about 4.5 MB without a build-time cost that matters
in CI (about 90 s) and gives about 4 MB of headroom for new crates. Fat LTO adds
only 0.16 MB on top at the price of 40 s more build time, so it is not used.

Rejected options:

- `panic = "abort"`: the batch driver (`orchestrator/batches.rs`) and the
  assistant service (`assistant/service.rs`) use `catch_unwind` to turn a
  panicking provider or tool into a recorded `outcome_unknown` or failure.
  Aborting would kill the server and lose that guard.
- `opt-level = "s"`: much smaller (16.5 MB) and API latency stays sub-millisecond,
  but it also slows SQLite (compiled by `cc` at the same level) and image
  decoding. It is the reserve lever if the budget is threatened again; measure
  gallery thumbnailing before adopting it.

The embedded portal is about 0.94 MB (raw 0.56 MB, plus `.br` and `.gz`
variants of 0.38 MB). Dropping variants would change what clients receive for
little gain, so it is unchanged. Duplicate crate versions found by `cargo tree -d`
(for example `tungstenite`, `miniz_oxide` and `digest`) come from transitive
dependencies and were not pursued, because the profile change alone met the goal.

`scripts/perf_check.py` prints a GitHub `::warning::` annotation for every
over-budget measurement, so a regression is visible on the pull request even
though the job stays informational.

For server cold start, time a fresh release process from launch to the first
successful `/api/v1/health` response with ComfyUI stopped. This is separate from
the CLI proxy and need not clear the operating-system file cache. To measure
idle server RSS on Linux, read `VmRSS` in `/proc/<pid>/status` after that
response with no requests in flight.

Run the focused Criterion benchmarks with:

```sh
cargo bench --locked -p genaimage-core --bench performance -- --quick
```

Build the portal before running the release measurements (CI does the same).
The core-only Criterion benchmark does not need the portal:

```sh
(cd apps/portal && npm ci && npm run build)
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
still non-blocking, and budget enforcement belongs in #77.

## Gallery scroll regression

```sh
(cd apps/portal && npm test -- src/routes/gallery/Gallery.test.ts)
```

The Testing Library regression mounts the production grid in jsdom with 5,000
synthetic outputs and a fixed 900 × 540 px viewport. It scrolls from the first
row through the middle and final rows, then back, checking that the expected
output is present and that no sample renders more than 35 tiles (five columns,
three visible rows and two overscan rows on either side). A separate test
exercises cursor pagination. Explicit tile geometry reserves space for the
thumbnail before it loads. This check measures DOM size, not browser frame rate
or layout shift; profile a production browser with representative local media
for those measurements. The gallery chunk is loaded only on the gallery route.

A production-build browser smoke check served 50 fixture pages of 100 GIF
metadata records over loopback. Repeatedly scrolling to the end of the loaded
list reached item 5,000 and sampled at most 18 tiles in a 519 × 301 CSS px grid
viewport. GIFs exercise the placeholder path, so this does not measure image
decoding cost or establish a frame-rate target.
