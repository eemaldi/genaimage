# GenAImage

GenAImage is a thin, fast, local glue layer and harness for controllable
ComfyUI image generation, assisted by local LLMs. Read
[ADR 0001](docs/adr/0001-target-architecture.md) and the
[architecture](docs/architecture.md) before making architectural changes.

## Architecture

- `crates/genaimage-core` owns configuration, workflow manifests and explicit
  binding, run orchestration, persistence and provenance. It also owns the
  assistant: LLM model roles (`llm_roles.rs`), conversations, prompt assist,
  workflow suggestion, vision review and the tool runtime with its confirmation
  protocol (`assistant/`). Do not duplicate orchestration in operator surfaces.
- `crates/genaimage-llm` implements the OpenAI-compatible `LlmProvider` and the
  Ollama-native model list and pull client.
- `crates/genaimage-webhook` implements the webhook `EventSink`; the JSONL
  sink, sink queues and status live in `genaimage-core` (`sinks.rs`).
- `crates/genaimage-comfyui` implements the core transport port using ComfyUI
  HTTP and WebSocket connections.
- `crates/genaimage` is the scripted interface. Health, workflow, preset and
  settings, run and job commands call the server through `crates/genaimage-api`.
  The isolated `serve` module wires the core and adapter to the local Axum
  server in `crates/genaimage-server`; the local `config init` module uses only
  core configuration and error types to write an example configuration.
- The server binds to loopback only; inside a container with
  `GENAIMAGE_CONTAINER=1` it may bind `0.0.0.0` or `::`, and ports must be
  published on `127.0.0.1` only (`Dockerfile`, [setup](docs/setup.md#container)).
  It currently serves health, doctor, workflows, runs, batches (pinned runs
  over a seed list or value grid), outputs and their vision-review annotations,
  uploads, settings, setup status and detection, LLM model pulls and
  checkpoints, live events, the assistant API, an OpenAPI contract and the embedded Svelte portal in `apps/portal`:
  dashboard, generate, jobs, gallery, workflows, run detail, batch mode and batch view, models,
  diagnostics, settings, the setup wizard, and an assistant chat panel
  available on every page. ComfyUI and the OpenAI-compatible local LLM service
  (Ollama for model pulls) remain separate processes; `compose.yaml` runs all
  three for local use only ([Compose](docs/compose.md)).
- Outbound service endpoints default to loopback. Explicitly allow-listed
  private or Compose hosts are permitted, never public endpoints.
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
  - An assistant tool that requires confirmation runs only after a confirm or
    edit decision bound to the `preview_token` of the preview the user saw.
    The confirmation ID is the run's idempotency key. An interrupted
    invocation settles as `outcome_unknown` and is never re-invoked. Each
    decision re-runs the tool's `preview`, so it must be deterministic and
    free of side effects.
  - Validate assistant proposals against the exact workflow revision the model
    was shown. Drop invalid items with a note; never coerce them. The
    assistant never chooses image inputs.
  - Never fall back between the text and vision model roles.
- Reconcile an ambiguous prompt submission against ComfyUI queue/history;
  never blindly retry it.
- Preserve source workflows and previously written run/production records.
  Create new versions instead of overwriting them. When an operation fails
  after it has written something, say what was written, so that a client does
  not retry into a duplicate.
- Operator surfaces, including the portal and the CLI, follow these rules:
  - Never rebuild a submission from current workflow data. Pin it to the
    reviewed `workflow_revision`, and use the server's verified
    `POST /api/v1/runs/{id}/rerun` for identical re-runs.
  - Generate one idempotency key per reviewed request and keep it until the
    outcome is definitive. Retry with the same key, never a new one.
  - Show a run as successful only once the backend has confirmed its outputs.
  - Keep numbers exact. Do not round large seeds, and do not turn `2.0` into
    `2` or `-0.0` into `0`. Validation compares numbers exactly too, never
    through `f64`.
  - Show every submitted value before submission, including inherited workflow
    inputs.
- The LLM API key is write-only. Never return, log or print it, including in
  `Debug` output and error messages.
- Keep the harness thin and fast. The target budgets are cold start < 150 ms,
  idle RSS < 40 MB, binary < 25 MB, portal initial JS < 150 KB gzip and p95
  non-ComfyUI API latency < 20 ms. Measure the binary-size impact of a new
  runtime dependency before building on it. The CI performance job is
  informational and never fails; read its binary-size row after each merge.

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
  are required in CI. Keep real-model tests opt-in and explicit. See
  [testing](docs/testing.md) for the assistant regression scenarios and the
  live LLM tests, which run only with `GENAIMAGE_LIVE_LLM=1`.
- Keep CLI output JSON-stable and errors actionable. Keep Copilot- and
  Claude-specific entry points aligned with this guidance.
- Regenerate the API contract with `cargo run -p genaimage-server --no-default-features --example openapi`.
  The example needs no portal build; CI checks the generated portal types.
- Before default Cargo builds, run `npm ci && npm run build` in `apps/portal`.
  Use `--no-default-features` for API-only builds.
- Run the repository CI checks locally with `TMPDIR=/tmp python3 scripts/ci_check.py`;
  a long `TMPDIR` breaks a unix-socket test. `scripts/generate_portal_types.py`
  needs `npm ci` in `apps/portal` first.
- Pin container base images by multi-arch index digest, never a
  single-platform manifest. Check Compose changes against the resolved
  configuration (`docker compose -f compose.yaml config --format json |
  python3 scripts/check_compose_config.py`), because Compose merges `ports`
  across files.
- Diagnostics and setup checks call the runtime's own domain functions; a
  looser copy hides failures as `ok`. A feature that adds files to an existing
  concept (for example pack templates) extends the shared validator, such as
  `pack_problems`, so validate, doctor and listings agree.
- When rebasing, regenerate `docs/api/openapi.json` and
  `apps/portal/src/lib/api/schema.d.ts`; never merge them by hand.
- To supervise an epic through subagents, use the `epic-supervision` skill.
