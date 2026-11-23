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
  transport port, to be formalised as `ImageBackend` in
  [#19](https://github.com/eemaldi/genaimage/issues/19), and has no HTTP or
  WebSocket client dependencies.
- `genaimage-comfyui` implements that port over HTTP and WebSocket, mapping
  ComfyUI responses and progress events into core-owned types.
- `genaimage` provides the existing scripted CLI. It supplies the ComfyUI
  adapter to the core harness until the local API is available.
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
local options when their compatible APIs are enabled. Traits will provide
extension points for providers, ComfyUI transport and assistant tools; fakes
can exercise them in tests without real models. They do not imply arbitrary
plugin loading or providers already implemented.

ComfyUI and the LLM service remain separate from GenAImage. A planned local
Docker Compose setup can run the dependencies with host-published ports bound
to loopback; GenAImage can run on the host. Compose does not change the server's
loopback-only bind or justify automatically installing or changing models,
nodes or settings.

Outbound ComfyUI and LLM endpoints default to loopback. The target policy also
permits explicitly allow-listed private hosts or Compose service names, subject
to local reachability and validation of resolved addresses and redirects;
public endpoints and blanket private-network access are excluded. This is
planned: current ComfyUI profiles accept loopback only. Inbound server binding
remains loopback only even when an outbound host is allow-listed.

## Run and assistant flow

1. Load a ComfyUI API-format workflow and strict GenAImage sidecar manifest;
   validate node/input bindings, types, presets and explicit parameter values.
2. A planned assistant may propose values or actions through its tool runtime.
   The user can edit the proposal and must explicitly confirm it before the
   assistant submits a run; model output or a tool call is not confirmation.
3. Check available workflow classes with ComfyUI, upload explicit image inputs,
   and record source hashes and returned filenames. Save the resolved workflow
   and preflight snapshot under a new run directory.
4. Submit once with a harness-selected prompt ID and client ID. Observe
   progress, reconcile queue/history and capture checksummed output and run
   provenance.

Never silently substitute a model, node, seed, image, prompt or setting. Safe
reads may retry, but an ambiguous prompt submission remains
`unknown_submission`: reconcile with ComfyUI queue/history rather than blindly
retrying. Preserve source workflows and existing run records; write new
versions instead of overwriting them. A successful technical run is not creative
approval.

## State and provenance

SQLite holds operational queue and event state. Portable run manifests record
resolved inputs, workflow, identifiers, outputs and checksums; preserve them
across reconnects and migrations.

## Performance budgets

These are target budgets to measure as the planned components are delivered:

| Measure | Budget |
| --- | --- |
| Cold start | < 150 ms |
| Idle RSS | < 40 MB |
| Binary size | < 25 MB |
| Portal initial JavaScript | < 150 KB gzip |
| p95 non-ComfyUI API latency | < 20 ms |
