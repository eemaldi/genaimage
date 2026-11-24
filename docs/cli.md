# GenAImage CLI reference

Start the loopback API with `genaimage serve` before using the server-backed
commands below. ComfyUI is a separate process. Only `serve` and `config init`
operate locally without making API requests.

## Global options

| Option | Meaning |
| --- | --- |
| `--server <URL>` | Loopback HTTP(S) origin for API calls, without a path. Takes precedence over `GENAIMAGE_SERVER`, which defaults to `http://127.0.0.1:3000` when unset. Does not change the listener used by `serve`. |
| `--json` | Emit machine-readable JSON rather than human-readable output. |
| `--workspace <PATH>` | Select the workspace for local commands and `serve`; defaults to the current directory. |
| `--config <PATH>` | Select the configuration file for `serve`, or the output path for `config init` (see [configuration discovery](setup.md)). |
| `-h`, `--help` | Show help for the CLI or the selected command. |
| `-V`, `--version` | Show the CLI version (at the top level). |

Server-backed commands use the server's workspace and configuration, not the
CLI's local `--workspace`, `--config`, or `GENAIMAGE_CONFIG` values.

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

For the server-backed commands below, `--json` writes one compact JSON value
and a newline to stdout for a completed command, including a negative check
(`6`) or a failed or cancelled run observed by a waiter (`7`). Command errors
instead write one JSON object and a newline to
stderr, leaving stdout empty:

```json
{"kind":"server_not_running","message":"Cannot connect to the GenAImage server at http://127.0.0.1:3000/. Start it with `genaimage serve`. If it uses a different address, set --server or GENAIMAGE_SERVER."}
```

The stable `kind` values are `server_not_running`, `server_problem`,
`client_error`, `input_error`, `output_error`, `failed_submission`, and
`unknown_submission`. Server problem details, when available, are included in
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

Both local commands write `{status: "error", error: "<message>"}` to stderr
for local failures in JSON mode.

## Server-backed commands

JSON field definitions follow the [OpenAPI contract](api/openapi.json).

- **`doctor`** checks the server and ComfyUI. JSON is a `HealthResponse`:
  `{self, image_backend, llm}` plus `image_backend_reason` when present.
  Status strings are `ok`, `unavailable`, or `not_configured`; the optional
  reason is `probe_failed` or `timeout`. Exit `6` means the server or image
  backend did not report `ok`; an unconfigured LLM does not fail the check.
- **`workflow list`** lists catalogue entries and non-fatal discovery
  diagnostics. JSON is `{workflows, diagnostics}`. Each workflow has `id`,
  `name`, `version`, `description`, `tags`, `category` and `thumbnail_url`;
  the descriptive fields can be null. Each diagnostic has `path` and `error`.
- **`workflow inspect <ID>`** shows a catalogue workflow, its declared
  bindings and outputs. JSON is `WorkflowDetails`: `{schema_version, id,
  version, title, workflow, description, tags, category, thumbnail, parameters,
  presets, outputs}`. `parameters` maps each name to its binding, constraints
  and UI hints; `outputs` is an array of `{node, kind}` objects. This is not a
  resolved-run preview. Use `--json` to see the full constraints and metadata.
- **`workflow validate <ID> [--preset <NAME>] [--set NAME=JSON]…`** checks
  bindings and the availability of required ComfyUI node classes without
  submitting a run. Explicit `--set` values override preset values; parameters not supplied
  by either fall back to manifest defaults where declared. Values parse as JSON first,
  otherwise as strings. JSON is `{valid, parameter_errors, node_check}`;
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

### Runs and jobs

All run records below use `RunResponse`: `{id, prompt_id, workflow_id,
workflow_version, workflow_sha256, status, parameters, error, created_at,
updated_at, completed_at}`, with optional `node_warnings`. `parameters`
contains the effective values; timestamps are RFC 3339 and `error` and
`completed_at` may be null. Statuses are `created`, `submitting`,
`unknown_submission`, `queued`, `running`, `completed`, `failed`, `cancelling`,
and `cancelled`.

- **`run <ID> [--preset <NAME>] [--set NAME=JSON]… [--image NAME=PATH]… [--version <VERSION>] [--wait]`**
  submits the server catalogue workflow once and prints its run record. Explicit
  bindings override the selected preset; the server applies declared defaults.
  `--set` parses JSON first, otherwise keeps the literal string. Use `--image`
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
- **`jobs wait <RUN_ID>`** waits for an existing run without submitting or
  reconciling it. Like `run --wait`, it writes human progress to stderr and
  only the final run record to stdout. JSON mode suppresses progress and
  emits exactly one final record for `completed`, `failed`, or `cancelled`.
  An unconfirmed run stops the wait with exit `8`. Both waits check the
  run's current state if SSE is interrupted and poll while it remains active.
  Interrupting the CLI does not cancel the server's run.
- **`jobs cancel <RUN_ID>`** requests cancellation and returns the current
  run record, which may still be `cancelling`. ComfyUI uses a global interrupt
  for running jobs; if the target finishes first, another prompt may be
  interrupted. A successful request is not confirmation of cancellation.
- **`jobs reconcile [<RUN_ID>]`** asks the server to check ComfyUI queue and
  history. With an ID, JSON is the updated run record; without one, it is
  `{runs}` for the active runs examined. This is not a resubmission. A successful
  reconciliation request can still return a non-successful run status.
