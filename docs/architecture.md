# Harness architecture

[ADR 0001](adr/0001-target-architecture.md) records the accepted target
direction. This document distinguishes that target from the implementation
available today; planned components and budgets are not yet guarantees.

## Responsibility boundary

GenAImage is a thin, fast, local glue layer between an operator, a local LLM
and ComfyUI. ComfyUI generates images; GenAImage owns the workflow manifests,
explicit parameter binding, run queue, reconciliation, provenance and, in the
planned architecture, the assistant tool runtime and self-hosted web portal.

## Workspace crates

Current dependencies (arrows point to dependencies):

```text
genaimage client modules -> genaimage-api
genaimage serve module -> genaimage-server -> genaimage-api
        |                       +-> genaimage-core
        +-> genaimage-comfyui -> genaimage-core
        +-> genaimage-core
genaimage local config module -> genaimage-core (configuration only)
```

- `genaimage-core` owns configuration, workflow manifests and explicit
  binding, run orchestration, SQLite run state and provenance. Core decides
  when to submit, reconcile, cancel and finalise runs. It calls a narrow
  `ImageBackend` trait and has no HTTP or WebSocket client dependencies.
- `genaimage-comfyui` implements `ImageBackend` over HTTP and WebSocket, mapping
  ComfyUI responses and progress events into core-owned types.
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
  URL parsers treat them as path navigation.
- `genaimage-server` is a thin loopback-only Axum adapter. It currently serves
  health, workflows, runs, outputs, uploads, settings and live events. The
  embedded Svelte portal is planned.
- `genaimage-llm` is planned for [#50](https://github.com/eemaldi/genaimage/issues/50)
  and is not a workspace member today.

The CLI uses the loopback API; the planned browser portal will use the same API.
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
  `not_configured`. It checks the image backend through `Harness::health` on
  each request with a two-second deadline. If unavailable, `image_backend_reason`
  is `timeout` or `probe_failed`; neither prevents server startup.
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

The [settings API](setup.md#settings-api) uses core validation and atomic TOML
persistence. The server serialises writes and publishes the saved configuration
and the active harness together; handlers snapshot the active harness before
using it. The event bus survives a swap. ComfyUI timing changes retain the
active adapter’s DNS pin. Endpoint identity and storage changes wait for
restart, when core checks database ownership again. Each run mutation takes
one active harness snapshot; submission gives that same snapshot to its
monitor. Tasks from all snapshots share the listener's shutdown drain.

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
The numeric cursor is scoped to the originating bus; do not resume it across
server restarts. Clients should retain the envelope's `bus_id` and resynchronise
if they observe a different one; a numeric ID alone cannot detect every restart.

If a cursor falls outside the replay window, the subscriber falls behind, or
the core invalidates the stream, the server sends a final `event: resync` frame
and closes the stream. Its empty `id:` clears the browser's reconnect cursor;
its JSON `data` has a `reason` and the corresponding details:

| `reason` | Details |
| --- | --- |
| `cursor_out_of_range` | `requested`, `oldest` (or `null`), and `latest` IDs |
| `lagged` | `skipped`: number of broadcast messages missed |
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

The workflows API is read-only apart from validation, which does not submit a run:

- `GET /api/v1/workflows` returns `workflows` (ID, name, version, description,
  tags, category and optional `thumbnail_url`) and non-fatal `diagnostics` for
  manifests that cannot be loaded. For IDs `.` and `..`, `thumbnail_url` is
  null because browsers normalise those URL path segments. The API does not
  generate a detail URL for any ID; callers using dot-segment IDs need an HTTP
  client that preserves the literal request path.
- `GET /api/v1/workflows/{id}` returns the v2 manifest, including parameter
  constraints and UI hints, presets and outputs. IDs are identifiers, not paths;
  URL-encode them as path segments. An unknown ID returns 404 problem details.
  Duplicate IDs and invalid manifests claiming an ID must be fixed before that
  ID can be resolved.
- `POST /api/v1/workflows/{id}/validate` accepts a JSON object with optional
  `preset` and `parameters` (an object of overrides). Preset values are overridden
  by parameters and manifest defaults fill remaining values. It returns `valid`,
  `parameter_errors` (each with `parameter` and `reason`), and `node_check`.
  The node status is `available`, `missing` (with `missing_classes`) or
  `unavailable` (with a reason); an unavailable or timed-out check is not valid.
  An unknown preset or malformed request returns problem details. Validation
  neither uploads images nor submits generation.
- `GET /api/v1/workflows/{id}/thumbnail` serves a declared PNG, JPEG, GIF, WebP
  or AVIF from the manifest directory (including subdirectories), up to 10 MiB.
  Missing, unsupported, oversized, absolute, traversal or escaping symlink
  paths return 404 problem details. On Unix, descriptor-relative no-follow
  opens reject symlinks below the configured workflow root, whose own symlinks
  are trusted and resolved before it is pinned. Non-regular files are rejected
  without blocking on FIFOs. On other
  platforms containment is checked before and after opening on a best-effort
  basis; this cannot prevent concurrent path swaps. The catalogue URL indicates
  a declaration, not a guarantee that the image can be read.

The versioned [OpenAPI 3.1 contract](api/openapi.json) is served at
`GET /api/v1/openapi.json`. Server routes and the contract are registered
together with `utoipa-axum`; regenerate it with
`cargo run -p genaimage-server --example openapi`. CI checks for drift,
structural errors and coverage through the actual router. The SSE response describes event names and links
to the `EventEnvelope` and `ResyncEvent` schemas. The browser portal is planned.

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

### Runs API

| Method and path (under `/api/v1`) | Behaviour |
| --- | --- |
| `POST /runs` | Submit one explicit run. Returns 202 and its run snapshot without waiting for generation. `Location` identifies the run. |
| `GET /runs` | List matching run snapshots, newest first. |
| `GET /runs/{id}` | Return the current persisted run snapshot. |
| `GET /runs/{id}/events` | Return the latest persisted events in ascending event ID order. `limit` defaults to 50 and accepts 1–200. |
| `POST /runs/{id}/cancel` | Request pending deletion or running interruption through core; return the current snapshot. |
| `POST /runs/{id}/reconcile` | Inspect ComfyUI queue and history for this run; return the refreshed snapshot. |
| `POST /runs/reconcile` | Reconcile non-terminal runs, including unknown submissions, and return `{ "runs": [...] }`. |

A submission body is `application/json`, for example
`{"workflow":"text-to-image","parameters":{"seed":42}}`. `workflow` is a
configured manifest ID, not a local path. `preset`, `parameters` (default `{}`)
and `version` (an optional new output directory name) are optional. Server-side
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
run's **current** state; a different request for a reserved key returns 409.
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

List responses contain `runs` and `next_cursor` (null at the end). Pass `cursor`
back unchanged with the same filters. Ordering is descending by creation time,
then descending by run ID; the versioned opaque token encodes both sort keys.
This is not a snapshot of changing statuses. Filters are `status` (snake-case
run status), `workflow` (exact manifest ID), and inclusive RFC 3339
`created_from`/`created_to` (URL-encode any `+` offsets). `limit` defaults to 50
and accepts 1–200. Errors use `application/problem+json` with a `recovery` hint.

### Outputs and media API

| Method and path (under `/api/v1`) | Behaviour |
| --- | --- |
| `GET /outputs` | List completed outputs and their provenance metadata without exposing server-local file paths. |
| `GET /outputs/{id}` | Stream the original file with a strong SHA-256 ETag, `Accept-Ranges: bytes` and `Cache-Control: public, max-age=31536000, immutable`. |
| `GET /outputs/{id}/thumbnail?size=128` | Return a cached WebP thumbnail with a strong ETag. |
| `GET /runs/{id}/manifest` | Stream the exact stored `run-manifest.json`, including its output and workflow checksums. Never reconstruct it on read. |

List responses contain `outputs` and `next_cursor` (null at the end). Pass the
opaque cursor back unchanged with the same filters. Outputs are sorted newest
first, with the database output ID breaking timestamp ties. `limit` defaults
to 50 and accepts 1–200. Filter by `run` (run ID), `workflow` (workflow ID),
`created_from` and `created_to` (inclusive RFC 3339 output creation times), or
`tag`. Tags are exact and case-sensitive; they are copied from the workflow
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

The planned LLM provider uses an OpenAI-compatible API, with an explicitly
configured endpoint and model. Ollama, llama.cpp and LM Studio are intended
local options when their compatible APIs are enabled.

The core defines async, object-safe `ImageBackend`, `LlmProvider`,
`AssistantTool` and `EventSink` contracts. The CLI serve module builds an `ExtensionRegistry`
from the resolved ComfyUI settings using an adapter factory supplied by the CLI;
core does not construct adapters. The registry looks up registered extensions by
name without a fallback. Only the image backend is configured today; the LLM
and tool contracts have no runtime implementation. These are compile-time
extension points, not runtime plugin loading. Core lifecycle tests use a fake backend
without connecting to ComfyUI; adapter contract tests still exercise the wire
protocol against a fake server.

ComfyUI and the LLM service remain separate from GenAImage. A planned local
Docker Compose setup can run the dependencies with host-published ports bound
to loopback; GenAImage can run on the host. Compose does not change the server's
loopback-only bind or justify automatically installing or changing models,
nodes or settings.

Outbound endpoints default to loopback. Configuration permits explicitly
allow-listed private hosts and Compose service names. The ComfyUI adapter
validates and pins resolved addresses, rejects public addresses and disables
redirects and proxies. The planned LLM adapter must enforce the same policy.
Server binding is loopback-only even when an outbound host is allow-listed.

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

Applications start one independent forwarding task per registered `EventSink`
using `ExtensionRegistry::forward_event_sinks` and should observe the returned
handles for failures. Each sink receives an explicit resync notification and
its task stops on lag or invalidation. Slow sinks and subscribers do not block
publishers. The CLI waits on run-filtered SSE and reads the durable run state
on completion, stream failure or resync. It falls back to status polling
without submitting or reconciling the run.

## State and provenance

SQLite holds operational queue and event state. Portable run manifests record
resolved inputs, workflow, identifiers, outputs and checksums; preserve them
across reconnects and migrations.

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

## Performance budgets

These are target budgets to measure as the planned components are delivered:

| Measure | Budget |
| --- | --- |
| Cold start | < 150 ms |
| Idle RSS | < 40 MB |
| Binary size | < 25 MB |
| Portal initial JavaScript | < 150 KB gzip |
| p95 non-ComfyUI API latency | < 20 ms |
