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

Health, events, workflows, runs, outputs, uploads, settings and assistant conversations are available.
The [OpenAPI 3.1 contract](api/openapi.json) is also served at
`GET /api/v1/openapi.json`. Open `http://127.0.0.1:3000/` for the embedded
portal. The assistant page is planned.

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

## Container

The root `Dockerfile` builds a distroless image (under 60 MB, non-root user
65532) with the portal embedded. To run it with ComfyUI and Ollama in one
command, see [Docker Compose](compose.md). Published images are described
under [Releases](#releases).

```sh
docker build -t genaimage:dev .
docker run -d --name genaimage -p 127.0.0.1:8190:8190 \
  -v genaimage-data:/data genaimage:dev
```

Open `http://127.0.0.1:8190/`. The container health check runs
`genaimage healthcheck`, which succeeds when `/api/v1/health` answers on
`GENAIMAGE_SERVER_PORT` within two seconds; an unavailable ComfyUI does not make
the container unhealthy. CLI commands run with `docker exec` use the same port
unless `--server` or `GENAIMAGE_SERVER` is set.

- **Bind exception.** The server normally refuses every non-loopback host. The
  image sets `GENAIMAGE_CONTAINER=1`, and only exactly `1` allows
  `GENAIMAGE_SERVER_HOST` to be `0.0.0.0` or `::`; `true`, `0` and any other
  value do not, and other non-loopback addresses such as `192.0.2.1` stay
  refused. A container cannot be reached on its own loopback, so this is needed
  inside it. Isolation relies on publishing the port to `127.0.0.1` on the host
  only: never use `-p 8190:8190` or `0.0.0.0:8190`.
- **Port.** The image listens on 8190. Requests are checked against the listener
  port, so publish the same host port (`127.0.0.1:8190:8190`); a different host
  port is refused with 421.
- **Data.** `/data` is the volume: `/data/state.sqlite3`, `/data/runs`,
  `/data/uploads` and `/data/config` (settings file).
- **Workflows.** Starter workflows ship read-only at
  `/usr/share/genaimage/workflows/comfyui` (`GENAIMAGE_WORKFLOWS`). To use your
  own, mount them and override the variable, for example
  `-v ./my-workflows:/workflows:ro -e GENAIMAGE_WORKFLOWS=/workflows`.
- **Services.** ComfyUI and the LLM stay outside the image. Point
  `GENAIMAGE_COMFYUI_URL` and `GENAIMAGE_LLM_URL` at them and list non-loopback
  hosts in `GENAIMAGE_COMFYUI_ALLOWED_HOSTS` and `GENAIMAGE_LLM_ALLOWED_HOSTS`;
  public endpoints remain refused.

## Releases

Not yet available: nothing is published until the first release tag exists.
Each release provides archives for Linux (x86_64, aarch64), macOS (arm64,
x86_64) and Windows (x86_64), each containing the `genaimage` binary with the
portal embedded, a `SHA256SUMS` file, and a multi-arch image
(`linux/amd64`, `linux/arm64`) at `ghcr.io/eemaldi/genaimage`. Every release is
tagged `<version>`; `latest` moves only for stable releases, and pre-releases
(for example `v0.1.0-rc.1`) never move it.

```sh
# Download an archive and SHA256SUMS from the GitHub Release, then verify.
sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify genaimage-0.1.0-x86_64-unknown-linux-gnu.tar.gz --repo eemaldi/genaimage
tar -xzf genaimage-0.1.0-x86_64-unknown-linux-gnu.tar.gz
./genaimage-0.1.0-x86_64-unknown-linux-gnu/genaimage --version

docker pull ghcr.io/eemaldi/genaimage:0.1.0
gh attestation verify oci://ghcr.io/eemaldi/genaimage:0.1.0 --repo eemaldi/genaimage
```

On macOS use `shasum -a 256 --check --ignore-missing SHA256SUMS`. The binaries
are not code-signed or notarised, so macOS and Windows may warn on first run.
Maintainers: see [releasing](releasing.md).

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
directory. `genaimage config show` displays the running server’s resolved
configuration, not a local file selected by the CLI.
If no `--config`, `GENAIMAGE_CONFIG` or current-directory file is selected,
GenAImage checks for a clearly legacy workspace file before XDG discovery. It
asks you to migrate or move that file aside after preserving its settings and
data. It detects only files accepted by the old schema and rejected by the new
schema. An ambiguous paths-only file is not selected automatically: use
`--config <path>` to select it. The check tolerates unreadable or malformed
files; explicit and current-directory selections do not inspect an unselected
workspace file.

The file contains `[server]`, `[comfyui]`, `[llm]` and `[paths]` sections;
see `genaimage.example.toml`. The `serve` command uses `[server]`. The settings
connectivity test can check an explicitly configured LLM URL, but it does not
start a service or select a model.
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

Assistant conversations use the workspace state database. On the first
conversation creation, GenAImage writes the editable default system prompt to
`$XDG_DATA_HOME/genaimage/workspaces/<workspace-hash>/assistant/prompts/system.md`.
Edit this regular UTF-8 text file to change the prompt used by subsequently
created conversations. Each conversation retains an exact prompt snapshot and
SHA-256 digest, so later file edits do not change an existing conversation.
If the configured text model changes after a conversation was created, new
turns use the explicit current text model and each assistant message records
the model used; the conversation's original model remains visible as its
creation model. The assistant HTTP and SSE contract is in the OpenAPI document.
Image turns use standard base64 strings for PNG, JPEG or WebP content and are
limited to 20 MiB of decoded bytes per image. Stored conversation reads return
the same base64 representation. SSE failure events contain safe summaries and
may omit `message_id` when terminal persistence did not succeed.

### Settings API

`GET /api/v1/settings` returns a `settings` map keyed by dotted TOML names.
Each entry includes its effective `value`, `source` (`default`, `file` or
`env`), the source `file` or environment `variable` where applicable, and
`read_only`. Environment overrides and the legacy-state claim flag cannot
be edited through the API. Only explicitly listed, non-secret settings are
returned. `config_file` shows the write target.

Send a sparse JSON object to `PUT /api/v1/settings`, for example
`{"comfyui.base_url":"http://127.0.0.1:8189","server.port":3001}`. Omitted
keys are unchanged. GET and successful PUT responses include a strong `ETag`.
Send it in `If-Match` with PUT to reject concurrent changes atomically (412
with a recovery hint); reload and review changes before resubmitting. PUT
without `If-Match` remains available for older clients but can overwrite
concurrent changes. Invalid, unknown or changed read-only keys return 422
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
saved settings). An optional `If-Match` header rejects stale revisions with
412 before probing; omit it only for legacy clients. Omit the query to test ComfyUI and a configured LLM together,
or pass `?service=comfyui` or `?service=llm` to test only that service; the
unselected status is `not_configured`. Environment-overridden endpoints use
their effective values. Testing does not save settings or change the running
adapter. The LLM test uses the configured provider, including its API key and
timeouts, lists the models at `{base_url}/models`, and returns capability
metadata for every model. It does not follow redirects or use proxies. API-key
values are never returned: settings GET reports only `api_key_set` and settings
PUT accepts a new non-empty string or `null` to clear the key. Capability
probes are cached for 30 seconds; a connection test refreshes the cache
explicitly. ComfyUI remains live-checked, while a configured LLM change waits
for restart.

`llm.base_url` defaults to an empty value so the remainder of GenAImage works
without an LLM. To enable the assistant, set it to the OpenAI-compatible root;
the documented local default is `http://127.0.0.1:11434/v1`, select exact
`text_model` and optional `vision_model` IDs, and test the connection. The text
role must support tools and the vision role must support vision. A model can be
overridden in `[llm.capabilities."<model>"]`, but GenAImage never substitutes
the other role. Both probes pin validated addresses. The API accepts only
loopback URLs for new endpoints and probes; configure allow-listed private or
Compose hosts through the file or environment. Public endpoints are never
permitted.

### Setup wizard

The portal's first-run setup wizard (`#/setup`) opens automatically on the
dashboard when `GET /api/v1/setup/status` reports `needs_setup`. You can dismiss
it (remembered per browser only, per set of blocking reasons) and reopen it from
the Setup link in the navigation. Its steps are: detect services, test ComfyUI,
choose the LLM endpoint and text and optional vision model from the installed
models, verify a checkpoint, review and save, apply changes, and run a test
generation.

Nothing is chosen for you and nothing is saved until you review every value.
A step is marked done only when the server confirmed it for your current
choices: "Ran" for detection, "Done" for a ComfyUI test or an LLM text model
verified at the endpoint you chose, and "Saved" only for exactly the values that
were saved; changing a choice afterwards shows "Changed since saved". Choosing a
different LLM endpoint clears the models chosen for the old one. Checkpoints are
listed by the running server, so a changed ComfyUI endpoint shows them as
verified on the running server only. The review lists anything not verified
before you save. Saving uses the settings ETag (`If-Match`); if a save fails
with a network or server error, the outcome is reported as unknown and you
reload the current settings. The new configuration is not live until you
restart `genaimage serve` (or the Compose container). The wizard lists the
pending `restart_required` keys and reports "no pending restart" only after a
fresh status confirms it; if the status cannot be refreshed it tells you to
assume a restart is needed. Endpoints owned by environment variables, as in
Compose, are shown read-only and never written. `POST /api/v1/setup/detect`
probes a fixed list of common local ports and Compose hosts. Hosts that are not
allow-listed are reported with the setting to add, never probed. See the
[architecture](architecture.md#local-http-server).

The test generation step runs the `text-to-image` starter workflow once. It
refuses to run until a checkpoint is chosen, the running server is confirmed to
use the ComfyUI URL you chose (restart after saving, then use "Check again") and
the workflow exists; each unmet check explains its fix. It lists every value it
will send, including inherited workflow defaults, an editable test prompt and an
exact seed, and asks the server to validate them. "Run test generation" then
submits that reviewed request once, pinned to the workflow revision and with one
idempotency key that is kept until the outcome is definitive, so a reload or an
uncertain submission offers a retry with the same key and never a new one. The
step is "Done" only after the server confirms the run produced outputs in this
session. A successful technical run is not creative approval.

### Environment overrides

`GENAIMAGE_CONTAINER=1` is not a setting: it only lets `server.host` be
`0.0.0.0` or `::` (see [Container](#container)).

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
| `llm.base_url` | `GENAIMAGE_LLM_URL` |
| `llm.api_key` | `GENAIMAGE_LLM_API_KEY` |
| `llm.text_model`, `llm.vision_model` | `GENAIMAGE_LLM_TEXT_MODEL`, `GENAIMAGE_LLM_VISION_MODEL` |
| `llm.allowed_hosts` | `GENAIMAGE_LLM_ALLOWED_HOSTS` |
| `llm.connect_timeout_secs` | `GENAIMAGE_LLM_CONNECT_TIMEOUT_SECS` |
| `llm.request_timeout_secs` | `GENAIMAGE_LLM_REQUEST_TIMEOUT_SECS` |
| `llm.idle_timeout_secs` | `GENAIMAGE_LLM_IDLE_TIMEOUT_SECS` |
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
text_model = "your-explicit-text-model"
vision_model = "your-explicit-vision-model"
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
edit `[comfyui]` instead. Remove the retired `paths.characters` key. Replace the
retired `llm.model` with `llm.text_model` and optionally `llm.vision_model`; the old key and
`GENAIMAGE_LLM_MODEL` fail with migration guidance. Unknown
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

Install Rust 1.80 or newer and a C toolchain. For a default source build,
install Node.js 24 and npm, then run `(cd apps/portal && npm ci && npm run build)`
from the repository root before building or installing the CLI. The installed
binary does not require Node.js. Use `--no-default-features` on Cargo commands
to build without the embedded portal. For development only, pass
`--portal-dev-url http://127.0.0.1:5173` to `genaimage serve` to serve Vite
over the GenAImage origin instead; for HMR, open Vite directly.

Check the project and local ComfyUI connection without submitting a run. Start
`genaimage serve` in a separate terminal first; see the [CLI reference](cli.md)
for server selection.

```bash
cargo run -p genaimage -- workflow list
cargo run -p genaimage -- workflow validate text-to-image --preset fast \
  --set checkpoint='"my-local-checkpoint.safetensors"' \
  --set positive_prompt='"an approved, specific visual description"'
cargo run -p genaimage -- doctor
```

Models are never downloaded automatically. See [models](models.md) to list,
pull and place them.

## 4. Verification

Run `python3 scripts/ci_check.py`. It expects Python 3.11 or newer and the Rust
toolchain with rustfmt and clippy. Node.js 24 and npm are also required; the
script installs locked dependencies and runs portal checks. For the portal's
loopback development server and API proxy, see [portal development](../README.md#portal-development).
