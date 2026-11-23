# Setup

## 1. ComfyUI

Install and configure ComfyUI separately. Start its server and confirm that the
local HTTP endpoint responds. GenAImage expects HTTP health, object information,
prompt, upload, queue, history and output endpoints, plus WebSocket events.
Export workflows from ComfyUI in **API format**. The harness does not install
models or custom nodes.

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

The file contains `[server]`, `[comfyui]`, `[llm]` and `[paths]` sections;
see `genaimage.example.toml`. The server and LLM are planned components, so
their validated settings do not start either service. Set `llm.model` explicitly
when an LLM integration becomes available: no model is selected automatically.
The server bind host is restricted to loopback regardless of outbound settings.

State defaults to `$XDG_DATA_HOME/genaimage/state.sqlite3`, runs to
`$XDG_DATA_HOME/genaimage/runs`, and uploads to
`$XDG_DATA_HOME/genaimage/uploads`. If unset, `XDG_DATA_HOME` defaults to
`~/.local/share`. Workflows remain under `workflows/comfyui` in the workspace.
Uploads is reserved for future use; the current adapter uploads explicit input
files directly. XDG directories must be absolute.

### Environment overrides

Environment values take precedence over the selected file. Host lists are
comma-separated; an empty value clears the list.

| Setting | Environment variable |
| --- | --- |
| `server.host`, `server.port` | `GENAIMAGE_SERVER_HOST`, `GENAIMAGE_SERVER_PORT` |
| `comfyui.base_url` | `GENAIMAGE_COMFYUI_URL` |
| `comfyui.request_timeout_seconds` | `GENAIMAGE_COMFYUI_REQUEST_TIMEOUT_SECONDS` |
| `comfyui.websocket_reconnect_seconds` | `GENAIMAGE_COMFYUI_WEBSOCKET_RECONNECT_SECONDS` |
| `comfyui.allowed_hosts` | `GENAIMAGE_COMFYUI_ALLOWED_HOSTS` |
| `llm.base_url`, `llm.model` | `GENAIMAGE_LLM_URL`, `GENAIMAGE_LLM_MODEL` |
| `llm.allowed_hosts` | `GENAIMAGE_LLM_ALLOWED_HOSTS` |
| `paths.state_database`, `paths.workflows` | `GENAIMAGE_STATE_DATABASE`, `GENAIMAGE_WORKFLOWS` |
| `paths.runs`, `paths.uploads` | `GENAIMAGE_RUNS`, `GENAIMAGE_UPLOADS` |

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
addresses, rejects any public result and pins HTTP and WebSocket connections to
the validated addresses. HTTP redirects and proxies are disabled. Service
names must resolve locally when connecting. The future LLM adapter must use the
same resolved-address policy; its endpoint is not contacted today.

### Migrating legacy configuration and data

Back up the old file and data. Replace `active_profile` and `[profiles.<name>]`
with `[comfyui]`, copying the selected profile's `base_url`, timeout and
reconnect settings. Remove `GENAIMAGE_PROFILE`; set `GENAIMAGE_COMFYUI_URL` or
edit `[comfyui]` instead. Remove the retired `paths.characters` key. Unknown
and old keys are rejected rather than ignored.

The new defaults do not move existing data. To retain old records, either set
`paths.state_database = ".genaimage/state.sqlite3"` and `paths.runs = "runs"`
in the new file, or stop GenAImage and copy the old database (including any
SQLite WAL/SHM files) and runs directory to the new locations. Keep backups.

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
