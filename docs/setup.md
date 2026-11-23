# Setup

## 1. ComfyUI

Install and configure ComfyUI separately. Start its server and confirm that the
local HTTP endpoint responds. GenAImage expects HTTP health, object information,
prompt, upload, queue, history and output endpoints, plus WebSocket events.
Export workflows from ComfyUI in **API format**. The harness does not install
models or custom nodes.

## Start the local HTTP server

```sh
genaimage serve
# In a separate terminal:
curl http://127.0.0.1:3000/api/v1/health
```

The default listener is `127.0.0.1:3000`. To change it, set `[server]` `host`
and `port` in the configuration file or use the environment overrides below.
Use a loopback host such as `127.0.0.1`, `::1` or `localhost`; wildcard and
non-loopback addresses are refused. No running ComfyUI is needed for startup:
health reports the image backend as `unavailable` until it responds. Server
logs are structured JSON on stderr; set `RUST_LOG` to control verbosity.
Stop with Ctrl+C (SIGINT) or SIGTERM to let in-flight requests finish.

Health, events, workflows, runs, outputs, uploads and settings are available.
The [OpenAPI 3.1 contract](api/openapi.json) is also served at
`GET /api/v1/openapi.json`. The portal is planned.
Requests must use `127.0.0.1:<port>`, `localhost:<port>`, `[::1]:<port>`,
or the configured loopback bind IP as the Host, with the actual listener port.
Other loopback IPs are not implicitly allowed. On HTTP port 80 the port may be
omitted from Host and Origin. A rejected Host returns 421.
Mutating browser requests require a same-origin `Origin` or
`Sec-Fetch-Site: same-origin`; conflicting or cross-site headers return 403.
Local CLI and curl requests with neither header are allowed unless they carry
browser indicators (other `Sec-Fetch-*` headers or a `Mozilla/` user agent).
This is not authentication: legacy browsers without recognisable headers and
local programs that forge headers cannot be distinguished from CLI clients.
Avoid untrusted local applications and old browsers. There is no permissive CORS.

Request bodies are limited to 1 MiB by default. The uploads API uses
`server.max_upload_bytes` (default 33,554,432 bytes, or 32 MiB), including
multipart framing. Oversized requests return 413. Bodies must arrive within
30 seconds (408 on timeout); outstanding reads are cancelled with 503 when
shutdown begins. Uploads are streamed to disk; submitting a run verifies and
holds one reference image at a time in memory before sending it to ComfyUI.

## 2. Configuration

No configuration file is required. To customise it, copy the example to
`genaimage.toml` or run `genaimage config init`. The CLI looks for one file, in
this order:

1. `--config <path>`;
2. `GENAIMAGE_CONFIG`;
3. `./genaimage.toml` in the current directory;
4. `$XDG_CONFIG_HOME/genaimage/genaimage.toml` (or
   `~/.config/genaimage/genaimage.toml` when unset).

An explicitly selected file must exist. `--workspace` selects the workspace,
not the configuration search directory. Relative data and workflow paths are
resolved against the workspace; relative configuration paths use the current
directory. `genaimage config show` displays the resolved configuration.
If no `--config`, `GENAIMAGE_CONFIG` or current-directory file is selected,
GenAImage checks for a clearly legacy workspace file before XDG discovery. It
asks you to migrate or move that file aside after preserving its settings and
data. It detects only files accepted by the old schema and rejected by the new
schema. An ambiguous paths-only file is not selected automatically: use
`--config <path>` to select it. The check tolerates unreadable or malformed
files; explicit and current-directory selections do not inspect an unselected
workspace file.

The file contains `[server]`, `[comfyui]`, `[llm]` and `[paths]` sections;
see `genaimage.example.toml`. The `serve` command uses `[server]`. LLM
inference is planned. The settings connectivity test can check an explicitly
configured LLM URL, but it does not start a service or select a model.
The server bind host is restricted to loopback regardless of outbound settings.

State defaults to `$XDG_DATA_HOME/genaimage/workspaces/<workspace-hash>/state.sqlite3`,
runs to `$XDG_DATA_HOME/genaimage/workspaces/<workspace-hash>/runs`, and uploads to
`$XDG_DATA_HOME/genaimage/uploads`. If unset, `XDG_DATA_HOME` defaults to
`~/.local/share`. Workflows remain under `workflows/comfyui` in the workspace.
Each canonical workspace gets its own database and runs directory. An empty
database may be rebound; once it contains runs, it is bound atomically to its
workspace and backend endpoint, including when a custom path is used. Changing
either then requires its original settings or a separate database.
The uploads API stores reference images in the configured uploads directory.
The CLI continues to upload explicit local input files directly to ComfyUI.
XDG directories must be absolute.

### Settings API

`GET /api/v1/settings` returns a `settings` map keyed by dotted TOML names.
Each entry includes its effective `value`, `source` (`default`, `file` or
`env`), the source `file` or environment `variable` where applicable, and
`read_only`. Environment overrides and the legacy-state claim flag cannot
be edited through the API. Only explicitly listed, non-secret settings are
returned. `config_file` shows the write target.

Send a sparse JSON object to `PUT /api/v1/settings`, for example
`{"comfyui.base_url":"http://127.0.0.1:8189","server.port":3001}`. Omitted
keys are unchanged. Invalid, unknown or changed read-only keys return 422
with a problem JSON `errors` array containing `key` and `reason` for each
issue. Saves edit the file selected at server startup (following the
precedence above), or create the XDG configuration file if none was selected.
The server preserves untouched TOML and existing comments where possible,
syncs a temporary file in the same directory, then renames it over the target.
It does not fall back to another file if the target is unwritable. If the file
has changed on disk, the save is refused; restart to load the external edits.

GET and PUT list staged changes in `restart_required`. Each affected entry
has the saved `value` and the current `active_value`. All `[server]` and
`[paths]` settings, plus `comfyui.base_url` and `comfyui.allowed_hosts`, take
effect on restart, even if the current database is empty. ComfyUI request and
WebSocket reconnect timings apply live without changing the active endpoint
or its pinned DNS addresses.

On restart, the existing database ownership check still applies. Before
switching endpoints, finish or reconcile outstanding runs and set a separate
`paths.state_database` in the same update. A database bound to a different
workspace or endpoint (even if empty) returns 409. Choose a new path or a
compatible database, or use the CLI and follow the migration instructions.
Existing files are checked read-only and must be empty or have a supported
GenAImage schema. The file and its directory must be writable (SQLite needs
WAL sidecar files); for a new path, the nearest existing directory must be
writable. Invalid targets return 422. Saving does not create or migrate a database.

`POST /api/v1/settings/test` accepts the same JSON shape (`{}` to use the
saved settings). It tests ComfyUI and a configured LLM without saving or
changing the running adapter. The probes run concurrently with a two-second
budget each. The LLM probe sends an unauthenticated `GET` to `{base}/models`,
or `/v1/models` if the URL has no base path. It reports `ok` for an HTTP 2xx
response, `unavailable` if the probe fails, and `not_configured` if
`llm.base_url` is unset or empty. Both probes pin validated loopback addresses
and never follow redirects or use proxies. The API accepts only loopback URLs
for new endpoints and for probes; configure allow-listed private/Compose hosts
via the file or environment instead.

### Environment overrides

Environment values take precedence over the selected file. Host lists are
comma-separated; an empty value clears the list.

| Setting | Environment variable |
| --- | --- |
| `server.host`, `server.port` | `GENAIMAGE_SERVER_HOST`, `GENAIMAGE_SERVER_PORT` |
| `server.max_upload_bytes` | `GENAIMAGE_SERVER_MAX_UPLOAD_BYTES` |
| `comfyui.base_url` | `GENAIMAGE_COMFYUI_URL` |
| `comfyui.request_timeout_seconds` | `GENAIMAGE_COMFYUI_REQUEST_TIMEOUT_SECONDS` |
| `comfyui.websocket_reconnect_seconds` | `GENAIMAGE_COMFYUI_WEBSOCKET_RECONNECT_SECONDS` |
| `comfyui.allowed_hosts` | `GENAIMAGE_COMFYUI_ALLOWED_HOSTS` |
| `llm.base_url`, `llm.model` | `GENAIMAGE_LLM_URL`, `GENAIMAGE_LLM_MODEL` |
| `llm.allowed_hosts` | `GENAIMAGE_LLM_ALLOWED_HOSTS` |
| `paths.state_database`, `paths.workflows` | `GENAIMAGE_STATE_DATABASE`, `GENAIMAGE_WORKFLOWS` |
| `paths.runs`, `paths.uploads` | `GENAIMAGE_RUNS`, `GENAIMAGE_UPLOADS` |
| One-time legacy state claim | `GENAIMAGE_CLAIM_LEGACY_STATE=1` |

Loopback endpoints are allowed. A private IP or Compose service name must be
listed exactly in that service's `allowed_hosts`, for example:

```toml
[comfyui]
base_url = "http://comfyui:8188"
allowed_hosts = ["comfyui"]

[llm]
base_url = "http://ollama:11434/v1"
allowed_hosts = ["ollama"]
model = "your-explicit-model"
```

Allow-listing never permits public addresses. The ComfyUI adapter resolves all
addresses on the first network operation (with a bounded DNS lookup), rejects
any public result and pins HTTP and WebSocket connections to the validated
addresses. HTTP redirects and proxies are disabled. Offline commands do not resolve service names. The settings LLM probe
uses the same address validation and pinning, restricted to loopback.

### Migrating legacy configuration and data

Back up the old file and data. Replace `active_profile` and `[profiles.<name>]`
with `[comfyui]`, copying the selected profile's `base_url`, timeout and
reconnect settings. Remove `GENAIMAGE_PROFILE`; set `GENAIMAGE_COMFYUI_URL` or
edit `[comfyui]` instead. Remove the retired `paths.characters` key. Unknown
and old keys are rejected rather than ignored.

The new defaults do not move existing data. **Keep the original database and
runs directory at their original absolute locations**: run records and output
records contain absolute paths, so copying or moving these directories alone
will not preserve access. Back them up with SQLite-aware tooling while the
application is stopped. Opening the database upgrades it to SQLite schema
version 3; older GenAImage releases will reject that version, so retain the
backup if rollback is needed. Set `paths.state_database = ".genaimage/state.sqlite3"`
and `paths.runs = "runs"` for the original workspace, and keep the original
ComfyUI endpoint. Do not copy the files to the new defaults and delete the old
locations.

Legacy databases do not record workspace or endpoint identity. Before opening
one, verify that **all** its runs belong to this workspace and ComfyUI endpoint;
if not, keep it separate and do not reconcile or cancel those runs. Once verified,
set `GENAIMAGE_CLAIM_LEGACY_STATE=1` for the first command opening it. This
atomically binds the database to that workspace and endpoint; unset the variable
afterwards. Subsequent attempts to use the database with a different workspace
or endpoint fail, even if the variable is set. Use a separate database for each
workspace and endpoint. No automatic path rewriting or data migration is offered.

## 3. CLI

Install Rust 1.80 or newer and a C toolchain. Check the project and local
ComfyUI connection without submitting a run:

```bash
cargo run -p genaimage -- workflow list
cargo run -p genaimage -- workflow validate text-to-image
cargo run -p genaimage -- doctor
```

## 4. Verification

Run `python3 scripts/ci_check.py`. It expects Python 3.11 or newer and the Rust
toolchain with rustfmt and clippy. If `apps/portal/package.json` exists, Node.js
22, npm and a lockfile are also required; the script installs locked dependencies
and runs portal checks.
