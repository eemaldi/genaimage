# Configuration reference

GenAImage runs without a configuration file. Every key below has a default.
Values come from, in increasing precedence: the built-in default, the
configuration file, then the `GENAIMAGE_*` environment variable. The server
refuses unknown keys, invalid values and zero timeouts at startup.

The file is chosen by `--config`, then `GENAIMAGE_CONFIG`, then
`./genaimage.toml`, then `$XDG_CONFIG_HOME/genaimage/genaimage.toml`; see
[setup](setup.md#2-configuration). Create an example with
`genaimage config init`. `genaimage config show` and `GET /api/v1/settings`
report each effective value with its `source` (`default`, `file` or `env`). A
value set by an environment variable is read-only there and in the portal.

## Keys

| Key | Environment variable | Default | Applies |
| --- | --- | --- | --- |
| `server.host` | `GENAIMAGE_SERVER_HOST` | `127.0.0.1` | Restart |
| `server.port` | `GENAIMAGE_SERVER_PORT` | `3000` | Restart |
| `server.max_upload_bytes` | `GENAIMAGE_SERVER_MAX_UPLOAD_BYTES` | `33554432` (32 MiB) | Restart |
| `comfyui.base_url` | `GENAIMAGE_COMFYUI_URL` | `http://127.0.0.1:8188` | Restart |
| `comfyui.request_timeout_seconds` | `GENAIMAGE_COMFYUI_REQUEST_TIMEOUT_SECONDS` | `60` | Live |
| `comfyui.websocket_reconnect_seconds` | `GENAIMAGE_COMFYUI_WEBSOCKET_RECONNECT_SECONDS` | `2` | Live |
| `comfyui.allowed_hosts` | `GENAIMAGE_COMFYUI_ALLOWED_HOSTS` | empty | Restart |
| `llm.base_url` | `GENAIMAGE_LLM_URL` | empty (LLM and assistant off) | Restart |
| `llm.api_key` | `GENAIMAGE_LLM_API_KEY` | unset; write-only | Restart |
| `llm.text_model` | `GENAIMAGE_LLM_TEXT_MODEL` | empty | Restart |
| `llm.vision_model` | `GENAIMAGE_LLM_VISION_MODEL` | empty | Restart |
| `llm.allowed_hosts` | `GENAIMAGE_LLM_ALLOWED_HOSTS` | empty | Restart |
| `llm.connect_timeout_secs` | `GENAIMAGE_LLM_CONNECT_TIMEOUT_SECS` | `5` | Restart |
| `llm.request_timeout_secs` | `GENAIMAGE_LLM_REQUEST_TIMEOUT_SECS` | `60` | Restart |
| `llm.idle_timeout_secs` | `GENAIMAGE_LLM_IDLE_TIMEOUT_SECS` | `30` | Restart |
| `llm.capabilities."<model>"` | none (file only) | none | Restart |
| `paths.state_database` | `GENAIMAGE_STATE_DATABASE` | `$XDG_DATA_HOME/genaimage/workspaces/<hash>/state.sqlite3` | Restart |
| `paths.workflows` | `GENAIMAGE_WORKFLOWS` | `workflows/comfyui` in the workspace; may be missing | Restart |
| `paths.packs` | `GENAIMAGE_PACKS` (path list) | `["workflows/packs"]` in the workspace | Restart; file only in the settings API |
| `paths.runs` | `GENAIMAGE_RUNS` | `$XDG_DATA_HOME/genaimage/workspaces/<hash>/runs` | Restart |
| `paths.uploads` | `GENAIMAGE_UPLOADS` | `$XDG_DATA_HOME/genaimage/uploads` | Restart |
| `paths.templates` | `GENAIMAGE_TEMPLATES` | `$XDG_DATA_HOME/genaimage/workspaces/<hash>/templates`; created on the first write | Restart |

`XDG_DATA_HOME` defaults to `~/.local/share`. "Restart" means a saved change
appears in `restart_required` until you restart `genaimage serve` (or the
Compose container); "Live" means ComfyUI request and reconnect timings apply
without a restart. Timeouts and `server.max_upload_bytes` must be positive and
`server.port` must be 1 to 65535.

Notes on individual keys:

- `llm.api_key` is never returned, logged or printed. The settings API reports
  only whether it is set and accepts a new string or `null` to clear it.
- `llm.text_model` and `llm.vision_model` are never chosen for you. The text
  model must support tools and the vision model must support vision. Declare a
  reviewed override with `[llm.capabilities."model-id"]` and `tools` and
  `vision` booleans. The two roles never fall back to each other.
- `paths.workflows` is the single writable user workflow root. `paths.packs` is
  a list of read-only pack roots, each a directory of pack directories that
  contain `pack.toml`; the default root holds the bundled `core` pack.
  `GENAIMAGE_PACKS` takes the platform path-list separator (`:` on Linux and
  macOS) and an empty value means no packs. The settings API does not edit
  `paths.packs`; it is preserved when other settings are saved. See
  [workflow packs](packs.md).
- `paths.*` relative values resolve against the workspace (`--workspace`, the
  current directory by default). A database that contains runs is bound to its
  workspace and ComfyUI endpoint; see [setup](setup.md#settings-api).

## Event sinks

`[[sinks]]` tables deliver bus events to a file or a local HTTP receiver
without polling. They are read-only in the settings API: edit the file and
restart the server. Saving other settings keeps every `[[sinks]]` table.
Unknown keys, duplicate names, public endpoints and a missing `secret_env`
variable stop the server from starting with an error naming the key.

```toml
[[sinks]]
name = "audit"              # unique; [a-z0-9][a-z0-9_-]{0,63}
kind = "jsonl"
path = "events.jsonl"       # relative; created below <data dir>/sinks
events = ["run_completed", "run_failed"]   # optional; default is every event

[[sinks]]
name = "local-hook"
kind = "webhook"
url = "http://127.0.0.1:9000/hooks/genaimage"
allowed_hosts = []          # same rules as comfyui.allowed_hosts
secret_env = "GENAIMAGE_HOOK_SECRET"       # optional HMAC key, read from the environment
timeout_seconds = 5         # 1 to 120, default 5
max_attempts = 5            # 1 to 20, default 5
events = ["run_completed"]
```

- `events` lists event `type` names: `run_queued`, `run_started`,
  `run_progress`, `run_output`, `run_completed`, `run_failed`, `run_cancelled`,
  `run_unknown_submission`, `backend_health_changed` and `settings_changed`.
  An empty list is rejected.
- A `jsonl` path must be relative, without `..`, below `<data dir>/sinks`, and
  not shared with another sink. The suffix `.dead-letter.jsonl` is reserved.
  Files are created with mode `0600` and grow until you rotate them; the file is
  reopened for every line, so renaming it is safe.
- A `webhook` URL follows the outbound endpoint rules above: loopback by
  default, private or Compose hosts only when listed in the sink's
  `allowed_hosts`, never public. Every attempt resolves the host again and
  checks every answer; redirects and proxies are disabled.
- `secret_env` names an environment variable. The secret is never stored in
  TOML and never printed, logged, returned or written to a dead-letter record.
  It signs each request (see [delivery semantics](architecture.md#event-sinks)).

Delivery status and failures appear in the `event_sinks` check of
`genaimage doctor` ([troubleshooting](troubleshooting.md#doctor-checks)).

## Other environment variables

| Variable | Meaning |
| --- | --- |
| `GENAIMAGE_CONFIG` | Configuration file to use for `serve`; set (even empty) it makes client commands fail. |
| `GENAIMAGE_SERVER` | Server origin for CLI client commands; `--server` wins. |
| `GENAIMAGE_CONTAINER` | Exactly `1` lets `server.host` be `0.0.0.0` or `::`. The container image sets it. Not a setting. |
| `GENAIMAGE_CLAIM_LEGACY_STATE` | `1` explicitly binds a verified legacy database once; unset it afterwards. |
| `RUST_LOG` | Verbosity of the structured server logs on stderr. |

`claim_legacy_state` is returned by `GET /api/v1/settings` as read-only and is set only through `GENAIMAGE_CLAIM_LEGACY_STATE`.

The retired `GENAIMAGE_PROFILE` and `GENAIMAGE_LLM_MODEL` are rejected with
migration guidance.

## Outbound endpoints and allow-lists

The server binds to loopback only; `0.0.0.0` or `::` is allowed only inside a
container with `GENAIMAGE_CONTAINER=1`, with ports published on `127.0.0.1`
only. Outbound endpoints (`comfyui.base_url`,
`llm.base_url`) may be loopback by default. A private IP address or a Compose
service name must be listed exactly in that service's `allowed_hosts` (comma
separated in the environment; an empty value clears the list). Allow-listing
never permits a public address: every resolved address is checked and
connections are pinned to the validated addresses. Redirects and proxies are
disabled.

```toml
[comfyui]
base_url = "http://comfyui:8188"
allowed_hosts = ["comfyui"]

[llm]
base_url = "http://ollama:11434/v1"
allowed_hosts = ["ollama"]
```

Requests to the server must use a loopback Host with the real listener port;
anything else gets HTTP 421 (see [troubleshooting](troubleshooting.md#http-421-misdirected-request)).

## Compose variables

`.env` also holds Compose-only variables (`COMPOSE_FILE`, `VIDEO_GID`,
`RENDER_GID`, `GENAIMAGE_PORT`, `COMFYUI_PORT`, `OLLAMA_PORT`,
`GENAIMAGE_IMAGE`). They are described in [Docker Compose](compose.md#configuration).
