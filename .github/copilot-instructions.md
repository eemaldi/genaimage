# Instructions for GitHub Copilot

Read the repository-root [AGENTS.md](../AGENTS.md) before making changes. It
contains the shared GenAImage project direction and repository conventions.

GenAImage is a local ComfyUI harness with a shared Rust core, a Rust CLI, and a
Tauri/React desktop. ComfyUI remains an external local process. Use English
(UK) for documentation and user-facing text. Keep ComfyUI transport, workflow
orchestration, gates, persistence, and provenance in `genaimage-core`; do not
duplicate them in the CLI or frontend.

Run the repository CI checks locally with
`python3 scripts/ci_check.py`. Keep [AGENTS.md](../AGENTS.md) and this file
aligned when project tooling changes.

Use strict workflow manifests and API-format ComfyUI JSON. Never silently
substitute a model, node, seed, image, prompt, or setting. A successful technical
run does not approve creative output or advance a character gate. Prompt
submission with an ambiguous network result must be reconciled rather than
blindly retried.

Project-local skills are installed under `.agents/skills/` for GitHub Copilot
and mirrored unchanged under `.claude/skills/` for Claude Code. Follow the
shared Agent Skills format and keep the two copies aligned.
