# GenAImage

GenAImage is a local harness for controllable ComfyUI image generation. A shared
Rust core drives an externally running local ComfyUI server through HTTP and
WebSocket, while a Rust CLI and Tauri 2 desktop provide operator surfaces.

## Architecture

- `crates/genaimage-core` owns configuration, ComfyUI transport, workflow
  binding, SQLite run state, cancellation/reconnect, gates, and provenance.
- `crates/genaimage-cli` is the scripted operator interface over the core.
- `apps/desktop/src-tauri` invokes the same core from typed Tauri commands;
  `apps/desktop/src` is the React/TypeScript WebView only.
- ComfyUI remains external. Do not install, start, bundle, or replace ComfyUI,
  models, weights, or custom nodes from this repository.
- Workflows are ComfyUI API-format JSON plus a sidecar `*.genaimage.json`
  manifest. Pipeline/orchestration choices and generation settings must remain
  explicit and inspectable.
- Agents make bounded creative decisions or guide iteration; workflow
  orchestration and generation transport stay in the Rust core.
- Skills package reusable image-making procedures and domain knowledge.

Keep generation logic in the shared core rather than duplicating it in CLI or
frontend code. Never represent a failed/unknown operation as a successful run.

## Project-local agent skills

Install reusable skills in this project, never globally. Keep the GitHub Copilot
copies in `.agents/skills/<skill-name>/SKILL.md` and the Claude Code copies in
`.claude/skills/<skill-name>/SKILL.md` identical. Use the shared Agent Skills
format with standard `name` and `description` frontmatter; do not rely on
client-specific metadata or tools unless a skill explicitly declares that
dependency.

## Repository commands

Run the repository CI checks locally with:

```text
python3 scripts/ci_check.py
```

## Character concept art pilot

The human-in-the-loop character concept art workflow is documented in
`workflows/character-concept-art/WORKFLOW.md`; reusable records are in
`templates/character-concept-art/`. Follow its sequential approval gates
strictly: never generate images before Gate 3 approval or additional views
before Gate 5 approval. The CLI and desktop now enforce these required gates,
but a technical success never approves a design or advances a gate. Gate
decisions require explicit approver and decision-note input.

## Working in this repository

- Write documentation and user-facing text in English (UK).
- Check the existing implementation and documentation before deciding how a
  feature fits the architecture above.
- Prefer Rust tests against the fake ComfyUI server to tests requiring a local
  model installation. Keep the latter opt-in and explicit.
- Keep CLI output JSON-stable and desktop commands typed; errors must explain the
  invalid binding, gate, host, or recovery action.
- Prompt submission is not blindly retried after an ambiguous network result.
  Reconcile ComfyUI queue/history before a new submission.
- Preserve source workflows and previously written run/production records.
  Create a new version instead of overwriting.
- Keep generation behaviour explicit and user-controllable; avoid hiding
  important model, workflow, or pipeline choices in implicit prompt-only
  behaviour.
- Keep Copilot- and Claude-specific entry-point files aligned with this shared
  guidance when it changes.
