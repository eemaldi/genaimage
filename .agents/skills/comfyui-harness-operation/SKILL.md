---
name: comfyui-harness-operation
description: Operate the local GenAImage CLI safely with explicit workflow bindings, job control, reconciliation and provenance; distinguish the planned local API from current capabilities.
---

# ComfyUI harness operation

Use this skill to operate GenAImage through its current CLI, which calls the
shared Rust core directly. ComfyUI is a separate, already running process; the
harness does not install or replace ComfyUI, models, weights or custom nodes.
Current ComfyUI profiles accept loopback hosts only.

## Prepare a run

1. Read `AGENTS.md` and `docs/workflow-manifest.md`. Use a ComfyUI API-format
   JSON workflow with a strict `*.genaimage.json` sidecar, not a UI export.
2. Inspect the host, available workflows, parameter bindings and presets:

   ```text
   genaimage doctor
   genaimage workflow list
   genaimage workflow validate <workflow-id-or-manifest>
   genaimage workflow inspect <workflow-id-or-manifest>
   genaimage presets <workflow-id-or-manifest>
   ```

   Use `--workspace <path>` to select a workspace, `--json` for machine-readable
   output, and `genaimage config show` to inspect configuration. The CLI can
   create example configuration with `genaimage config init`; do not use
   `--force` to replace existing configuration without the user's direction.
3. Check required typed parameters, defaults, presets, local image inputs and
   node/input bindings. The CLI's `--set NAME=JSON` overrides the preset; values
   parse as JSON first and otherwise become strings. Confirm the selected
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

## Planned API

ADR 0001 describes a future loopback-only Rust server and local API, a thin CLI
client and a local LLM assistant runtime in the shared core. These are planned,
not current interfaces: do not invent endpoints, commands or availability.
The same explicit binding, confirmation, reconciliation and provenance rules
apply when they are implemented.
