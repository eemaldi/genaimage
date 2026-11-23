# GenAImage

GenAImage is a thin, fast, local glue layer and harness for controllable
ComfyUI image generation, assisted by local LLMs. Read
[ADR 0001](docs/adr/0001-target-architecture.md) and the
[architecture](docs/architecture.md) before making architectural changes.

## Architecture

- `crates/genaimage-core` owns configuration, workflow manifests and explicit
  binding, run orchestration, persistence and provenance. The assistant tool
  runtime and provider abstractions are planned in the core; do not duplicate
  orchestration in operator surfaces.
- `crates/genaimage-comfyui` implements the core transport port using ComfyUI
  HTTP and WebSocket connections.
- `crates/genaimage` is the scripted interface. Today it calls the adapter and
  the core directly; the target is a thin client of the same local API as the
  portal. `crates/genaimage-api` and `crates/genaimage-server` are placeholders.
- The planned Rust server serves its API and embedded Svelte portal from one
  loopback-bound binary. ComfyUI and the OpenAI-compatible local LLM service
  remain separate processes. Docker Compose support is planned for local
  dependencies, not public exposure.
- Outbound service endpoints default to loopback. The target also allows
  explicitly allow-listed private/Compose hosts, never public endpoints; this
  is planned and current ComfyUI profiles remain loopback-only. The GenAImage
  server itself must bind to loopback only.
- Use ComfyUI API-format JSON and strict `*.genaimage.json` workflow manifests.
  ComfyUI, models, weights and custom nodes are external; do not silently
  install, replace or substitute them.

## Generation and assistant guardrails

- Keep generation choices explicit and inspectable. Never silently substitute a
  model, node, seed, image, prompt or setting; never represent a failed or
  unknown operation as a successful run.
- LLM output is a proposal the user can edit and must explicitly confirm. The
  assistant never submits a run without that confirmation. A successful
  technical run is not creative approval.
- Reconcile an ambiguous prompt submission against ComfyUI queue/history;
  never blindly retry it.
- Preserve source workflows and previously written run/production records.
  Create new versions instead of overwriting them.
- Keep the harness thin and fast. The target budgets are cold start < 150 ms,
  idle RSS < 40 MB, binary < 25 MB, portal initial JS < 150 KB gzip and p95
  non-ComfyUI API latency < 20 ms.

## Project-local agent skills

Install reusable skills in this project, never globally. Keep the GitHub Copilot
copies in `.agents/skills/<skill-name>/SKILL.md` and the Claude Code copies in
`.claude/skills/<skill-name>/SKILL.md` identical. Use the shared Agent Skills
format with standard `name` and `description` frontmatter; do not rely on
client-specific metadata or tools unless a skill explicitly declares that
dependency.

## Working in this repository

- Write documentation and user-facing text in English (UK).
- Check existing implementation and documentation before deciding how a feature
  fits the architecture. Clearly mark planned behaviour as not yet implemented.
- Prefer focused Rust tests against fake ComfyUI and LLM servers; no real models
  are required in CI. Keep real-model tests opt-in and explicit.
- Keep CLI output JSON-stable and errors actionable. Keep Copilot- and
  Claude-specific entry points aligned with this guidance.
- Run the repository CI checks locally with `python3 scripts/ci_check.py`.
