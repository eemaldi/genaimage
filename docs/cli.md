# GenAImage CLI reference

Start the loopback API with `genaimage serve` before using the server-backed
commands below. ComfyUI is a separate process. `run`, `jobs`, `serve` and
`config init` still use their existing local code paths; the API port of `run`
and `jobs` is not covered here.

## Global options

| Option | Meaning |
| --- | --- |
| `--server <URL>` | Loopback HTTP(S) origin for API calls, without a path. Takes precedence over `GENAIMAGE_SERVER`, which defaults to `http://127.0.0.1:3000` when unset. Does not change the listener used by `serve`. |
| `--json` | Emit machine-readable JSON rather than human-readable output. |
| `--workspace <PATH>` | Select the workspace for local commands and `serve`; defaults to the current directory. |
| `--config <PATH>` | Select the configuration file for local commands and `serve` (see [configuration discovery](setup.md)). |

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

For the server-backed commands below, `--json` writes one compact JSON value
and a newline to stdout for a completed command, including a negative result
(code `6`). Command errors instead write one JSON object and a newline to
stderr, leaving stdout empty:

```json
{"kind":"server_not_running","message":"Cannot connect to the GenAImage server at http://127.0.0.1:3000/. Start it with `genaimage serve`. If it uses a different address, set --server or GENAIMAGE_SERVER."}
```

The stable `kind` values are `server_not_running`, `server_problem`,
`client_error`, `input_error`, and `output_error`. For `server_problem` only,
there is also a `problem` object with `type`, `title`, `status`, `detail`,
`recovery`, and optional `errors` (field-specific `{key, reason}` objects).
Use `kind` and the exit code for branching rather than matching `message`.
Without `--json`, errors are human-readable on stderr. Clap still renders
usage errors, help and version text normally, even with `--json`. Local
commands retain their previous output formats.

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
