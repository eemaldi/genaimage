# GenAImage

GenAImage aims to make AI image generation more controllable and less generic by
organising generation around pipelines, workflows, agents, and skills.

## Architecture direction

- **Pipelines** define explicit, composable image-generation steps and their
  inputs and outputs.
- **Workflows** coordinate pipelines and other capabilities to complete a
  broader generation task.
- **Agents** make bounded decisions or guide iteration within a workflow;
  keep orchestration and generation steps outside the agent where possible.
- **Skills** package reusable image-making procedures and domain knowledge.

Keep these responsibilities distinct as implementation is introduced. Make
generation choices and intermediate results inspectable so users can understand
and control how an image was produced. Treat this as the intended direction;
derive implementation details from the code and docs as they appear.

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
before Gate 5 approval. This repository currently documents the workflow only;
do not imply that image generation or automatic gate enforcement is configured.

## Working in this repository

- Write documentation and user-facing text in English (UK).
- Check the existing implementation and documentation before deciding how a
  feature fits the architecture above.
- Keep generation behaviour explicit and user-controllable; avoid hiding
  important model, workflow, or pipeline choices in implicit prompt-only
  behaviour.
- Keep Copilot- and Claude-specific entry-point files aligned with this shared
  guidance when it changes.
