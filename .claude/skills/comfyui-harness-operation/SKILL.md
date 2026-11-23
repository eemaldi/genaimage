---
name: comfyui-harness-operation
description: Operate the local GenAImage ComfyUI harness safely, including API workflow validation, explicit parameter binding, queue control, provenance, cancellation, and character approval gates.
---

# ComfyUI harness operation

Use this skill when submitting or controlling image-generation work through the
GenAImage CLI or Tauri desktop. The harness connects to an already running local
ComfyUI instance; it does not install ComfyUI, models, or custom nodes.

## Safety and workflow boundaries

1. Read `AGENTS.md` and the relevant project workflow first.
2. Use only a ComfyUI API-format workflow. If the harness reports a UI export,
   ask the user to export the API format from ComfyUI developer mode.
3. Validate the workflow manifest before submission. Resolve every required
   parameter and do not silently substitute a model, checkpoint, seed, size,
   reference image, or prompt.
4. For character concept work, verify the character state from
   `workflow-state.json`. Never generate before Gate 3 approval or create
   multi-view output before Gate 5 approval.
5. A completed technical run does not approve the design or advance a gate.
   Ask the user to review it and record an explicit decision.

## Preflight

Run:

```text
genaimage doctor
genaimage workflow validate <workflow-id-or-manifest>
genaimage workflow inspect <workflow-id-or-manifest>
genaimage character status <character-id>
```

Treat a missing node type, invalid binding, missing uploaded file, unavailable
checkpoint, disconnected host, or pending/rejected gate as a hard stop. Report
the exact blocker rather than editing the ComfyUI JSON or project record
silently.

## Submit and monitor

Launch a validated run with explicit values:

```text
genaimage run <workflow-id> \
  --preset <preset> \
  --character <character-id> \
  --version <version> \
  --set checkpoint='"checkpoint.safetensors"' \
  --set positive_prompt='"approved prompt"' \
  --wait
```

Use `jobs show <run-id>`, `jobs wait <run-id>`, or the Tauri Jobs screen to
inspect progress. Do not claim success until the run is `completed` and its
output manifest exists.

## Failure and cancellation

- If submission is `unknown_submission`, reconcile through queue/history state
  before any retry. Do not blindly resubmit because the first prompt may exist.
- Cancelling a queued prompt removes that prompt from the queue. Cancelling the
  executing prompt asks ComfyUI to interrupt the active local job.
- Preserve explicit error messages and events. Do not convert a failed WebSocket
  into a successful-looking run; HTTP reconciliation can continue while the
  WebSocket failure remains visible.
- Reconnect with `genaimage jobs reconcile` after either process restarts.

## Provenance

For each completed run, preserve:

- the original and resolved ComfyUI workflows;
- manifest and parameter versions;
- ComfyUI prompt/client IDs;
- source upload paths and SHA-256 hashes when available;
- output files and SHA-256 hashes;
- the raw ComfyUI history snapshot;
- the gate-state snapshot that authorised submission.

Never overwrite an approved generation or its manifest. Create a new semantic
version and link it to the previous version and review.
