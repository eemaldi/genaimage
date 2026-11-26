# Instructions for GitHub Copilot

Read the repository-root [AGENTS.md](../AGENTS.md) and
[ADR 0001](../docs/adr/0001-target-architecture.md) before making changes.
They contain the shared project direction, guardrails and conventions.

GenAImage is a thin, fast local harness with a shared Rust core and CLI. The
loopback server currently serves health, workflows, runs, outputs and their
vision-review annotations, uploads, settings, live events, the assistant API,
an OpenAPI contract and the embedded Svelte portal (dashboard, generate, jobs,
gallery, workflows, run detail, diagnostics, settings and an assistant chat panel on every
page). Keep generation orchestration, run state, provenance and the assistant
(model roles, conversations, tool runtime and confirmations) in the core, the
OpenAI-compatible LLM provider in `genaimage-llm`, and ComfyUI
transport in its adapter. CLI client commands use `genaimage-api`; only the isolated
`serve` module wires the core, adapter and server. Local `config init` uses core
configuration and error types without opening a harness.

Use API-format ComfyUI JSON and strict manifests. Never silently substitute a
model, node, seed, image, prompt or setting. LLM output is a proposal the user
can edit and must explicitly confirm; the assistant never submits a run without
that confirmation. An assistant tool that needs confirmation runs only after a
decision bound to the reviewed `preview_token`; an interrupted invocation is
`outcome_unknown` and never re-invoked. Reconcile ambiguous prompt submissions against ComfyUI
queue/history, never blindly retry them. Preserve run records by creating new
versions instead of overwriting. A technical success is not creative approval.
Operator surfaces must:
- pin each submission to the reviewed `workflow_revision`
- use the server's verified re-run endpoint rather than rebuilding a request
- keep one idempotency key per reviewed request
- show success only once the outputs are confirmed
- keep numbers exact, and compare them exactly (never through `f64`)

Never return, log or print the write-only LLM API key, including in `Debug`
output. See [AGENTS.md](../AGENTS.md) for details.

Everything runs locally; the server binds to loopback only (inside a container
with `GENAIMAGE_CONTAINER=1` it may bind `0.0.0.0`; publish ports on `127.0.0.1`
only). Outbound services default to loopback, with explicitly allow-listed
private or Compose hosts as the only exceptions. Keep documentation and
user-facing text in English (UK), and respect the performance budgets in ADR 0001; measure the
binary-size impact of any new runtime dependency.

Regenerate the API contract with `cargo run -p genaimage-server --no-default-features --example openapi`;
CI checks generated portal types. Build `apps/portal` before default Cargo builds
or use `--no-default-features` for API-only builds. Run `python3 scripts/ci_check.py`
locally; [testing](../docs/testing.md) covers the assistant scenarios and the
opt-in live LLM tests. Project-local skills under `.agents/skills/` are mirrored identically
under `.claude/skills/`; keep both
copies aligned. When you rebase, regenerate the OpenAPI contract and the portal
types; never hand-merge them. Use the `epic-supervision` skill to supervise
epics through subagents. Keep this file and [AGENTS.md](../AGENTS.md) aligned when
project direction or tooling changes.
