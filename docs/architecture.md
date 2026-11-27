# Harness architecture

[ADR 0001](adr/0001-target-architecture.md) records the accepted target
direction. This document distinguishes that target from the implementation
available today; planned components and budgets are not yet guarantees.

## Responsibility boundary

GenAImage is a thin, fast, local glue layer between an operator, a local LLM
and ComfyUI. ComfyUI generates images; GenAImage owns the workflow manifests,
explicit parameter binding, run queue, reconciliation, provenance, the
assistant tool runtime and the self-hosted web portal.

## Workspace crates

Current dependencies (arrows point to dependencies):

```text
genaimage client modules -> genaimage-api
genaimage serve module -> genaimage-server -> genaimage-api
        |                       +-> genaimage-core
        +-> genaimage-comfyui -> genaimage-core
        +-> genaimage-llm -> genaimage-core
        +-> genaimage-core
genaimage local config module -> genaimage-core (configuration only)
genaimage-llm -> genaimage-core (OpenAI-compatible provider)
genaimage serve module -> genaimage-webhook -> genaimage-core (webhook event sink)
```

- `genaimage-core` owns configuration, workflow manifests and explicit
  binding, run orchestration, SQLite run state and provenance. Core decides
  when to submit, reconcile, cancel and finalise runs. It calls a narrow
  `ImageBackend` trait and has no HTTP or WebSocket client dependencies.
- `genaimage-comfyui` implements `ImageBackend` over HTTP and WebSocket, mapping
  ComfyUI responses and progress events into core-owned types.
- `genaimage-llm` implements `LlmProvider` for OpenAI-compatible local chat and
  model-listing endpoints. It handles SSE text and tool-call streams, vision
  data URLs and endpoint validation without moving assistant orchestration out
  of core. The isolated serve module registers it when `llm.base_url` is set.
- `genaimage-webhook` implements the webhook `EventSink` with endpoint
  validation, retries and a dead-letter log; see [event sinks](#event-sinks).
- `genaimage` provides the scripted CLI. Its client modules use only the HTTP
  API through `genaimage-api` for GenAImage functionality. The isolated `serve`
  module supplies the ComfyUI adapter factory, constructs the core harness and
  passes it to `genaimage-server`. The local configuration module uses core's
  example configuration and error types for `config init`, without opening a
  harness. CI rejects client references to the core, adapter, server and local
  modules; only `main.rs` may call the local dispatch functions, whose public
  interfaces cannot expose local types. Client files also reject `#[path]`
  attributes and all file-inclusion macros, including those with computed paths.
  See the [CLI reference](cli.md) for server selection and output contracts.
- `genaimage-api` holds shared API wire types and OpenAPI schemas. It also
  exposes an async `genaimage_api::client::Client` with the opt-in
  `client` feature (reqwest with rustls). The server uses the wire types
  without enabling the client in production. The client defaults to
  `http://127.0.0.1:3000`; `Client::with_base_url` accepts a custom loopback
  origin. A confirmed connection refusal suggests starting `genaimage serve`;
  other connection failures are reported separately. The client returns typed
  problem details and streams media responses without buffering.
  Live events are a typed SSE stream with explicit cursor replay and resync
  handling, not automatic reconnection. Run submissions require a
  caller-supplied idempotency key and are never retried by the client.
  Workflow IDs `.` and `..` cannot be used in client URL paths because HTTP
  URL parsers treat them as path navigation; catalogue discovery reports them
  as invalid and asks for a different ID.
- `genaimage-server` is a thin loopback-only Axum adapter. It currently serves
  health, workflows, runs, outputs, uploads, settings and live events. The
  Svelte portal shell is embedded when the default `portal` feature is enabled.

The CLI and the browser portal use the same loopback API. The portal in
`apps/portal` uses Svelte 5, TypeScript, Vite and hash routing, with a typed
API client and one shared event stream. The assistant chat panel is lazily
loaded, available on every page and full-width at `#/assistant`. The
default binary embeds
`apps/portal/dist` at compile time, including Brotli and gzip sidecars created
with Node's built-in zlib at portal build time. No Node process is needed at
runtime. API-only builds disable the `portal` feature with
`--no-default-features`; the OpenAPI example deliberately uses this mode.

The server uses the core and local adapters rather than reimplementing run
orchestration or assistant logic.

## Local HTTP server

`genaimage serve` listens on the configured `[server]` host and port (default
`127.0.0.1:3000`). It validates each resolved address before binding. `build_app`
assembles `/api/v1` from per-domain routers sharing an `AppState` with an
`Arc<Harness>` snapshot of the active configuration. Tests can call the router
in-process or serve it on an ephemeral loopback listener without installing
process signal handlers. Pass its actual port to `build_app`; in-process
callers must provide the intended port.

- `GET /api/v1/health` returns HTTP 200 while the server is running, with `self`
  set to `ok`, `image_backend` set to `ok` or `unavailable`, and `llm` set to
  `not_configured`, `ok` or `unavailable`. The response includes `llm_reason`
  plus per-role `llm_text` and `llm_vision` details. It checks the image backend
  through `Harness::health` on each request with a two-second deadline. LLM
  capability data uses a 30-second cache; health starts a background refresh
  when needed and never waits for the LLM. A settings test is an explicit
  refresh. If ComfyUI is unavailable, `image_backend_reason` is `timeout` or
  `probe_failed`; neither prevents server startup. Optional `hints` provide
  recovery guidance for each service.
- `GET /api/v1/doctor` runs the core `doctor` module (`genaimage-core`) and
  returns `{checks: [...]}`: one entry per check id, in a fixed order, each with
  `status` (`ok`, `warning`, `error` or `skipped`), `summary`, bounded `items`
  and a `fix` that is always present unless the status is `ok`. Unlike health,
  it probes ComfyUI (`/system_stats`, `/object_info`), the LLM model listing,
  the workflow node classes, pack requirements, free disk space and configured ports concurrently,
  each with its own deadline (at most about seven seconds in total). It is
  read-only, contacts only the configured endpoints, and never includes the
  LLM API key. See [troubleshooting](troubleshooting.md#doctor-checks).
- `GET /api/v1/setup/status` is the offline first-run assessment (core `setup`
  module); it probes nothing. `needs_setup` is true when any reason is
  blocking: `no_config_file` while the environment does not own
  `comfyui.base_url`, or `invalid_config` when a saved endpoint fails
  validation. Advisory reasons (`endpoints_from_environment`,
  `llm_not_configured`, `llm_text_model_unset`) never set it, so a Compose stack
  with env-owned endpoints and no file does not need setup but may still
  recommend the model steps. It also returns `config_file`,
  `config_file_exists`, `endpoints_env_owned` and each endpoint's
  `configured_url` and `source`. `config_file_exists` is also on
  `GET /api/v1/settings`.
- `POST /api/v1/setup/detect` takes no body and probes a fixed list: ComfyUI at
  `127.0.0.1:8188`, `localhost:8188` and `comfyui:8188`; LLMs at Ollama
  `127.0.0.1:11434/v1` and `ollama:11434/v1`, LM Studio `127.0.0.1:1234/v1` and
  llama.cpp `127.0.0.1:8080/v1`; plus the saved URLs. Every candidate passes
  endpoint validation and DNS-answer checks first; a host outside the allow-list
  is reported as `skipped` with a `fix` naming `comfyui.allowed_hosts` or
  `llm.allowed_hosts` (and the environment variable) instead of being probed.
  Probes run concurrently with a 1.5 s deadline and a 256 KiB body cap, with no
  proxy or redirects. `found` requires the expected protocol (ComfyUI
  `/system_stats` with a `system` object; an OpenAI `/models` list); another
  answer is `error`, silence or refusal is `not_found`. The LLM API key is sent
  only to the saved LLM URL and never returned. It follows the same
  same-origin protection as the other POST routes.
- Requests get a generated `x-request-id` when the caller does not supply one;
  the server returns the ID and includes it in structured request tracing.
- API routing errors use `application/problem+json` (RFC 9457) with a `recovery`
  hint. On SIGINT or SIGTERM, the listener stops accepting requests and new
  run mutations, signals monitors and SSE streams to stop, and drains
  connections and run tasks concurrently. After five seconds it closes
  remaining connections and ends pending backend calls. Before exiting it
  records unconfirmed submissions as `unknown_submission`, rolls back
  cancellations that were not sent, and records attempted but unconfirmed
  cancellations for reconciliation.

The portal handles non-API `GET` and `HEAD` requests. Unknown UI paths fall
back to `index.html`, served with `Cache-Control: no-cache`. Vite's hashed
`/assets/` files are immutable for one year. Static responses vary on
`Accept-Encoding` and retain the original media type when compressed.
`/api` and `/api/*` never fall back to the portal, even outside `/api/v1`.

`genaimage serve --portal-dev-url <url>` replaces embedded serving with an
HTTP proxy to an explicit loopback Vite origin. `localhost` resolves only to
pinned loopback addresses; outbound system proxies and redirect following are
disabled. It forwards UI GET/HEAD requests and selected headers, not browser
credentials or WebSocket upgrades. Hop-by-hop headers, including those named
by `Connection`, are not forwarded in either direction. Vite injects styles during development, so
proxied UI responses allow inline styles only; neither mode allows inline
scripts. Open Vite directly for HMR; Vite's own API proxy targets port 8190.
See [portal development](../README.md#portal-development).

The server accepts `127.0.0.1:<port>`, `localhost:<port>` and `[::1]:<port>`
Host headers for its actual port, plus the configured bind IP when it is a
loopback address. Other loopback IPs are not implicitly allowed. For HTTP port
80, the port may be omitted from Host and Origin; equivalent IPv6 spellings
are accepted. Mutating requests must have a
same-origin `Origin` when present, and `Sec-Fetch-Site` must be `same-origin`
when present. Browser-indicating requests missing both are rejected. Local
non-browser clients without either header are allowed; these checks are not
authentication, and do not protect against local clients forging headers or
legacy browsers that supply no recognisable browser headers. There is no CORS
allow-list. Responses include a same-origin CSP, nosniff, no-referrer and frame
protection, including on errors.

Bodies are bounded before dispatch, including streamed bodies without a length
header: 1 MiB by default, or `[server] max_upload_bytes` for
`POST /api/v1/uploads`, including multipart framing. Uploads are streamed to
disk; other bodies are buffered within their limit. Axum extractor limits
are disabled because the hardening middleware bounds the request body. Each body must arrive within
30 seconds or receives 408; body reads are cancelled with 503 when graceful
shutdown starts, so a stalled upload cannot prevent draining.

Core `LlmRoles` is the public gate for assistant routes. `text()` and
`vision()` return a role binding only for the exact configured model and never
fall back to the other role. A text binding requires a configured, reachable,
listed model but does not itself require tools support; a vision binding still
requires vision support. Capability metadata remains available for reporting.
`LlmRoleBinding::check_request` rejects a mismatched model and known unsupported
tools or vision capabilities before every provider call. The settings
API never serialises the LLM API key; it exposes only `api_key_set` and accepts
an explicit replacement or clear operation.

`POST /api/v1/assistant/prompt` uses the exact configured text role for one
request and returns editable proposals only. It never changes a workflow or
prompt; the response reports `applied: false`. Expand, refine and negative modes
return at most one proposal, while variations requests 1–8 values (four by
default), drops empty or duplicate values and does not pad the result. Refine
requires `instruction`. Negative mode uses optional `original` as the existing
negative prompt and diff baseline; other modes diff against `prompt`. Empty,
oversized or mode-invalid fields are rejected. The LLM stream is collected with
a 64 KiB response ceiling and its strict JSON envelope is parsed without
fabricating a proposal on failure. A malformed or failed provider response is
502. The editable mode templates are `prompt_expand.md`,
`prompt_refine.md`, `prompt_negative.md` and `prompt_variations.md` below the
data directory's `assistant/prompts` directory; their exact names and SHA-256
digests accompany the response. When `workflow_id` is supplied, only that
workflow is loaded. Its description, tags and the named parameter's manifest
hints are included in the model request; an unknown workflow is never replaced
or substituted.

The [settings API](setup.md#settings-api) uses core validation and atomic TOML
persistence. The server serialises writes and publishes the saved configuration
and the active harness together; handlers snapshot the active harness before
using it. The event bus survives a swap. ComfyUI timing changes retain the
active adapter’s DNS pin. Endpoint identity and storage changes wait for
restart, when core checks database ownership again. Each run mutation takes
one active harness snapshot; submission gives that same snapshot to its
monitor. Tasks from all snapshots share the listener's shutdown drain.

### Assistant tool runtime

The conversation service owns assistant tool orchestration. Registered
`AssistantTool` implementations expose a name, description, JSON schema and
confirmation metadata, and are invoked as
`invoke(ToolContext, Value)`. Every request carrying tools is checked against
the selected text model's advertised capabilities; tools are never silently
dropped. A tool loop is bounded to eight model steps by default. Invalid and
unknown calls are recorded and returned to the model as error tool results.

Tools requiring confirmation are validated and persisted but not invoked. The
message stream emits `confirmation_required` and completes its paused turn with
`awaiting_confirmation`, which is not success. The user decides through
`POST /api/v1/assistant/conversations/{id}/confirmations/{confirmation_id}`.
`confirm` uses the proposed arguments, `edit` must revalidate replacement
arguments, and `reject` supplies a rejection tool result. Before either submit
decision, the operator previews effective values and supplies a `preview_token`.
`POST /api/v1/assistant/conversations/{id}/confirmations/{confirmation_id}/preview`
validates replacement arguments and the pinned workflow without making a
decision or changing state, then returns the preview and a digest token over the
confirmation ID, canonical arguments and preview content. The server recomputes
and checks this token before claiming a confirmation; missing, mismatched or
stale tokens leave it pending and return current preview information in the
problem response. Decisions are single use and compare-and-set in SQLite. A new
user message is rejected with 409 while a confirmation remains pending.

Conversation SSE can emit `message_started`, `delta`, `tool_call`,
`tool_result`, `confirmation_required`, `done` and `error`.
`GET /api/v1/assistant/conversations/{id}/tool-calls` exposes the bounded,
append-only audit trail. The `confirmation_id` passed in `ToolContext` is the
stable idempotency key for a confirmed invocation; tools that submit external
work must reuse it.

The serve entry point registers the built-in read and generation tools in the
core registry. `list_workflows` and `inspect_workflow` expose the bounded local
catalogue; `propose_run` validates a pinned proposal and resolves preset,
default and inherited inputs; `submit_run` requires confirmation and reuses its
single confirmation ID as the submission idempotency key; `run_status` only
exposes outputs for completed runs; `get_output` returns metadata without bytes;
and `review_image` calls the exact configured vision role and appends a
model-opinion annotation. Review is automatic because it is an append-only
operator assessment rather than a generation submission, does not alter run
provenance and uses no text-model fallback. A `submit_run` pause can include an
optional read-only `preview` in `confirmation_required`; it contains every
effective value the user is approving, while the original editable `arguments`
remain available separately. The event and preview response carry the matching
`preview_token`. Every built-in tool result is bounded to 32 KiB; oversized text
and arrays are explicitly marked as truncated, while numbers and identifiers
are never shortened and are instead omitted with a note when necessary.

### Live event stream

`GET /api/v1/events` returns `text/event-stream` without compression, with
`Cache-Control: no-cache` and `X-Accel-Buffering: no`. A `: connected` comment
flushes the response; `: heartbeat` comments are sent after every 15 seconds of
stream inactivity. Comments do not change the last event ID. The stream ends
when the server starts graceful shutdown.

Each ordinary frame has a decimal `id` from the core event bus, an `event`
name equal to the envelope's snake-case `type`, and a `data` line containing
the entire [core event envelope](#core-events) as JSON. For example:

```text
id: 42
event: backend_health_changed
data: {"bus_id":"6b43069f-ae9b-497a-8bb6-0d3129058eb8","id":42,"timestamp":"2026-01-01T10:00:00Z","type":"backend_health_changed","backend":"comfyui","health":null,"error":"Unavailable"}

```

Without a cursor the stream delivers only new events. Supply `Last-Event-ID`
(or `?last_event_id=` when the header is absent) to replay events strictly after
that ID, then continue live without a gap or duplicate at the handover. IDs
must be unsigned decimal integers; malformed cursors return 400 problem
details. `?run_id=` selects events for that exact run in both replay and live
delivery, excluding global events such as backend health and settings changes.
Filtered streams can have gaps because IDs are assigned across the whole bus.
The numeric cursor is scoped to the originating bus. Send the envelope's
`bus_id` as a query parameter with the replay cursor to detect a server restart
before any replay or live event. `bus_id` must be a UUID and requires an event
ID; cursors without `bus_id` remain supported but cannot reliably detect a
restart.

If a cursor belongs to a different bus or falls outside the replay window,
the subscriber falls behind, or the core invalidates the stream, the server sends a final `event: resync` frame
and closes the stream. Its empty `id:` clears the browser's reconnect cursor;
its JSON `data` has a `reason` and the corresponding details:

| `reason` | Details |
| --- | --- |
| `cursor_out_of_range` | `requested`, `oldest` (or `null`), and `latest` IDs |
| `lagged` | `skipped`: number of broadcast messages missed |
| `bus_changed` | `requested` and `current` bus UUIDs |
| `invalidated` | `id`: the invalidation boundary |

A client receiving `resync` should stop automatic reconnects (close the
`EventSource` in its `resync` listener). Open a replacement stream without the
stale cursor and begin buffering its events before fetching an authoritative
snapshot. Once the snapshot arrives, apply buffered events in order,
idempotently and without overwriting newer snapshot state, then continue
processing live events. If the replacement stream also signals `resync`, start
again with a fresh stream and snapshot. Resync frames are not subject to
`run_id` filtering. Slow readers do not block event publishers.

### Workflows API

Workflow discovery spans one writable user root (`paths.workflows`) and every
read-only [pack](packs.md) below `paths.packs`. The core merges them, tags each
workflow with its `source` and treats an ID claimed twice as a diagnostic that
makes the ID unavailable. The index cache fingerprints every root. Pack
requirements are checked by one core function shared by the packs API and the
doctor.

The workflows API is read-only apart from validation, which does not submit a run,
`POST /api/v1/workflows/refresh`, which forces a catalogue rebuild, and the
import endpoints, whose confirmation writes one new user workflow:

- `GET /api/v1/workflows` returns `workflows` (ID, name, version, description,
  tags, category, `source`, `available`, `unavailable_reason`, `revision` and optional `thumbnail_url`) and non-fatal `diagnostics` for
  manifests and references that cannot be inspected, including IDs `.` and `..`
  which browsers normalise as URL path segments. An unreadable directory or
  manifest makes the catalogue incomplete; its diagnostic identifies the problem.
  The API does not generate detail URLs.
- `POST /api/v1/workflows/refresh` rebuilds the catalogue and returns the same
  listing. The IDs `refresh` and `import` are reserved. `POST /api/v1/workflows/import`
  proposes a manifest for an API-format graph without writing, and
  `POST /api/v1/workflows/import/confirm` writes it as a new user workflow; see
  [importing a workflow](workflow-manifest.md#importing-a-comfyui-workflow).
  `GET /api/v1/packs` lists packs with
  their workflow IDs, templates, requirements and diagnostics, and
  `GET /api/v1/packs/{id}/validate` checks the requirements against ComfyUI
  `/object_info`, reporting missing, unknown and unreachable as such.
- `GET /api/v1/workflows/{id}` returns the v2 manifest, including parameter
  constraints and UI hints, presets and outputs, plus `revision`. The effective `required` and
  output `kind` remain available; `declared_required` and `declared_kind` are
  null when the manifest omitted those fields, otherwise they contain the
  declared value. Responses from older servers may omit these fields; the CLI
  preserves that unknown state when serialising them. IDs are identifiers, not
  paths; URL-encode them as path segments. An unknown ID returns 404 problem
  details.
  Duplicate IDs and invalid manifests claiming an ID must be fixed before that
  ID can be resolved. If directory traversal or manifest reading cannot establish
  uniqueness, detail, validation, submission and rerun refuse the ID with a hint
  to repair the path or its permissions.
- `POST /api/v1/workflows/{id}/validate` accepts a JSON object with optional
  `preset`, `parameters` (an object of overrides) and `workflow_revision`. Preset
  values are overridden by parameters and manifest defaults fill remaining values.
  HTTP 200 returns the `revision` of the snapshot checked, `valid`,
  `parameter_errors` (each with `parameter` and `reason`), and `node_check`.
  The node status is `available`, `missing` (with `missing_classes`) or
  `unavailable` (with a reason); an unavailable or timed-out check is not valid.
  An unknown preset or malformed request returns problem details. Validation
  neither uploads images nor submits generation. `inherited_inputs` lists
  optional parameters left unbound and their node inputs from that snapshot:
  exact JSON literals or node output connections. These are not bindings. When
  `workflow_revision` is supplied, validation checks it before contacting ComfyUI and returns the same
  workflow-changed problem as submission if the snapshot no longer matches.
- `GET /api/v1/workflows/{id}/thumbnail` serves a declared PNG, JPEG, GIF, WebP
  or AVIF from the manifest directory (including subdirectories), up to 10 MiB.
  Missing, unsupported, oversized, absolute, traversal or escaping symlink
  paths return 404 problem details. A discovery index is built on the first
  thumbnail request and checked at most once per second by a shared revalidation
  of directory, manifest, workflow and thumbnail metadata; each catalogue list
  forces an immediate rebuild. Metadata and directory traversal failures are
  tracked and reported as diagnostics without hiding unrelated valid workflows.
  A cache hit checks its own manifest metadata and reads the current thumbnail
  safely; an edited manifest is reloaded. Workflow-set changes, such as a newly
  added duplicate or an inaccessible directory, can be stale for up to one
  second plus the revalidation time. A revalidation that cannot establish
  uniqueness makes thumbnail requests fail until the path is repaired. The
  detail endpoint checks traversal immediately. Repeated changes during
  discovery produce a diagnostic and suppress unverified thumbnail entries;
  the next revalidation rebuilds an unstable index rather than reusing it. A
  settings change switches to the new harness's index. On Unix, descriptor-relative
  no-follow opens reject symlinks below the configured workflow root, whose own symlinks
  are trusted and resolved before it is pinned. Non-regular files are rejected
  without blocking on FIFOs. On other
  platforms containment is checked before and after opening on a best-effort
  basis; this cannot prevent concurrent path swaps. The catalogue URL indicates
  a declaration, not a guarantee that the image can be read.

The versioned [OpenAPI 3.1 contract](api/openapi.json) is served at
`GET /api/v1/openapi.json`. Server routes and the contract are registered
together with `utoipa-axum`; regenerate it with
`cargo run -p genaimage-server --no-default-features --example openapi`. CI checks for drift,
structural errors and coverage through the actual router. The SSE response describes event names and links
to the `EventEnvelope` and `ResyncEvent` schemas. Portal API integration is planned.

### Uploads API

`POST /api/v1/uploads` accepts multipart form data with one file field named
`file`, for example `curl -F 'file=@reference.png' "$ORIGIN/api/v1/uploads"`.
Files are identified as PNG, JPEG or WebP by their signatures, not by their
filenames or supplied media types. This does not decode or validate pixels.
Multipart framing before the first file (including the preamble and headers)
is limited to 16 KiB, independently of the configured request size.
The server streams to a temporary directory, calculates SHA-256, then publishes
both bytes and metadata atomically in the configured uploads directory.

The response is 201 with `upload_id` (`sha256:<lowercase hex digest>`),
`sha256`, `byte_size`, `media_type` and `original_filename` (a sanitised
basename). Identical bytes share one upload ID and keep the first accepted
filename. A repeat upload verifies the stored copy and repairs missing or
corrupt bytes atomically without changing the metadata. The server refuses
symlinked digest directories, image files and metadata files. Unix reads and
repairs are relative to the opened digest directory; elsewhere, type checks
are best effort and do not guard against concurrent path swaps. `Location` points to
`GET /api/v1/uploads/{id}` for the same metadata; it does not serve the image
bytes. Malformed multipart requests return 400,
oversized requests 413 and unrecognised signatures 415, with problem details
and a recovery hint.

### Models API

Model bootstrap is explicit. `GET /api/v1/models/llm` lists installed LLM models
(Ollama `/api/tags`, or `/v1/models` with `pull_supported: false`) and flags the
configured text and vision models as present or missing.
`POST /api/v1/models/llm/pulls` is the only operation that downloads, and runs
only on that request. The core `LlmModelManager` port is implemented by the
Ollama adapter in `genaimage-llm`, and the core `PullRegistry` keeps pulls in
memory, one per model name and at most two at a time. Pull state is not
persisted; see [models](models.md) for the re-attach and restart semantics.
`GET /api/v1/models/checkpoints` reads ComfyUI `object_info` and fails when it
cannot, rather than returning an empty list.

### Runs API

| Method and path (under `/api/v1`) | Behaviour |
| --- | --- |
| `POST /runs/{id}/rerun` | Create a distinct run using verified source provenance. Returns 201 and the new run snapshot and `Location`. |
| `POST /runs` | Submit one explicit run. Returns 202 and its run snapshot without waiting for generation. `Location` identifies the run. |
| `GET /runs` | List matching run snapshots, newest first. |
| `GET /runs/{id}` | Return the current persisted run snapshot. |
| `GET /runs/{id}/events` | Return the latest persisted events in ascending event ID order. `limit` defaults to 50 and accepts 1–200. |
| `POST /runs/{id}/cancel` | Request pending deletion or running interruption through core; return the current snapshot. |
| `POST /runs/{id}/reconcile` | Inspect ComfyUI queue and history for this run; return the refreshed snapshot with its `outcome`. |
| `POST /runs/reconcile` | Reconcile non-terminal runs, including unknown submissions, and return `{ "runs": [...] }`, each with its own `outcome`. |

A submission body is `application/json`, for example
`{"workflow":"text-to-image","parameters":{"seed":42}}`. `workflow` is a
configured manifest ID, not a local path. `preset`, `parameters` (default `{}`)
and `version` (an optional new output directory name) are optional. Pass the
reviewed workflow’s opaque `revision` as `workflow_revision` to pin the
submission to its exact API graph and sidecar bytes. A stale revision returns
HTTP 409 `application/problem+json` with type
`urn:genaimage:problem:workflow-changed` before backend I/O. Reload the
workflow and review the resolved parameters again before submitting. Omitting
`workflow_revision` (or sending `null`) is unpinned: core uses the current
workflow and its defaults. The CLI currently submits unpinned. Server-side
image file paths are not accepted. Use the `upload_id` string returned by the
uploads API as the value of an `image_upload` parameter, for example
`"parameters":{"reference":"sha256:<digest>"}`. Workflow validation reports
unknown upload IDs against their parameter names. The response
includes the run and prompt IDs, workflow ID/version/hash, effective parameters,
status, timestamps and any recorded error. An acknowledged partial submission
is accepted, and its `node_warnings` appear in the run and its persisted
acknowledgement event even if cancellation is in progress.

Every submission requires one `Idempotency-Key` of 1–128 visible ASCII
characters, excluding commas. Use a fresh key for each distinct run. The server
hashes the decoded request (JSON object member order, whitespace and equivalent
omitted defaults do not change the key's meaning). The core reserves the key
and creates the run atomically in SQLite before uploading references or sending
the prompt. Keys remain
bound to their runs across restarts. A replay returns HTTP 200 and the original
run's **current** state without rechecking the workflow revision. The revision
is part of the request fingerprint: changing it under the same key returns the
existing idempotency conflict (HTTP 409, `about:blank`).
Validation rejected before reservation does not consume the key. If artefact
preparation fails after reservation, the run is recorded as `failed` and is not
sent. Before sending a reference to ComfyUI, the core verifies the SHA-256 of
the exact bytes it will send. It records the upload ID, source hash, sanitised
original filename, byte size and returned ComfyUI filename in provenance. A
failed reference upload fails the run before `/prompt`. ComfyUI uploads are
not retried: an unconfirmed upload may have left an unused backend input file,
but it cannot have queued generation. Replaying a key never uploads again.
A process crash during preparation or submission can leave a `submitting`
record; recovery conservatively treats that record as unconfirmed. Replaying its key or
reconciling it recovers it as `unknown_submission` when the run uses the
submission-lock protocol and no process holds its lock. This does not release
the key or resend the prompt. Older runs without a protocol marker require
queue or history evidence to advance; if neither identifies the prompt, an
operator must investigate and decide how to recover.

A timeout or other unconfirmed `/prompt` response produces
`unknown_submission`. Neither a retry with the same key nor reconciliation
resends the prompt. Use reconciliation to check queue and history. For newly
accepted queued runs, the server observes progress via the core event bus and
persists backend progress and lifecycle events. Monitoring stops on shutdown;
use reconciliation to refresh old runs. A cancellation request can leave the
run in `cancelling` until ComfyUI provides evidence for the target prompt.

Reconciliation reports an `outcome` separately from the durable run `status`:

| Outcome | Meaning |
| --- | --- |
| `terminal` | The run was already terminal or has now reached a terminal state. Check `status` for the result. |
| `active` | ComfyUI reports the prompt pending or running; the original run is tracked under its existing ID. |
| `in_progress_elsewhere` | A live submission owner still holds the run. |
| `not_found` | The run is still `unknown_submission` and its prompt was absent from both queue and history in a check started after the settle window. |
| `still_ambiguous` | The evidence did not settle the run's state, for example because history has an incomplete entry and the prompt is not in the queue. |

Explicit reconciliation records a `run_reconciled` event with the outcome;
background polling does not write these audit events. The server starts a
monitor as soon as queue evidence is reflected in an active run record, even
if reading history or reconciling another run later fails. The settle window starts
at the persisted submission attempt and lasts for its configured ComfyUI
request timeout plus 10 seconds (70 seconds with default settings). For
older records without a recorded attempt, absence alone remains inconclusive.
An absent prompt within the window returns
`still_ambiguous` with `settles_at`, the earliest time to check again.

ComfyUI denotes absent history with an empty object; unexpected envelopes
are inconclusive. Success requires valid output references and either positive
completion status or a legacy outputs-only entry. `not_found` means absent from queue
and history after the settle window. ComfyUI cannot prove absence, so very late
admission is still possible and a later reconcile can adopt the original run.
Jobs requires explicit confirmation after `not_found` before calling the
separate re-run endpoint. A definitive rejection requires fresh reconciliation
and confirmation before another attempt. An ambiguous response does not reopen
that action. Neither reconcile endpoint resubmits a prompt.

Re-run takes no request body or `Content-Type` header. It shares the submission
key namespace with `POST /runs`; the key identifies the operation and source
run ID, not the source's current status. The first call returns 201 with a
`RunResponse` (including `rerun_of`) and a `Location` pointing to the new run.
A replay returns 200 with the new run's current state and the same `Location`,
without verifying or submitting again. The source is never updated by re-run.
Errors use `application/problem+json`: 400 for a non-empty body or an invalid
or missing key, 404 for an unknown source, and 409 for a key already bound to
a different request
(all `about:blank`). Unverifiable or changed provenance returns 409 with type
`urn:genaimage:problem:rerun-mismatch` and guidance to restore the original
files or use Remix. Neither a 404 nor a fingerprint conflict consumes a key.

Any source status is eligible if provenance is verifiable, including
`unknown_submission`. The Jobs UI must reconcile an unknown submission and
only offer **Submit as new run** after `not_found`; the API does not enforce
this UI policy. A fresh key can create a second backend job if the first one
was accepted. For an uncertain HTTP result, keep the same key to check the
new run. As for regular submission, a created run may itself be failed or
unconfirmed; inspect its `status` rather than treating 201 as generation success.

List responses contain `runs` and `next_cursor` (null at the end). Pass `cursor`
back unchanged with the same filters. Ordering is descending by creation time,
then descending by run ID; the versioned opaque token encodes both sort keys.
This is not a snapshot of changing statuses. Filters are `status` (snake-case
run status), `workflow` (exact manifest ID), and inclusive RFC 3339
`created_from`/`created_to` (URL-encode any `+` offsets). `limit` defaults to 50
and accepts 1–200. Errors use `application/problem+json` with a `recovery` hint.

### Batches API

A batch runs one reviewed, pinned workflow revision over an explicit seed list
or a value grid. Each item is an ordinary run submitted through the keyed run
path; the batch only groups them. Nothing is random, truncated or substituted.

| Method and path (under `/api/v1`) | Behaviour |
| --- | --- |
| `POST /batches/preview` | Expand and validate without submitting or contacting ComfyUI. Returns `item_count`, `max_items`, `max_items_cap`, `workflow_revision`, the `inherited_inputs` that every item shares (listed once) and every item's exact values. |
| `POST /batches` | Validate, record the batch and its items atomically, and return 202 with the batch and `Location`. Requires `Idempotency-Key`. |
| `GET /batches` | List batches newest first (`workflow`, `cursor`, `limit` of 1–200, default 50) with state and item counts, using a fixed number of queries per page. It never recovers submissions (see below). |
| `GET /batches/{id}` | Return the batch with every item's submission state and run status. It also settles a run whose submission owner died as `unknown_submission`, as `GET /runs/{id}` does. |
| `POST /batches/{id}/resume` | Submit the items that have no run (see below). 202 if a driver started, otherwise 200 with the current state. |
| `GET /runs?batch={id}` | List the runs of one batch. `RunResponse` and run events carry `batch_id` and `batch_index` (absent for ordinary runs). |

The request body is shared by preview and submission. `workflow_revision` and
`max_items` are required, and unknown fields are rejected:

```json
{
  "workflow": "text-to-image",
  "workflow_revision": "<revision from the reviewed workflow>",
  "preset": "fast",
  "parameters": {"steps": 20, "cfg": 2.0},
  "sweep": {"seeds": {"parameter": "seed", "values": [18446744073709551615, 7]}},
  "max_items": 2
}
```

`sweep` is exactly one of `{"seeds": {"parameter": "<seed parameter>", "values":
[...]}}`, an explicit list for a parameter of kind `seed`, or `{"grid":
{"<parameter>": [v1, v2, ...], ...}}`. A grid is the cartesian product in a
fixed order: parameter names sort by byte order and the **last name varies
fastest**. For `{"cfg": [2, 3], "steps": [10, 20]}` the items are
`(2, 10)`, `(2, 20)`, `(3, 10)`, `(3, 20)`. Each item's run request is the base
`parameters` plus that item's swept values, with the same precedence as a
single run (preset, then explicit values). It is serialised exactly as a
normal submission of the same request, so its fingerprint is the same.

Rules, all enforced before anything is recorded or sent (HTTP 422 unless noted):

- A swept parameter must not also appear in `parameters`; it is rejected, never
  overridden. Sweep parameters must be defined by the workflow.
- The serialised parameters of all items together must not exceed 4 MiB
  (`urn:genaimage:problem:batch-too-large`), which bounds the preview, the
  stored batch and its responses. The preview repeats every item's parameters,
  swept values and effective parameters, so those together must not exceed
  16 MiB either; a longer one is rejected as `batch-too-large` instead of
  returned, and submission applies the same limit.
- Every list is non-empty and has no repeated value. Numbers compare exactly
  and numerically (decimal arithmetic, never `f64`), so `2` and `2.0` are
  duplicates ("values at positions 0 and 1 are numerically equal"), and the
  accepted spelling is preserved: `2.0`, `-0.0` and seeds up to 2^64 - 1 reach
  ComfyUI unchanged.
- A grid lists each parameter once. A name repeated in the JSON object is
  rejected with `urn:genaimage:problem:batch-invalid`, never merged, so a later
  list cannot silently replace an earlier one.
- The item count is the product of the list lengths. It must not exceed
  `max_items` or the server limit of 256 (`max_items_cap`); otherwise
  `urn:genaimage:problem:batch-too-large`. Nothing is truncated.
- Every item is validated against the pinned revision with the workflow
  validation rules (including upload IDs). If any item is invalid nothing is
  submitted and the key stays free: `urn:genaimage:problem:batch-items-invalid`
  carries `invalid_item_count` and up to 20 `invalid_items`, each with its
  `index` and every parameter error. Other shape errors use
  `urn:genaimage:problem:batch-invalid`.
- A stale `workflow_revision` returns 409 `urn:genaimage:problem:workflow-changed`.
  A key reused with another body returns the idempotency conflict (409).

Submission also checks node availability once. The key space is separate from
the runs API; use a fresh key per reviewed batch and retry with the same key.

**Items and keys.** Item `i` of batch `batch-<uuid>` is submitted under the
idempotency key `batch-<uuid>:<i>`, which always fits the 128-character limit
because it is built from the batch ID rather than the client's key. Because
those keys use the ordinary run path, an item gets the existing guarantees: one
run per key, an ambiguous send becomes `unknown_submission` and is reconciled
(`POST /runs/{id}/reconcile`), and nothing is retried blindly. A database
unique index allows one run per `(batch_id, batch_index)`.

**Submission states.** Each item reports `submission`:

| State | Meaning |
| --- | --- |
| `pending` | Not yet attempted, or being submitted now. |
| `submitted` | A run exists and ComfyUI acknowledged it. `run_status` is the run's own status; `completed` appears only once the backend confirmed outputs. |
| `failed` | No prompt was sent for the item (`error` says why). Without a `run_id` it was rejected before a run was recorded, and resume retries it. With a `run_id` the run failed while it was being prepared, which is final: resume never retries it. |
| `not_submitted` | Never attempted, with the reason in `error`: a restart, shutdown, a panic in the driver or a workflow revision change. |
| `outcome_unknown` | The run is `unknown_submission`: ComfyUI may have accepted it. Reconcile it; it is never resubmitted. |

The batch `state` reports the driver and never overstates success: `submitting`,
`interrupted` (some items `not_submitted`), `submitted` (every item has an
acknowledged run; run outcomes are separate), `partial` (some failed or unknown)
and `failed` (none accepted). A failed item does not stop later ones. A change
that affects every item (the workflow revision, an ambiguous workflow ID, a
shutdown, or ComfyUI not answering the per-item node check) stops the driver,
and the remaining items become `not_submitted` with the reason ("ComfyUI
unreachable; resume when it is back"). A failure of one item alone, such as a
node only it needs being missing, fails only that item.

**Driver and recovery.** The server creates the batch row, its items and a
driver claim (an OS file lock, like run submission) together, then spawns a
tracked task that submits the items one at a time in index order and returns 202
without waiting. Creating the batch and starting its driver happen inside one
tracked task, so a client that disconnects cannot leave a batch without a
driver. A running driver keeps the settings, and the harness, it started with
until it finishes or is resumed; a settings change applies to the next resume.
The task is wrapped so that, even after an error or a panic, every item without
a run is settled as `not_submitted` and none stays pending or becomes
successful. On a graceful shutdown the driver finishes the item in flight and
leaves the rest `not_submitted`; if the shutdown deadline expires first, the
item in flight is recorded as `unknown_submission`. On start, the server marks
batches left `submitting` by a dead process as `interrupted` with their unsent
items `not_submitted`, unless another live process holds the batch's claim; it
never resumes them.

To resume, re-`POST /batches` with the same key and body (it replays with 200
and resumes), or call `POST /batches/{id}/resume`, which needs no key and is
what a client holding only the batch ID should use. Resuming submits every item
that has no run (`pending`, `not_submitted`, and `failed` items without a
`run_id`, which were rejected before any prompt was sent). Items with a run,
including `failed` ones with a `run_id` (final) and `outcome_unknown` ones,
replay through their item key and are never sent again, so a crash between the send and the bookkeeping cannot duplicate a
prompt. A resume while a driver is running starts nothing. The pinned revision
is checked for every item, so resuming after the workflow changed leaves the
rest `not_submitted`.

`GET /batches` reads the stored state in a fixed number of queries per page and
never runs recovery, so a run whose submission owner died still shows as
`pending` there until the batch is read with `GET /batches/{id}`, which settles
it as `unknown_submission`. That settlement is the same one a read of the run
makes.

Live progress arrives as the usual run events, which carry `batch_id` and
`batch_index`; there is no separate batch event. Items that fail without a run
produce no event, so poll `GET /batches/{id}` while the state is `submitting`.

### Outputs and media API

| Method and path (under `/api/v1`) | Behaviour |
| --- | --- |
| `GET /outputs` | List indexed outputs from completed runs by default, without exposing server-local file paths. |
| `GET /outputs/{id}/metadata` | Fetch the same metadata for one output, without transferring media bytes. |
| `GET /outputs/{id}` | Stream the original file with a strong SHA-256 ETag, `Accept-Ranges: bytes` and `Cache-Control: public, max-age=31536000, immutable`. |
| `GET /outputs/{id}/thumbnail?size=128` | Return a cached WebP thumbnail with a strong ETag. |
| `GET /runs/{id}/manifest` | Stream the exact stored `run-manifest.json`, including its output and workflow checksums. New manifests also include `resolved_workflow_sha256`; legacy manifests may omit it. Never reconstruct it on read. |
| `GET /runs/{id}/resolved-workflow` | Stream the archived resolved graph as JSON. New runs verify its SHA-256 against the fingerprint pinned in the run record before submission; older archives are marked unverified. This does not independently prove what ComfyUI executed. |

List responses contain `outputs` and `next_cursor` (null at the end). Pass the
opaque cursor back unchanged with the same filters. Outputs are sorted newest
first, with the database output ID breaking timestamp ties. `limit` defaults
to 50 and accepts 1–200. Filter by `run` (run ID), `workflow` (workflow ID),
`created_from` and `created_to` (inclusive RFC 3339 output creation times), or
`tag`. `status` selects an exact run status and defaults to `completed`. Only
terminal runs can expose indexed outputs: `failed` and `cancelled` return
only artefacts already indexed for such runs, and other statuses return an
empty page. Tags are exact and case-sensitive; they are copied from the workflow
manifest archived for the run, not from the current workflow catalogue. Runs
completed before media indexing remain available but have no indexed tags.

All three file endpoints support `If-None-Match` (304). The original endpoint
supports one closed, open-ended or suffix byte range (206); unsatisfiable or
multiple byte ranges return 416 with `Content-Range: bytes */<length>`. Unknown
range units are ignored, and oversized byte-range ends and suffixes are clipped
to the representation. An
`If-Range` value other than the current strong ETag falls back to the full file.
Unrecognised or active file types are served as downloads (`application/octet-stream`).

Core opens stored run files beneath the configured runs directory. Originals are
hashed into private on-disk snapshots before serving so a checksum mismatch or
concurrent in-place edit cannot expose unverified bytes. Recorded manifests are
also checked against their indexed SHA-256. For older manifests without a
separately recorded digest, the server validates the manifest structure and
its identifiers against the run record. It returns
`X-GenAImage-Manifest-Integrity: unverified` on successful responses and 304s.
The ETag for a legacy manifest identifies its stored bytes, but does not
establish their provenance. Missing or changed files for recorded media return
409 rather than being silently replaced. Unknown IDs and runs without a completed
manifest return 404.

Thumbnail `size` is required and must be 128, 256, 512 or 1024 (longest edge in
pixels, without upscaling). PNG, JPEG and WebP inputs are decoded off the async
executor, with limits on source size, dimensions and decoded memory. Thumbnails
are cached by source SHA-256 and size under the workspace data directory in
`thumbnails/webp-v1/`; each cached thumbnail is checked against its SHA-256
sidecar before serving. Concurrent misses or repairs share an encoder; cache
files are published through atomic renames. Retention and cache eviction are not implemented.

### API integration tests

`crates/genaimage-server/tests/common` provides `ApiHarness::start()` with an
isolated temporary configuration, data paths, loopback API and the shared
scriptable fake ComfyUI in `genaimage-test-support` (a dev-only dependency).
Tests can script backend state, issue typed JSON requests with same-origin
headers, submit through the runs API or directly through core, and
subscribe before an action then await matching core events with a deadline.
`request_with_origin` can override or omit Origin for hardening tests. The
fake's state switches script health, queue state, completion and ambiguous
submissions; queue and history overrides support more elaborate scenarios.
The fixture uses the default upload limit and the server's fixed body-read
deadline; its fake-backend request timeout defaults to five seconds. Its default
queue and history track the most recently submitted prompt. Await
`wait_for_progress_listener` with a deadline before sending scripted WebSocket
events.
Call `shutdown().await` to signal the server and await a bounded graceful
drain; dropping the fixture signals shutdown and applies a best-effort deadline.

## Providers, extensions and deployment

`genaimage-llm` provides the OpenAI-compatible provider, with explicitly
configured endpoint and role models. Ollama, llama.cpp and LM Studio are
intended local options when their compatible APIs are enabled. The registered
provider drives assistant conversations and tools, prompt assist, workflow
suggestion, operator-requested output reviews and settings capability probes.

Core vision review sends the verified output bytes and recorded run parameters
through the exact configured vision role model on each request. It never falls
back to the text model. The `POST /api/v1/outputs/{id}/review` operator endpoint
stores and returns a model-opinion annotation, while
`GET /api/v1/outputs/{id}/annotations` returns at most the newest 100 annotations,
newest first. Annotations contain the chosen model and creation time and are
append-only in the state database; they do not modify run records or provenance.
The loopback HTTP response deliberately includes the complete sent `prompt` and
`raw_response` for inspection, editing and debugging. They are audit material for
a model opinion, not provenance, and callers must not present them as verified
facts about the output.

PNG, JPEG and WebP inputs are accepted up to 20 MiB and are never silently
downscaled or re-encoded. The editable prompt is written once, and only when
missing, to `assistant/prompts/vision_review.md` beneath the configured
workspace data directory. Existing operator edits are never overwritten. A
missing, unavailable or non-vision role produces reasoned 503 problem details
before a chat request is sent.

The core defines async, object-safe `ImageBackend`, `LlmProvider`,
`AssistantTool` and `EventSink` contracts. The CLI serve module builds an `ExtensionRegistry`
from the resolved ComfyUI settings using an adapter factory supplied by the CLI;
core does not construct adapters. The registry looks up registered extensions by
name without a fallback. The image backend, the LLM provider and the built-in
assistant tools are registered today. The LLM provider implementation and fake
server are available, while the core conversation store and service persist
bounded, immutable message history, snapshot an explicit creation model and
prompt, expose a token stream, and retain cancelled or failed partial assistant
messages. The server resolves the configured text role for every conversation
request and uses its current model for each turn, recording that model on the
assistant message. Its HTTP and SSE routes stream text and tool proposals only;
assistant tool execution remains planned. Editable prompt files live below the
configured data directory under `assistant/prompts`. These are compile-time
extension points, not runtime plugin loading. Core lifecycle
tests use a fake backend
without connecting to ComfyUI; adapter contract tests still exercise the wire
protocol against a fake server.

ComfyUI and the LLM service remain separate from GenAImage. The local
Docker Compose setup ([compose](compose.md)) runs the dependencies with host-published ports bound
to loopback; GenAImage can run on the host. Compose does not change the server's
loopback-only bind or justify automatically installing or changing models,
nodes or settings.

Outbound endpoints default to loopback. Configuration permits explicitly
allow-listed private hosts and Compose service names. The ComfyUI adapter
validates and pins resolved addresses, rejects public addresses and disables
redirects and proxies. The LLM adapter applies that endpoint and resolved-address
policy too, and also disables redirects and proxies. Server binding is
loopback-only even when an outbound host is allow-listed.

## Run and assistant flow

1. Load a ComfyUI API-format workflow and strict GenAImage sidecar manifest;
   validate node/input bindings, types, presets and explicit parameter values.
2. A planned assistant may propose values or actions through its tool runtime.
   The user can edit the proposal and must explicitly confirm it before the
   assistant submits a run; model output or a tool call is not confirmation.
3. Check available workflow classes with ComfyUI, upload explicit image inputs,
   and record source hashes and returned filenames. Save the resolved workflow
   and preflight snapshot under a new run directory.
4. Persist the harness-selected prompt ID and client ID before submitting once.
   Observe advisory progress events, reconcile queue/history and capture
   checksummed output and run provenance.

Never silently substitute a model, node, seed, image, prompt or setting. Safe
reads may retry, but an ambiguous prompt submission remains
`unknown_submission`: reconcile with ComfyUI queue/history rather than blindly
retrying. If neither source identifies the prompt, leave the submission unknown.
Cancellation is confirmed only by evidence for the target prompt, such as
history recording an interruption. A queue/history gap or deletion request
acknowledgement alone leaves the run in `cancelling`. If a cancellation POST
may have reached ComfyUI but its acknowledgement was lost, the run stays in
`cancelling` and records `cancellation_unknown`. Only a definitely unsent or
explicitly rejected request restores the previous state. Reconcile before
trying again: ComfyUI may provide no history for deletion of a queued prompt. For running prompts, GenAImage sends a targeted interrupt only to
recognised ComfyUI releases from 0.3.57 onward. For versions without confirmed
support, upgrade or manage cancellation in ComfyUI itself; GenAImage never
falls back to a global interrupt.

Preserve source workflows and existing run records; write new
versions instead of overwriting them. A successful technical run is not creative
approval.

## Core events

The in-process `EventBus` publishes typed run lifecycle, progress, output,
backend health and settings-change events. Each JSON event is an envelope with
`bus_id`, `id`, `timestamp` (UTC RFC 3339) and a snake-case `type`
discriminator plus variant-specific fields. The bus assigns increasing IDs and
timestamps; IDs are ordered by publication, but wall-clock timestamps need not
be monotonic.
Each bus has a random lifetime identifier. An `EventCursor` combines it with an
ID; `subscribe_from_cursor` rejects cursors from an earlier bus, even when the
numeric ID happens to fit.
The orchestrator publishes lifecycle changes after recording them, and retains
its existing database events. Health changes are published when queried; the
server publishes a settings change after a successful save.

The default bus retains 1,024 events in memory. `subscribe_since` replays
strictly after a cursor and hands over atomically to the live broadcast. A
cursor before the retained window or ahead of the bus is rejected with a
resync-required reason. A lagging subscriber receives a resync-required error
and must reload authoritative state and subscribe again; it must not treat a
partial stream as complete. If a state update may have committed but recording
or publishing its event fails, the bus invalidates existing subscribers and
rejects replay across that boundary. Invalidation reserves an ID and clears the
replay window; no event is silently presented as a complete stream. IDs and
replay are local to one bus lifetime and are not durable across restarts:
reconnecting to a new bus requires a full resync. The numeric-only
`subscribe_since` is for the originating bus lifetime; transport consumers
should retain and check the full cursor. Broadcast
buffering can be rounded up by Tokio; neither it nor the replay buffer grows
with slow readers.

Reconciliations for a run are serialised across harnesses sharing a bus and
state database, including output materialisation. Cancellation has separate
coordination and does not wait for reconciliation's backend calls. Conditional
database updates prevent stale reconciliation or cancellation results from
replacing newer state. Reconciliation and post-submission transition tasks
survive caller cancellation, so a committed terminal state is followed by its
event or explicit invalidation. Other processes have separate buses; a CLI
waiter also publishes a progress snapshot when it observes a terminal state
written by another process.

Applications start event sinks with `genaimage_core::sinks::spawn_sink`, which
gives each sink its own bounded queue, forwarding task and worker. The older
`ExtensionRegistry::forward_event_sinks` is a minimal helper that ends a sink on
lag and is not used by `serve`.

### Event sinks

`serve` builds the configured `[[sinks]]` ([configuration](configuration.md#event-sinks)),
registers them in the `ExtensionRegistry` and starts them on a task tracker.
`JsonlSink` lives in core; `WebhookSink` lives in `genaimage-webhook`, which
carries the HTTP client so core stays free of one.

- **The bus is never blocked.** A forwarder moves events into a per-sink queue
  of 256 notifications and nothing on the publish path awaits a sink.
- **Drops are explicit.** A full queue drops the newest event, counts it and
  puts a gap marker before the next queued event. A bus lag (`ResyncRequired`)
  is counted, queued as a gap and the forwarder re-subscribes at the live tail
  instead of ending. A sink error or panic never stops forwarding; it is
  counted and the next event is attempted.
- **JSONL.** One `EventEnvelope` per line, written with a single append and
  `fsync` per line. Gaps are written as
  `{"type":"sink_gap","reason":"lagged","skipped":N,"at":"..."}`. A line of
  1 MiB or more is replaced by a `sink_oversize` marker and counted as a
  failure. A torn last line from a crash is terminated before the next write.
- **Webhook.** `POST` of the envelope JSON with `Content-Type: application/json`,
  `Idempotency-Key: <bus_id>:<id>`, `X-GenAImage-Event`, `X-GenAImage-Attempt`
  and, with `secret_env`, `X-GenAImage-Timestamp` and
  `X-GenAImage-Signature: sha256=<hex>` where the digest is HMAC-SHA256 over
  `<timestamp>.<body>`. Delivery is **at-least-once and sequential per sink**:
  receivers must de-duplicate on the key. A `2xx` is success. Connection
  errors, timeouts, `408`, `429` and `5xx` are retried with exponential backoff
  (250 ms doubling to 30 s, with jitter) up to `max_attempts`. Any other status,
  including a redirect, is permanent and is not retried. An event that fails
  permanently or exhausts its attempts is appended to
  `<data dir>/sinks/<name>.dead-letter.jsonl` with the attempts, the last
  redacted error and the event. Gap notifications are not posted; receivers
  see a jump in `id`, and doctor shows the count.
- **Shutdown.** After the server has drained, sinks stop intake, deliver the
  queue for up to three seconds and count what remains as dropped. Events
  still queued at that point are not delivered after restart.
- **Status.** Each sink has a shared status (delivered, failed, dropped,
  dead-lettered, resyncs, retries, last redacted error, last success). Counters
  are totals since start. The sink is `failing` after three consecutive failed
  events or an unexpected task stop, `degraded` after any drop, dead letter,
  failure or resync, and otherwise `ok`. The doctor `event_sinks` check reads
  the same status through the bus.

Slow sinks and subscribers do not block
publishers. The CLI waits on run-filtered SSE and reads the durable run state
on completion, stream failure or resync. It falls back to status polling
without submitting or reconciling the run.

## State and provenance

SQLite holds operational queue and event state. Portable run manifests record
resolved inputs, workflow, identifiers, outputs and checksums; preserve them
across reconnects and migrations.

The workflow revision is derived from the existing graph and sidecar
SHA-256 fingerprints. Pinned submission verifies the revision on the same
in-memory snapshot that core binds and uses to build the prompt. The run
record already stores both fingerprints, so no additional provenance field
is needed. The revision covers the two files, not backend model weights or
other external assets referenced by the graph.

Re-run uses the source's recorded workflow ID and version, preset reference,
resolved parameters (including seed) and image references. The core verifies
the graph and manifest SHA-256 and all recorded input hashes against the exact
snapshots it will bind and send; it never reloads unchecked replacements. New
runs store the workflow manifest and preflight fingerprints in SQLite and
archive the manifest bytes used for binding. A re-run records `rerun_of` in the
new run and its completed manifest. Legacy runs without a submission-time
manifest fingerprint remain readable but cannot be exactly re-run, even with
an indexed completion manifest. Use Remix to review their saved parameters and
submit a new run.

Known file-backed ComfyUI loaders (including `LoadImage` and `LoadImageMask`)
must have their file slot bound to a verified upload. A literal ComfyUI-side
filename or a link supplying an unverified path returns 409: changing a file at
that path does not change the workflow hash. Bind supported image inputs as
`image_upload` parameters, or use Remix to review the workflow and submit a new
run. Re-run copies and verifies all recorded inputs into private temporary
files before sending any, then uploads them one at a time from those same
snapshots. Ordinary submissions verify and upload references one at a time.
Local inputs retain their filenames in the private snapshots, so path-only
image backends can upload the exact recorded bytes on both submit and re-run.
The checks do not cover model weights or behaviour inside arbitrary custom
nodes, so identical output is not guaranteed.

The state database uses versioned, transactional SQLite migrations, WAL,
foreign-key enforcement, a ten-second busy timeout and `synchronous=NORMAL`.
Async callers use the blocking-pool repository boundary and `Harness::new_async`
for database opening; synchronous repository methods are for non-async callers.
A legacy unversioned database is migrated in place when its schema is
compatible, with old nullable gate columns left untouched. If migration fails,
stop all GenAImage processes, back up the state database and move it aside
before restarting with a fresh operational cache. Also preserve its `-wal` and `-shm` files if present, and keep `runs/`:
run records are the source of truth. This does not automatically reimport old
runs or recover in-flight queue state. Newer schema versions require a
compatible GenAImage version, rather than a downgrade. Once started, a
blocking-pool write may finish even if its async caller is cancelled.

## Portal UI

**Direction:** Make the active workspace obvious at a glance: a quiet, flat
canvas frames the page, and accents only signal interaction or status. The
#41 shell uses no external visual references, imagery or downloaded fonts.
A rail would trade useful width for persistent navigation; an overlay would
require additional focus management. The selected layout keeps the navigation
in document flow and folds it into a disclosure at 48rem.

Use the semantic CSS properties in `apps/portal/src/styles/tokens.css`:
`canvas` and `surface` separate page from workspace, `subtle` highlights active
or hovered controls, `ink` and `muted` carry text, `border` defines control edges,
`accent` pairs with `on-accent` for primary actions, and `danger` pairs with
`danger-surface` for failures. Keep underlines or labels for state, not colour
alone. The system font stack, rem spacing, control/panel radii and minimum
control size stay consistent across workspaces. Use the shared focus ring and
reserve the 120ms transition for feedback; reduced motion uses 0ms. The portal
uses flat sRGB colours with no external assets, media treatments, camera or
lighting rules. Content should reflow rather than shrink or crop on narrow screens.

### Components

Import Svelte files from `apps/portal/src/lib/ui/`:

| Component | Public interface |
| --- | --- |
| `Button.svelte` | Native button props and child content, plus `variant="primary"` or `"secondary"` (default). Defaults to `type="button"`. |
| `Input.svelte` | `label`, optional `id`, `hint`, `error`, `name`, `required`, `disabled`; `bind:value` (string), `multiline`, `rows`, `placeholder`, `maxlength`, `autocomplete`, `readonly`. |
| `Select.svelte` | Same common field props as Input; `bind:value` (string), `options` (`{ value, label, disabled? }[]`, unique values). |
| `Slider.svelte` | `label`, optional `id`, `hint`, `error`, `name`, `disabled`; `bind:value` (number), `min`, `max`, `step`. |
| `Dialog.svelte` | `bind:open`, `title`, optional `description`, `returnFocus` callback and child content. Escape and Close restore focus to the opener or call the fallback (the shell focuses the current `main`). |
| `Tabs.svelte` | `label`, `tabs` (`{ id, label }[]`, unique IDs), optional `bind:value` (active ID; falls back to first tab), `children(tab)` snippet. Panels remain mounted. |
| `Tooltip.svelte` | `text` and a `children(attrs)` snippet that spreads `attrs` on one focusable trigger. Put essential instructions in visible text instead. |
| `ToastRegion.svelte` | Mounted by the shell; optional `returnFocus` callback for the last dismissal. Use `pushToast({ message, kind?: 'info' \| 'error' })` (returns ID), `dismissToast(id)` or the readable `toasts` store from `toasts.ts`. Notifications persist until dismissed. |
| `ProblemDetails.svelte` | `problem: Problem`, a structural RFC 9457 object (`type`, `title`, `status`, `detail`, `instance`, all optional), with `recovery` and optional `errors: { key, reason }[]`. Displays actionable errors inline; no dependency on the API client. |

`apps/portal/src/shortcuts.ts` exports `sections`, `shortcuts`, and
`createShortcutHandler({ navigate, help }, now?)` with `handle(event)` and
`reset()`. The shell handles enablement and suspends shortcuts in modals;
editable controls suppress shortcuts. Two-key shortcuts expire after 1.2 seconds.

Gallery lightbox navigation follows the filtered results and fetches the next
cursor page at the end of a loaded page. Next retains keyboard focus while
loading so arrow-key navigation can continue. Direct links can show an output before
its position is known; neighbour navigation remains disabled until it is in the
loaded results. The gallery reads recorded run parameters with the typed client's
`getRecordedRun`, preserving large JSON integers as `bigint` where the browser
supplies the original token. Values that cannot be read exactly are shown
separately from identical fields and differences. Vision annotations are loaded
only when an output is opened in the lightbox or its review panel is expanded on
run detail; the gallery grid never requests annotations. They are labelled as
model opinions, separate from run provenance, with the sent prompt and raw
response kept in collapsed details. Model text is rendered as text, and review
is disabled with a Settings hint when no vision model is available. Dialogs
leave focus traversal inside native audio and video controls to the browser.

### Generate share links and Remix

`#/generate?workflow=<id>` opens an empty editor with manifest defaults.
Versioned share links add `v=2`, `wv` (workflow version), optional `wr`
(workflow revision), optional `preset` and `modified=1`, and `p` (URL-encoded
JSON). The `p` value is an array of
`[name, source, value, seedMode?, chosenSeed?]` tuples. Sources are `d`
(default), `p` (preset), `u` (user) and `n` (no source). Text and booleans
use JSON scalars; `null` explicitly means unset, `['int','<decimal>']` and
`['float','<decimal>']` preserve numeric type, and `['draft','<text>']`
retains invalid editor text. Seed modes are `l` (locked) and `r` (randomise).
A randomised field can include its concrete reviewed seed as `chosenSeed`;
it is visible and reused at the next explicit review unless the user requests
another seed. Upload values are `sha256:<digest>` strings, not file contents.
Shared editor links include defaults and explicitly unset fields. Restoration
keeps changed defaults as linked, reports per-field differences even for the
same workflow version, and blocks review on missing fields or unset fields
that now have a default until the user resolves them. Preset-sourced values
are explicit; restoration never applies the current preset behind the scenes.
Links over 32 KiB of encoded fragment or 256 tuples are rejected. Opening a
share link never submits a run. A different revision is shown as a notice;
it does not itself block review. Links copied after validation can include `i`,
containing reviewed inherited inputs for comparison at the next explicit review.
The fragment contains clear-text parameter data: share only with intended
recipients. Upload references only work where those uploads are stored.

`apps/portal/src/routes/generate/share.ts` exports `encodeFormState(state)`
and `buildGenerateUrl(run)`; both return the full route hash, not an origin.
Remix should pass `await api.getRecordedRun(id, { exactBindings: true })`
to `buildGenerateUrl` to keep exact numeric values. A run response does not identify
the parameter value sources, so Remix imports recorded parameters as user values
and locks recorded seeds.
The editor restores and validates them against the current manifest. Run responses
include `workflow_revision` when the original graph and sidecar fingerprints
are recorded; Remix carries it as `wr` and warns if the current revision differs,
even when the workflow version is unchanged. For legacy runs without those
fingerprints, `unverified_source=1` preserves a warning that the source cannot
be verified, including when the restored form is shared or reloaded.

On review, the server validates the concrete parameter snapshot pinned to the
loaded workflow revision. If validation or submission reports that revision
has changed, Generate reloads the workflow, shows differences against the
retained field values where possible, and requires a fresh review. Submit sends
those parameters and the reviewed revision explicitly, not a preset for the
server to expand, and uses one idempotency key for the lifetime of that snapshot. An uncertain request
can be retried explicitly with the same key; it is never retried automatically.
The exact pending request and key stay in this tab’s session storage across
navigation and reload until the server returns a run or definitively rejects
the request. Jobs is the status-check fallback because there is no lookup by
idempotency key. Recovery stays available even if the workflow cannot be loaded.
An older unpinned request cannot be retried; after checking Jobs, the user can
explicitly dismiss it without submitting anything.
Live output cards require both completed run status and indexed outputs.

### Accessibility checks

Use a desktop-width route and a narrow screen with both menu states as review
cases. Verify Tab reaches the skip link, selection is visible without colour,
Escape closes the menu or help and returns focus, and no horizontal scrolling
is needed at 320 CSS pixels. Repeat in both themes and with reduced motion.
Target WCAG AA: 4.5:1 for normal text and 3:1 for control edges and focus rings.
Vitest runs axe on the shell and help, and checks the flat palette pairs
separately. jsdom does not measure rendered colour contrast or implement native
modal behaviour; verify those and assistive-technology announcements in a
browser. The modal uses a heading without introducing a second banner landmark.

For a manual browser regression check at 320 CSS pixels, navigate from Dashboard
to Jobs, open Menu, focus a navigation link, press `?`, go Back, then press
Escape. Focus should land on Dashboard’s `main`. In the UI test fixture at
`apps/portal/src/lib/ui/Fixture.test.svelte`, render `kind="radio"` with and
without `checkedRadio`; Tab must wrap after the final group and Shift+Tab from
Close must reach the checked or first radio. Render `kind="tooltip-edges"` and
Tab to both triggers; each bubble must stay within the viewport. With
`kind="editor"`, Tab from Close must reach the editor and the following Tab
must wrap to Close. With `editorTabIndex: -1`, Tab must skip the editor.
To mount either fixture without adding browser tooling, open the Vite dev
portal and run this in its DevTools console. Reload before changing fixtures.

```js
const kind = 'radio'; // Or 'editor' or 'tooltip-edges'.
const checkedRadio = true; // Also try false for the radio fixture.
const svelte = performance.getEntriesByType('resource').find(({ name }) =>
  new URL(name).pathname.endsWith('/deps/svelte.js'));
const { mount } = await import(svelte.name);
const editorTabIndex = undefined; // Also try -1 for the editor fixture.
const { default: Fixture } = await import('/src/lib/ui/Fixture.test.svelte');
document.querySelector('#app').hidden = true;
mount(Fixture, { target: document.body, props: { kind, checkedRadio, editorTabIndex } });
```

## Performance budgets

These are target budgets to measure as the planned components are delivered:

| Measure | Budget |
| --- | --- |
| Cold start | < 150 ms |
| Idle RSS | < 40 MB |
| Binary size | < 25 MB |
| Portal initial JavaScript | < 150 KB gzip |
| p95 non-ComfyUI API latency | < 20 ms |

### Workflow catalogue timing

The catalogue uses one list request, filters an in-memory search index, and
loads only declared thumbnails lazily. `npm --prefix apps/portal test --
--run src/routes/workflows/catalogue.test.ts` measures the time from resolving
an in-memory fake list response to Svelte's completed DOM update for 100 cards
in jsdom; it logs the elapsed time and enforces a 1,500 ms regression bound.
A focused local run measured 88.3 ms (100 cards, no thumbnails, warm process).
This deliberately loose bound accommodates shared CI machines and is not proof
of the 100 ms local-server target: jsdom has no layout or image loading and the
measurement excludes server discovery, network transfer and browser paint.
For an end-to-end local measurement, a release `genaimage serve` served 100
copies of the bundled text-to-image manifest and API workflow with unique IDs,
without thumbnails. In the integrated Electron 43 / Chromium 150 browser,
navigate from Dashboard to Workflows; record the workflow list resource's
`startTime` and use a `MutationObserver` to detect 100 cards, then two
`requestAnimationFrame` callbacks as a paint approximation. With the app and
filesystem cache warm, after one warm-up navigation, five request-to-paint
samples were 36.5, 35.2, 42.4, 37.7 and 30.0 ms (median 36.5 ms). This is a
local observation, not a guarantee for other hardware, larger manifests,
thumbnail decoding or cold caches.

The detail API normalises omitted parameter defaults and explicit null to the
same value; the portal cannot distinguish those source-manifest cases. The JSON
client cannot represent integers outside JavaScript’s safe range exactly; the detail page marks them as unavailable
instead of presenting rounded values as exact. Validation checks defaults
without overrides; it is not an approval or a guarantee that later
user-supplied values are valid.
