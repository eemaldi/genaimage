# Instructions for GitHub Copilot

Read the repository-root [AGENTS.md](../AGENTS.md) before making changes. It
contains the shared GenAImage project direction and repository conventions.

GenAImage is intended to make AI image generation more controllable and less
generic through composable pipelines, workflows, bounded agents, and reusable
skills. Use English (UK) for documentation and user-facing text. Preserve those
distinctions, and base implementation choices on the actual code and
documentation rather than assuming planned architecture is already implemented.

Run the repository CI checks locally with
`python3 scripts/ci_check.py`. Keep [AGENTS.md](../AGENTS.md) and this file
aligned when project tooling changes.

Project-local skills are installed under `.agents/skills/` for GitHub Copilot
and mirrored unchanged under `.claude/skills/` for Claude Code. Follow the
shared Agent Skills format and keep the two copies aligned.
