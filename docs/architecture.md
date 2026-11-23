# Harness architecture

## Responsibility boundary

GenAImage has one domain core and two operator surfaces:

```text
CLI (Rust)          Tauri commands
     \                  /
      └── genaimage-core
            ├── configuration and profiles
            ├── workflow manifests and binding
            ├── ComfyUI HTTP/WebSocket client
            ├── character gate transitions
            ├── SQLite run repository
            ├── reconciliation and cancellation
            └── immutable output/provenance capture
                         |
                         v
              external local ComfyUI
```

The desktop React application never opens a network connection to ComfyUI.
Tauri invokes `genaimage-core`, and the Rust core remains the source of truth
for all generation and approval rules.

## Runtime sequence

1. Load the project and named loopback ComfyUI profile.
2. Load the ComfyUI API-format workflow and its GenAImage manifest.
3. Validate JSON shape, node IDs, inputs, value types/ranges, output nodes,
   presets, and required gate context.
4. Assert the complete predecessor chain and required gate approvals.
5. Ask ComfyUI for object information and verify every workflow class exists.
6. Upload image inputs, then record each source hash and returned filename.
7. Save the resolved workflow and preflight snapshot under a new run directory.
8. Submit once with a harness-selected prompt ID and client ID.
9. Observe WebSocket progress and reconcile with queue/history state.
10. Download output bytes atomically, compute SHA-256 hashes, and write the run
    manifest.

Prompt submission is deliberately different from safe reads. Queue, history,
health, and download reads retry with backoff. A prompt submission that times
out remains `unknown_submission` and is not replayed automatically.

## State model

The SQLite database is an operational cache of queue and event state. It is
ignored by Git and can be removed for a clean reconnect, provided run manifests
and character state are retained.

Character gate state is not hidden in SQLite. It is stored as project JSON and
mirrored into the existing human-readable production record. This keeps the
approval basis portable and reviewable.

## Gate enforcement

Generation mode provides a default gate requirement:

| Generation mode | Required gate |
| --- | --- |
| `initial_concept` / `correction` | Gate 3 |
| `refinement` | Gate 4 |
| `multi_view` | Gate 5 |

A manifest may add required gates, but it cannot remove the mode-derived gate.
Approval is ordered from Gate 1 through Phase 08. Rejecting an earlier gate
resets later gate state to pending while preserving every decision in the audit
history.

## Desktop security boundary

The current profile must resolve to `localhost`, `127.0.0.1`, or `::1`. Tauri
starts no public web server and grants only the default command capability. The
browser frontend accesses GenAImage through typed Tauri commands and receives
run updates over local Tauri events.
