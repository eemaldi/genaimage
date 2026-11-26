# GenAImage CLI reference

Start the loopback API with `genaimage serve` before using the server-backed
commands below. ComfyUI is a separate process. `serve`, `config init` and
`completions` operate locally without making API requests.

## Global options

| Option | Meaning |
| --- | --- |
| `--server <URL>` | Loopback HTTP(S) origin for API calls, without a path. Takes precedence over `GENAIMAGE_SERVER`. When neither is set, the CLI uses `http://127.0.0.1:$GENAIMAGE_SERVER_PORT` if that is a valid port, otherwise `http://127.0.0.1:3000`; the CLI ignores an invalid `GENAIMAGE_SERVER_PORT`, whereas `serve` rejects it. Does not change the listener used by `serve`. |
| `--json` | Emit machine-readable JSON rather than human-readable output. |
| `--workspace <PATH>` | Select the workspace for `serve`; defaults to the current directory. |
| `--config <PATH>` | Select the configuration file for `serve`, or the output path for `config init` (see [configuration discovery](setup.md)). |
| `-h`, `--help` | Show help for the CLI or the selected command. |
| `-V`, `--version` | Show the CLI version (at the top level). |

Server-backed commands reject `--workspace`, `--config`, and a set
`GENAIMAGE_CONFIG` (even if empty) with exit code `1`. Pass local options or
set `GENAIMAGE_CONFIG` when running `genaimage serve`, then unset the variable
for client commands and select that instance with `--server`. The retired
`GENAIMAGE_PROFILE` is also rejected; unset it and migrate the old profile to
`[comfyui]` in the server configuration.

## Exit codes and output

| Code | Meaning |
| --- | --- |
| `0` | Success. |
| `1` | Local failure, such as malformed parameter overrides, an output error or a local command failure. |
| `2` | Command-line usage error (clap). |
| `3` | Connection refused: start `genaimage serve` or check `--server` / `GENAIMAGE_SERVER`. |
| `4` | The server returned problem details. |
| `5` | Another API client failure, such as an invalid URL, transport failure, or unexpected response. |
| `6` | A completed health or validation check returned a negative result. |
| `7` | A submission failed, or the run being waited on failed or was cancelled. |
| `8` | The submission outcome is unconfirmed. Reconcile before considering another submission. |
| `9` | The observation deadline or a run-status request deadline expired. |
| `10` | A model pull failed or was cancelled. |

For the server-backed commands below, `--json` writes one compact JSON value
and a newline to stdout for a completed command, including a negative check
(`6`) or a failed or cancelled run observed by a waiter (`7`). Command errors
instead write one JSON object and a newline to
stderr, leaving stdout empty:

```json
{"kind":"server_not_running","message":"Cannot connect to the GenAImage server at http://127.0.0.1:3000/. Start it with `genaimage serve`. If it uses a different address, set --server or GENAIMAGE_SERVER."}
```

The stable `kind` values are `server_not_running`, `server_problem`,
`client_error`, `input_error`, `output_error`, `file_error`, `browser_error`,
`failed_submission`, `unknown_submission`, `wait_timeout`, `model_pull_failed`
and `model_pull_unknown`. Server problem
details, when available, are included in
`problem` with `type`, `title`, `status`, `detail`, `recovery`, and optional
`errors` (field-specific `{key, reason}` objects). Submission errors include
`run` when the server returned a durable run record. Errors during observation
of a known run include `run_id`. Use `kind` and the exit code for branching
rather than matching `message`.
Without `--json`, errors are human-readable on stderr. Clap still renders
usage errors, help and version text normally, even with `--json`.

## Local commands

- **`serve`** starts the local HTTP server using the selected workspace and
  configuration. It runs until interrupted, writes structured server logs to
  stderr regardless of `--json`, and returns no JSON result on stdout.
- **`config init [--force]`** writes an example configuration to `--config`
  or `./genaimage.toml`. Use `--force` only to replace an existing file.
  JSON is `{path, created}`, with `created: true` on success.

`serve` and `config init` write `{status: "error", error: "<message>"}` to
stderr for local failures in JSON mode.

### Shell completions

`genaimage completions <shell>` prints a completion script without starting the
server or reading configuration. Supported shell names are `bash`, `zsh`,
`fish`, `powershell` and `elvish`. The commands below overwrite the same
completion file on repeat runs; profile setup lines are guarded or added once.

- **Bash** — save the completion file and add one guarded source line to
  `~/.bashrc`:
  ```sh
  mkdir -p "$HOME/.bash_completion.d"
  genaimage completions bash > "$HOME/.bash_completion.d/genaimage"
  touch "$HOME/.bashrc"
  source_line='[ -r "$HOME/.bash_completion.d/genaimage" ] && . "$HOME/.bash_completion.d/genaimage"'
  grep -qxF "$source_line" "$HOME/.bashrc" || printf '%s\n' "$source_line" >> "$HOME/.bashrc"
  ```
  For the current session only: `eval "$(genaimage completions bash)"`.
- **Zsh** — save the completion file:
  ```sh
  mkdir -p "$HOME/.zfunc"
  genaimage completions zsh > "$HOME/.zfunc/_genaimage"
  ```
  In `~/.zshrc`, ensure `fpath=("$HOME/.zfunc" $fpath)` appears once before
  your existing `compinit` initialisation. Add `autoload -Uz compinit && compinit`
  once if you do not already initialise Zsh completions.
- **Fish** — save the script in Fish's automatically loaded completions directory:
  ```fish
  mkdir -p "$HOME/.config/fish/completions"
  genaimage completions fish > "$HOME/.config/fish/completions/genaimage.fish"
  ```
- **PowerShell** — keep the generated script separate from `$PROFILE` (it starts
  with `using namespace`), then dot-source it from the profile only once:
  ```powershell
  New-Item -ItemType Directory -Force (Split-Path $PROFILE) | Out-Null
  $completionFile = Join-Path $HOME 'genaimage-completions.ps1'
  genaimage completions powershell > $completionFile
  if (-not (Test-Path -LiteralPath $PROFILE)) {
      New-Item -ItemType File -Path $PROFILE | Out-Null
  }
  $sourceLine = '. "$HOME\genaimage-completions.ps1"'
  if (-not (Select-String -Path $PROFILE -SimpleMatch $sourceLine -Quiet)) {
      Add-Content -Path $PROFILE -Value $sourceLine
  }
  ```
- **Elvish** — save the script in the library directory and add `use genaimage`
  to `~/.config/elvish/rc.elv` once:
  ```sh
  mkdir -p "$HOME/.config/elvish/lib"
  genaimage completions elvish > "$HOME/.config/elvish/lib/genaimage.elv"
  touch "$HOME/.config/elvish/rc.elv"
  grep -qxF 'use genaimage' "$HOME/.config/elvish/rc.elv" || printf '%s\n' 'use genaimage' >> "$HOME/.config/elvish/rc.elv"
  ```

## Local server

`genaimage serve [--portal-dev-url <url>]` starts the loopback API and serves
its embedded portal at `/` by default. The optional flag replaces the embedded
assets with an HTTP proxy to Vite, even in a build without embedded assets.
Use a loopback HTTP origin such as `http://127.0.0.1:5173`, with no credentials,
path, query or fragment. Use a separate port from GenAImage. API requests stay
on GenAImage. WebSocket upgrades are not proxied; refresh manually or use
[Vite directly for HMR](../README.md#portal-development).

## Server-backed commands

JSON field definitions follow the [OpenAPI contract](api/openapi.json).

- **`open [--print]`** checks the server health endpoint, then opens the configured
  loopback server root (the portal in default builds) in the default browser.
  `--print` writes only that URL instead of opening a browser; with `--json`,
  either mode emits `{"url": "..."}`. Launchers receive the validated URL as one argument, without a shell, and their stdin,
  stdout and stderr are redirected. On Linux and other Unix systems GenAImage
  invokes `xdg-open` and leaves standard `BROWSER` handling (including command
  lists and `%s` templates) to it; macOS uses `open`, and Windows uses
  `rundll32`. GenAImage waits up to 500 ms for the launcher; a non-zero exit is
  a local `browser_error` (exit `1`) with a `--print` hint, while a launcher still
  running after that is assumed to have opened the URL. The server must be
  running (otherwise exit `3`); non-loopback URLs are rejected (exit `5`).
- **`doctor`** runs the server's diagnostics (`GET /api/v1/doctor`). Human
  output has one line per check (`[ok]`, `[warning]`, `[ERROR]` or `[skipped]`),
  followed by a `Fix:` line for everything that is not `ok`. JSON is a
  `DoctorReport`: `{"checks": [{id, status, summary, items, fix}]}`. `status` is
  `ok`, `warning`, `error` or `skipped`; `items` is a bounded list of detail
  lines; `fix` is a string for every check except `ok` (where it is `null`).
  The check ids are `server`, `config`, `comfyui.reachable`, `comfyui.version`,
  `comfyui.object_info`, `workflows.nodes`, `llm.reachable`, `llm.text`,
  `llm.vision`, `gpu`, `disk` and `ports`; see
  [troubleshooting](troubleshooting.md#doctor-checks). Every id is always
  present. Exit `6` means at least one check is an `error`, including a server that
  accepts connections but does not answer within 20 s; warnings and skipped
  checks (such as an LLM that is not configured) exit `0`. If the server is
  not running, `doctor` still prints a report on stdout, with a `server` error,
  a `ports` check and every other check skipped, and exits `3`. The `ports` check
  claims the port is free only when the connection was refused; otherwise its
  state is reported as unknown. If another
  program is listening on the server port, the `server` and `ports` checks are
  errors and the exit code is `6`. This replaces the earlier `HealthResponse`
  JSON (`self`, `image_backend`, `llm`); use the `GET /api/v1/health` endpoint
  for that shape.
- **`workflow list`** lists catalogue entries and non-fatal discovery
  diagnostics. JSON is `{workflows, diagnostics}`. Each workflow has `id`,
  `name`, `version`, `revision`, `description`, `tags`, `category` and `thumbnail_url`;
  the descriptive fields can be null. Each diagnostic has `path` and `error`.
- **`workflow inspect <ID>`** shows a catalogue workflow, its declared
  bindings and outputs. JSON is `WorkflowDetails`: `{schema_version, id,
  version, revision, title, workflow, description, tags, category, thumbnail, parameters,
  presets, outputs}`. `parameters` maps each name to its binding, constraints
  and UI hints; `outputs` is an array of `{node, kind}` objects. This is not a
  resolved-run preview. Use `--json` to see the full constraints and metadata.
- **`workflow validate <ID> [--preset <NAME>] [--set NAME=JSON]…`** checks
  bindings and the availability of required ComfyUI node classes without
  submitting a run. Explicit `--set` values override preset values; parameters not supplied
  by either fall back to manifest defaults where declared. Values must be valid
  JSON and each name can appear only once. JSON is `{revision, valid, parameter_errors, node_check}`;
  parameter errors contain `parameter` and `reason`, and `node_check` contains
  `status` (`available`, `missing` or `unavailable`), `missing_classes` (an
  array), and `reason` (a nullable string). `valid: false` exits with `6`.
- **`presets <ID>`** displays the declared presets. JSON maps preset names
  to objects of parameter names and JSON values, for example
  `{"fast":{"seed":42,"steps":10}}`. An empty map means no presets.
- **`config show`** displays settings as resolved by the running server for
  the next start, including already-applied values. JSON is `{settings,
  config_file, restart_required}`. `settings` maps dotted keys to objects with
  `value`, `source` (`default`, `file` or `env`), `read_only`, and optional
  `active_value`. A `file` source also has `file`; an `env` source has
  `variable`. `restart_required` lists keys with saved and active differences.

`inspect`, `validate` and `presets` take catalogue IDs, not local or server
filesystem paths. To validate a local workflow, install its sidecar and graph
in the server's configured workflow directory and use the ID from
`workflow list`. Resolve duplicate IDs or broken sidecars before validating.

### Models

Nothing here downloads unless you run `models pull`. See [models](models.md)
for where checkpoints go and which models to start with.

- **`models list`** reads installed LLM models and ComfyUI checkpoints from the
  server. `--json` prints `{llm, llm_error, checkpoints, checkpoints_error}`.
  `llm` is `{source, pull_supported, pull_unsupported_reason, models, roles}`:
  `source` is `ollama` or `openai`, each model has `name`, `size_bytes`
  (exact, nullable), `digest` and `modified_at`, and each of the `text` and
  `vision` `roles` has `model` and `state` (`present`, `missing` or
  `not_configured`; the name must match exactly). `checkpoints` is
  `{checkpoints, locations, refresh_note}`. A section that is unavailable, for
  example because ComfyUI is down, is `null` with its message in `*_error`, and
  the command exits `4`; it never reports an empty list as success.
- **`models pull <NAME>`** starts one explicit download through Ollama and
  follows it. In human mode a progress line with sizes and a percentage is
  written to stderr. The total grows as Ollama announces layers. With `--json`
  stdout carries one final status object only:
  `{id, name, state, status, layer, total_bytes, completed_bytes, error,
  started_at, finished_at, sequence}`, where `state` is `succeeded`, `failed`
  or `cancelled`. A failure exits `10` with that object on stdout and a
  `model_pull_failed` error on stderr. A name that is invalid (`422`), already
  being pulled (`409`), or on an endpoint without Ollama's pull API (`501`)
  exits `4` with the problem details. Interrupting the CLI does not cancel the
  server's pull; the server keeps it running and the next `models pull` of the
  same name reports a `409` naming it. If the server restarts mid-pull the
  outcome is unknown (`model_pull_unknown`, exit `5`); run `models list`.

### Runs and jobs

All run records below use `RunResponse`: `{id, prompt_id, workflow_id,
workflow_version, workflow_sha256, status, parameters, error, created_at,
updated_at, completed_at}`, with optional `node_warnings`. `parameters`
contains the effective values; timestamps are RFC 3339 and `error` and
`completed_at` may be null. Statuses are `created`, `submitting`,
`unknown_submission`, `queued`, `running`, `completed`, `failed`, `cancelling`,
and `cancelled`.

- **`run <ID> [--preset <NAME>] [--set NAME=JSON]… [--image NAME=PATH]… [--version <VERSION>] [--wait [--timeout <SECONDS>]]`**
  submits the server catalogue workflow once and prints its run record. Explicit
  bindings override the selected preset; the server applies declared defaults.
  The CLI does not pin the workflow revision: it uses the snapshot current at
  submission time.
  `--set` requires valid JSON and rejects duplicate names before uploading or
  submitting. For a string, use shell quoting such as `--set 'prompt="a portrait"'`.
  Use `--image`
  to upload an explicitly selected local file for an `image_upload` parameter,
  or use `--set` with an existing upload ID. A path passed to `--set` is not
  uploaded. Do not bind the same image parameter through both options.
  `--version` selects the output version without overwriting existing artefacts.
  Without `--wait`, success means submission was accepted, not that generation
  completed. An unconfirmed or failed submission is never retried automatically;
  inspect `jobs list` or `jobs show` and use `jobs reconcile`.
- **`jobs list [--limit <N>] [--cursor <TOKEN>]`** lists runs, newest first.
  Default limit 50, maximum 200. JSON is `{runs, next_cursor}`, with a null
  cursor on the last page. Pass the cursor unchanged for the next page.
- **`jobs show <RUN_ID> [--events <N>] [--output-cursor <TOKEN>]`** shows the
  run, its latest persisted events (default 100, maximum 200), and a page of
  output metadata. JSON is `{run, events, outputs, next_output_cursor}`;
  `events` contains `RunEventResponse` objects in ascending event ID order,
  `outputs` contains `OutputResponse` objects, and a null cursor means there
  are no more output pages.
- **`jobs wait <RUN_ID> [--timeout <SECONDS>]`** waits for an existing run without submitting or
  reconciling it. Like `run --wait`, it writes human progress to stderr and
  only the final run record to stdout. JSON mode suppresses progress and
  emits exactly one final record for `completed`, `failed`, or `cancelled`.
  An unconfirmed run stops the wait with exit `8`. Both waits check the
  run's current state if SSE is interrupted and poll while it remains active.
  Each status request has a 5-second deadline. Both waits have a 600-second
  overall observation deadline by default; `--timeout` accepts 1–86,400 seconds.
  For `run --wait`, that budget starts after submission. A timeout exits with
  `9` and reports the run ID and a `jobs reconcile` hint; it never resubmits,
  cancels or reconciles the run. Interrupting the CLI also does not cancel it.
- **`jobs cancel <RUN_ID>`** requests cancellation and returns the current
  run record, which may still be `cancelling`. ComfyUI uses a global interrupt
  for running jobs; if the target finishes first, another prompt may be
  interrupted. A successful request is not confirmation of cancellation.
- **`jobs reconcile [<RUN_ID>]`** asks the server to check ComfyUI queue and
  history. With an ID, JSON is the updated run record plus an `outcome` field;
  without one, `{runs}` contains such a record for each non-terminal run examined.
  Outcomes are `terminal`, `active`, `in_progress_elsewhere`, `not_found` and
  `still_ambiguous`; see [troubleshooting](troubleshooting.md#a-run-is-unknown_submission).
  An optional `settles_at` gives the earliest time to check again.
  This is not a resubmission. A successful reconciliation request can still
  return a non-successful run status.

- **`jobs download <RUN_ID> --dir <DIR>`** downloads original output bytes
  from all output pages. Creates the destination directory if needed. Local
  filenames are numbered (`output-001.png`, for example); the extension is
  taken from the reported filename if safe, otherwise `.bin`. JSON is
  `{run_id, outputs: [{id, path}]}` (an empty array if no outputs are available).
  Existing files are never replaced. If a later download fails, files already
  saved remain; choose a fresh directory to repeat the download.
- **`jobs manifest <RUN_ID> [--out <FILE>]`** prints the archived run's
  provenance as JSON (pretty-printed by default, compact with `--json`). It
  preserves the manifest fields apart from the server filesystem locations
  `workflow.manifest_path`, `outputs[].local_path` and
  `source_inputs[].source_path`. With `--out`, writes the same provenance to
  a new local file and reports `{run_id, path}` in JSON mode. It refuses to
  replace an existing file.
