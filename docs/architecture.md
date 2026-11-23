# Harness architecture

[ADR 0001](adr/0001-target-architecture.md) records the accepted target
direction. This document distinguishes that target from the implementation
available today; planned components and budgets are not yet guarantees.

## Responsibility boundary

GenAImage is a thin, fast, local glue layer between an operator, a local LLM
and ComfyUI. ComfyUI generates images; GenAImage owns the workflow manifests,
explicit parameter binding, run queue, reconciliation, provenance and, in the
planned architecture, the assistant tool runtime and self-hosted web portal.

```text
CLI (thin client, planned) ──┐
                             ├── loopback Rust server + embedded Svelte portal
Browser (local portal) ──────┘                     (planned)
                                      │
                              genaimage-core
                         ┌────────────┼─────────────┐
                    workflows,     assistant      run state,
                    bindings and   tool runtime   reconciliation,
                    ComfyUI client (planned)      provenance
                         │             │
                   local ComfyUI   local LLM endpoint
```

- `crates/genaimage-core` is the domain and generation source of truth. It
  currently owns configuration, workflow binding, ComfyUI transport, SQLite
  run state, cancellation/reconnect and provenance. The assistant runtime and
  provider abstractions are planned here, not in the frontend or CLI.
- `crates/genaimage-cli` currently calls the core directly. Its target is a
  thin scripted client of the same local server API used by the portal.
- A Rust server component, embedded Svelte portal and local API are planned;
  their exact crate and asset paths are not yet established. One server binary
  will serve both API and portal, bound to loopback only.
- The current `apps/desktop` Tauri/React application and character approval
  gates still exist and are enforced where applicable. Both are being retired
  under [epic #2](https://github.com/eemaldi/genaimage/issues/2); neither is the
  future operator surface or approval model. Until removal, do not bypass
  existing checks.

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
across reconnects and migrations. Existing character gate state and decision
records remain in the current implementation until the pilot is retired; their
retirement must not overwrite previously written records.

## Performance budgets

These are target budgets to measure as the planned components are delivered:

| Measure | Budget |
| --- | --- |
| Cold start | < 150 ms |
| Idle RSS | < 40 MB |
| Binary size | < 25 MB |
| Portal initial JavaScript | < 150 KB gzip |
| p95 non-ComfyUI API latency | < 20 ms |
