# Instructions for GitHub Copilot

Read the repository-root [AGENTS.md](../AGENTS.md) and
[ADR 0001](../docs/adr/0001-target-architecture.md) before making changes.
They contain the shared project direction, guardrails and conventions.

GenAImage is a thin, fast local harness with a shared Rust core and CLI. The
loopback server currently serves health, workflows, runs, outputs, uploads,
settings, live events, an OpenAPI contract and the embedded Svelte portal
(dashboard, generate, jobs, gallery, workflows, run detail and settings; the
assistant page is planned). Keep generation orchestration, run
state, provenance and the planned assistant runtime in the core, and ComfyUI
transport in its adapter. CLI client commands use `genaimage-api`; only the isolated
`serve` module wires the core, adapter and server. Local `config init` uses core
configuration and error types without opening a harness.

Use API-format ComfyUI JSON and strict manifests. Never silently substitute a
model, node, seed, image, prompt or setting. LLM output is a proposal the user
can edit and must explicitly confirm; the assistant never submits a run without
that confirmation. Reconcile ambiguous prompt submissions against ComfyUI
queue/history, never blindly retry them. Preserve run records by creating new
versions instead of overwriting. A technical success is not creative approval.
Operator surfaces must:
- pin each submission to the reviewed `workflow_revision`
- use the server's verified re-run endpoint rather than rebuilding a request
- keep one idempotency key per reviewed request
- show success only once the outputs are confirmed
- keep numbers exact

See [AGENTS.md](../AGENTS.md) for details.

Everything runs locally; the server binds to loopback only. Outbound services
default to loopback, with explicitly allow-listed private or Compose hosts as
the only exceptions. Keep documentation and user-facing text in
English (UK), and respect the performance budgets in ADR 0001.

Regenerate the API contract with `cargo run -p genaimage-server --no-default-features --example openapi`;
CI checks generated portal types. Build `apps/portal` before default Cargo builds
or use `--no-default-features` for API-only builds. Run `python3 scripts/ci_check.py`
locally. Project-local skills under `.agents/skills/` are mirrored identically
under `.claude/skills/`; keep both
copies aligned. When you rebase, regenerate the OpenAPI contract and the portal
types; never hand-merge them. Use the `epic-supervision` skill to supervise
epics through subagents. Keep this file and [AGENTS.md](../AGENTS.md) aligned when
project direction or tooling changes.
