# Instructions for GitHub Copilot

Read the repository-root [AGENTS.md](../AGENTS.md) and
[ADR 0001](../docs/adr/0001-target-architecture.md) before making changes.
They contain the shared project direction, guardrails and conventions.

GenAImage is a thin, fast local harness with a shared Rust core and CLI. Its
planned loopback server and embedded Svelte portal are not yet implemented.
Keep generation orchestration, transport, run state, provenance and the
planned assistant runtime in the core.

Use API-format ComfyUI JSON and strict manifests. Never silently substitute a
model, node, seed, image, prompt or setting. LLM output is a proposal the user
can edit and must explicitly confirm; the assistant never submits a run without
that confirmation. Reconcile ambiguous prompt submissions against ComfyUI
queue/history, never blindly retry them. Preserve run records by creating new
versions instead of overwriting. A technical success is not creative approval.

Everything runs locally; the planned server binds to loopback only. Outbound
services default to loopback, with explicitly allow-listed private/Compose hosts
planned as the only exceptions. Keep documentation and user-facing text in
English (UK), and respect the performance budgets in ADR 0001.

Run `python3 scripts/ci_check.py` locally. Project-local skills under
`.agents/skills/` are mirrored identically under `.claude/skills/`; keep both
copies aligned. Keep this file and [AGENTS.md](../AGENTS.md) aligned when
project direction or tooling changes.
