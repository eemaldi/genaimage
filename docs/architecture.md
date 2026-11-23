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
          genaimage (CLI binary)
             /              \
            v                |
    genaimage-comfyui        |
            |                |
            v                v
          genaimage-core

    genaimage-api       (skeleton, no dependencies)
    genaimage-server    (skeleton, no dependencies)
```

- `genaimage-core` owns configuration, workflow manifests and explicit
  binding, run orchestration, SQLite run state and provenance. Core decides
  when to submit, reconcile, cancel and finalise runs. It calls a narrow
  `ImageBackend` trait and has no HTTP or WebSocket client dependencies.
- `genaimage-comfyui` implements `ImageBackend` over HTTP and WebSocket, mapping
  ComfyUI responses and progress events into core-owned types.
- `genaimage` provides the existing scripted CLI. It supplies the ComfyUI
  adapter factory to the extension registry and passes the selected backend to
  the core harness until the local API is available.
- `genaimage-api` is reserved for shared API types and the client.
- `genaimage-server` is reserved for the Axum server. It will serve the API and
  the embedded Svelte portal on loopback only; neither is implemented yet.
- `genaimage-llm` is planned for [#50](https://github.com/eemaldi/genaimage/issues/50)
  and is not a workspace member today.

The target is for the CLI and browser portal to use the same loopback API.
The server will use the core and local adapters rather than reimplementing
run orchestration or assistant logic.

## Providers, extensions and deployment

The planned LLM provider uses an OpenAI-compatible API, with an explicitly
configured endpoint and model. Ollama, llama.cpp and LM Studio are intended
local options when their compatible APIs are enabled.

The core defines async, object-safe `ImageBackend`, `LlmProvider`,
`AssistantTool` and `EventSink` contracts. The CLI builds an `ExtensionRegistry`
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
redirects and proxies. The LLM adapter and server remain planned; future
adapters must enforce the same policy. Server binding is configured as
loopback only even when an outbound host is allow-listed.

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
acknowledgement alone leaves the run in `cancelling`; a later success or error
still resolves it. ComfyUI may provide no history for deletion of a queued
prompt, and its global interrupt can affect a different prompt if the target
finishes after the queue check.

Preserve source workflows and existing run records; write new
versions instead of overwriting them. A successful technical run is not creative
approval.

## Core events

The in-process `EventBus` publishes typed run lifecycle, progress, output,
backend health and settings-change events. Each JSON event is an envelope with
`id`, `timestamp` (UTC RFC 3339) and a snake-case `type` discriminator plus
variant-specific fields. The bus assigns increasing IDs and timestamps; IDs
are ordered by publication, but wall-clock timestamps need not be monotonic.
The orchestrator publishes lifecycle changes after recording them, and retains
its existing database events. Health changes are published when queried; the
settings event is available to future settings writers.

The default bus retains 1,024 events in memory. `subscribe_since` replays
strictly after a cursor and hands over atomically to the live broadcast. A
cursor before the retained window or ahead of the bus is rejected with a
resync-required reason. A lagging subscriber receives a resync-required error
and must reload authoritative state and subscribe again; it must not treat a
partial stream as complete. IDs and replay are local to one bus lifetime and
are not durable across restarts: reconnecting to a new bus requires a full
resync, even if a numeric cursor happens to fit. Broadcast buffering can be
rounded up by Tokio; neither it nor the replay buffer grows with slow readers.

Applications start one independent forwarding task per registered `EventSink`
using `ExtensionRegistry::forward_event_sinks` and should observe the returned
handles for failures. Each sink receives an explicit resync notification and
its task stops on lag. Slow sinks and subscribers do not block publishers. The
CLI consumes progress from the bus, retaining its existing JSON output. SSE
transport remains planned for [#30](https://github.com/eemaldi/genaimage/issues/30).

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
