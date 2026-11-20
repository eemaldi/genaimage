# GenAImage

GenAImage is a framework for making AI image generation more controllable and
less generic through explicit pipelines, workflows, bounded agents, and reusable
skills.

## Use the project agent

In GitHub Copilot Chat, select the **GenAImage** agent to start with any project
capability. It routes requests to the focused project skills and workflows;
each capability's instructions and user approval requirements remain
authoritative.

## Character concept art pilot

The first documented workflow is a human-approved character concept art
pipeline. It is designed to turn a character description and references into a
traceable concept package, not to generate images immediately.

Read the [workflow](workflows/character-concept-art/WORKFLOW.md) before starting.
Use the templates in [templates/character-concept-art/](templates/character-concept-art/)
to collect the input, record design decisions, approve the visual brief, and
evaluate revisions.

### Start a character

1. Create a folder at `characters/<character-id>/` using an ID such as
   `CHR-001`.
2. Copy the production record and input package templates into that folder.
3. Collect the description and references. Do not generate an image yet.
4. Complete Gate 1 and wait for the user's explicit approval before analysing
   the design further.
5. Continue one phase at a time, recording every decision and approval.

The workflow is currently a documented, human-operated process. It does not
include an image-generation integration or automatically enforce approvals.

## Project conventions

- Keep pipeline steps, workflow orchestration, bounded agent decisions, and
  reusable skills as distinct responsibilities.
- Make generation choices and intermediate results inspectable.
- Never infer approval from silence or proceed past an unapproved gate.
- Preserve approved outputs; create a new version instead of overwriting one.
- Write project documentation and user-facing text in English (UK).

## CI

GitHub Actions runs repository checks on pushes and pull requests. Run the same
checks locally with:

```text
python3 scripts/ci_check.py
```
