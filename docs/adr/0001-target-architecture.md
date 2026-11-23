# ADR 0001: Target architecture

- **Status:** Accepted
- **Scope:** GenAImage's target architecture; implementation is incremental.

## Context

GenAImage is a thin, fast, local glue layer and harness. ComfyUI generates
images; local LLMs assist users in preparing and driving generation. The
current implementation has a Rust core and CLI alongside a Tauri desktop and a
character approval-gate pilot. Those two pilot components are being retired by
[#12–#15](https://github.com/eemaldi/genaimage/issues/2), not extended as the
future architecture. This decision does not itself remove existing code or
relax its currently enforced checks.

The harness must keep generation choices inspectable, recover from uncertain
network outcomes and preserve provenance without turning model suggestions into
unattended generation.

## Decision

### Components and deployment

- `crates/genaimage-core` remains the shared domain core. It owns workflow
  manifests, explicit parameter binding, ComfyUI transport, the run queue,
  reconciliation, persistence and provenance. The assistant tool runtime and
  its safety boundary are also planned in the core.
- The CLI remains a thin scripted interface. The planned CLI
  calls the same local server API as the portal rather than duplicating domain
  or generation logic; its current direct-core operation remains in place until
  that API exists.
- A planned Rust server component serves the API and embedded, built Svelte
  portal assets from a single binary. The portal is an operator surface, not a
  second orchestration engine. The server listens on loopback only. Its exact
  crate and asset paths are implementation decisions, not existing components.
- ComfyUI and the LLM server remain separate local services. A planned Docker
  Compose setup can run these dependencies locally, with host-published ports
  bound to loopback. GenAImage can run on the host; any Compose networking used
  to reach dependencies must respect the endpoint policy below. Neither Compose
  nor the harness silently installs or substitutes models, nodes or settings.

### Assistant and extension boundaries

A planned LLM provider abstraction supports OpenAI-compatible local endpoints,
including Ollama, llama.cpp and LM Studio when configured with compatible APIs.
The configured endpoint and model are explicit. Traits define extension points
for LLM providers, ComfyUI transport and assistant tools, so implementations can
be tested with fakes and extended without moving orchestration into an operator
surface. These are planned interfaces, not a promise of arbitrary dynamically
loaded plugins or additional providers today.

LLM output is a proposal that the user can edit and must explicitly confirm.
The assistant tool runtime must not submit a run without that confirmation;
model output or a tool call alone is not consent. The submitted workflow and
all effective bindings must be visible to the user. Never silently substitute a
model, node, seed, image, prompt or setting. An ambiguous prompt submission is
reconciled against ComfyUI queue/history and never blindly retried. Preserve
previously written run records and create new versions instead of overwriting.

### Endpoint policy

The server binds to loopback only, independently of outbound service settings.
ComfyUI and LLM endpoints default to loopback. A non-loopback private address
or Compose service hostname may be used only when explicitly allow-listed in
local configuration; public endpoints are not permitted. Allow-listing is for
specific intended hosts, not blanket access to all private networks. Validate
resolved addresses and redirects against the policy so hostnames cannot turn
an allowed local endpoint into an unexpected public connection. Compose host
names need both explicit allow-listing and local network reachability; the
allow-list does not expose the GenAImage server beyond loopback. This broader
outbound policy is planned; current ComfyUI profiles accept loopback only.

### Performance budgets

The target budgets, not claims about the current implementation, are:

| Measure | Budget |
| --- | --- |
| Cold start | < 150 ms |
| Idle RSS | < 40 MB |
| Binary size | < 25 MB |
| Portal initial JavaScript | < 150 KB gzip |
| p95 non-ComfyUI API latency | < 20 ms |

## Consequences

- One shared core and a loopback server give the CLI and portal consistent
  validation, queue state and provenance, while keeping the portal deployable
  with the binary and the interface lightweight.
- Replacing the Tauri desktop and retiring character gates require separate
  migration work; existing code and checks remain until those changes land.
- Local providers and private/Compose hosts add configuration, endpoint
  validation and confirmation responsibilities. They do not authorise public
  binding, cloud services or unattended generation.
- Embedded assets and abstractions must be measured against the budgets as they
  are implemented; performance is a design constraint rather than an already
  verified result.
