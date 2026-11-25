---
name: comfyui-harness-operation
description: Operate the local GenAImage CLI safely with explicit workflow bindings, job control, reconciliation and provenance; use the portal, its assistant and the local API without bypassing confirmation.
---

# ComfyUI harness operation

Use this skill to operate GenAImage through its current CLI, which calls the
loopback server for health, workflow, preset, settings, run and job commands.
ComfyUI is a separate, already running process; the harness does not install or replace ComfyUI,
models, weights or custom nodes.
Current ComfyUI profiles accept loopback hosts only.

## Prepare a run

1. Read `AGENTS.md` and `docs/workflow-manifest.md`. Use a ComfyUI API-format
   JSON workflow with a strict `*.genaimage.json` sidecar, not a UI export.
2. Start `genaimage serve` if needed. Inspect the host, available workflows,
   parameter bindings and presets (using catalogue IDs):

   ```text
   genaimage doctor
   genaimage workflow list
   genaimage workflow validate <workflow-id>
   genaimage workflow inspect <workflow-id>
   genaimage presets <workflow-id>
   ```

   Select the workspace when starting the server with `--workspace <path>`.
   Use `--server` or `GENAIMAGE_SERVER` to select the API origin, `--json` for
   machine-readable output, and `genaimage config show` to inspect settings.
   Create example configuration with `genaimage config init`; do not use
   `--force` to replace existing configuration without the user's direction.
3. Check required typed parameters, defaults, presets, local image inputs and
   node/input bindings. Use `run --image NAME=PATH` for an explicitly selected
   local image. The CLI's `--set NAME=JSON` overrides the preset; values
   must be valid JSON, with string values quoted and no duplicate names. Confirm the selected
   workflow, preset and all effective choices with the user, including defaults
   and values in the source workflow. Inspection is not a resolved-run preview;
   don't claim one exists. Treat invalid bindings, missing inputs or unavailable
   dependencies as blockers and report them accurately.
4. An LLM's prompt or settings are proposals, not instructions or consent. The
   user can edit them and must explicitly confirm the intended run before the
   assistant submits it. Never silently substitute a model, node, seed, image,
   prompt or setting. Never modify a source workflow or existing run record to
   conceal a problem.

## Submit and monitor

After explicit confirmation, run a validated workflow with the user's choices,
using parameter names declared by its manifest. For example:

```text
genaimage run text-to-image \
  --preset fast \
  --set checkpoint='"my-local-checkpoint.safetensors"' \
  --set positive_prompt='"a specific visual description"' \
  --version v0.1 \
  --wait
```

The `--preset`, `--version` and `--wait` options are optional. Inspect the
record and events using `genaimage jobs list` and `genaimage jobs show <run-id>`;
`genaimage jobs wait <run-id>` waits for a trackable run. Report the actual
status and output evidence, not assumed success. A completed technical run is
not creative approval; the user reviews the result.

## Reconciliation and cancellation

- If submission is `unknown_submission` or otherwise ambiguous, use
  `genaimage jobs reconcile <run-id>` to check ComfyUI queue/history. Never
  blindly resubmit: if the result remains unresolved, report that uncertainty.
  A new submission requires a deliberate user decision and confirmation.
- `genaimage jobs reconcile` without an ID checks active runs, including after a
  restart. Use `genaimage jobs cancel <run-id>` to request cancellation; queued
  prompts are requested for removal and an executing prompt may be interrupted.
  Inspect and reconcile the resulting state rather than assuming success.
- Preserve errors and events; a failed connection or unknown status is not a
  successful run.

## Provenance

Preserve original and resolved workflows, manifest and effective parameters,
source upload paths and hashes, ComfyUI IDs and history, output files and
checksums. Run records and outputs must not overwrite previous files; create new
versions instead. Do not invent missing provenance.

## Portal and API

`genaimage serve` exposes the loopback API (see `docs/api/openapi.json`). It
also serves the embedded portal at `/`, which has dashboard, generate, jobs,
gallery, workflows, run detail and settings pages. Generate shows the fully
resolved values for review and pins the submission to the reviewed
`workflow_revision`. Run detail re-runs a run through the server's
provenance-checked `POST /api/v1/runs/{id}/rerun`, which never alters the
source run.

## Local LLM assistant

The assistant is available in the portal (a chat panel on every page, and
`#/assistant`) and through the API under `/api/v1/assistant/` and
`/api/v1/outputs/{id}/review`. There are no CLI assistant commands; do not
invent them. Configure it in the `[llm]` settings: `base_url`, `text_model`,
`vision_model` and an optional write-only `api_key`. Health reports whether
each role is available and why not.

- Prompt assist (`POST /api/v1/assistant/prompt`) and workflow suggestion
  (`POST /api/v1/assistant/suggest`) return proposals with `applied: false`.
  Nothing changes until the user accepts it.
- In a conversation, `submit_run` pauses with `confirmation_required` and the
  full effective preview. The user decides through
  `POST .../confirmations/{id}` with the shown `preview_token`. An edit needs
  `POST .../confirmations/{id}/preview` first. Never decide on the user's
  behalf, and treat `awaiting_confirmation` as not submitted.
- `outcome_unknown` means the submission must be reconciled through Jobs; it
  is never a success and must not be resubmitted blindly.
- Vision review writes append-only annotations labelled as model opinion. They
  are not provenance and never alter run records.
