# GenAImage

GenAImage is a thin, fast, local harness for controlled, inspectable ComfyUI
image generation. The target is a loopback server with an embedded web portal,
a thin CLI and local LLM assistance; see [ADR 0001](docs/adr/0001-target-architecture.md)
and the [architecture](docs/architecture.md). The server, portal and LLM
assistance are planned; the existing CLI will become a thin server client.

Today it validates API-format workflows, submits explicit parameter values,
tracks queue state, captures outputs and provenance, and enforces the existing
character concept art approval gates.

ComfyUI remains a separate process. GenAImage does not install, start, bundle,
or replace ComfyUI, models, weights, or custom nodes.

## Current interfaces

The Tauri desktop and character gate pilot are being retired in favour of the
planned portal and general-purpose harness; the following describes the
current release.

- `genaimage` — the Rust command-line application for scripts and reproducible
  operation.
- **GenAImage desktop** — a Tauri 2 application with a React/TypeScript WebView
  for workflow launch, jobs, outputs, gates, and local configuration.

Both interfaces call the same `genaimage-core` crate. Workflow, queue, retry,
gate, SQLite, and provenance logic is not duplicated in the frontend.

## Current scope

- One local ComfyUI host on a loopback URL.
- ComfyUI API-format workflow JSON, never an implicit prompt-only path.
- Strict workflow manifests with typed node bindings, presets, uploads, outputs,
  and required gates.
- Queue inspection, WebSocket progress, queued deletion, active interruption,
  and reconciliation after either process restarts.
- SQLite operational state plus portable, checksummed run manifests.
- Ordered character approvals with an immutable decision history.

Remote/multi-host routing, cloud storage, model installation, and custom-node
management are outside the current scope. The planned outbound endpoint policy
allows explicitly configured private or Compose hosts; the server will still
bind to loopback only.

## Prerequisites

- A working local ComfyUI installation and server, normally at
  `http://127.0.0.1:8188`.
- Rust 1.80 or newer with `cargo`.
- Node.js 20 or newer and npm for the Tauri WebView.
- Python 3.11 or newer for repository checks.
- Tauri 2 Linux system libraries on Linux; see
  [setup](docs/setup.md).

## Quick start

Create project configuration:

```bash
cp genaimage.example.toml genaimage.toml
cargo run -p genaimage-cli -- doctor
```

List and inspect the bundled workflow examples:

```bash
cargo run -p genaimage-cli -- workflow list
cargo run -p genaimage-cli -- workflow validate text-to-image
cargo run -p genaimage-cli -- workflow inspect text-to-image
```

Submit a generic run with every non-default value explicit:

```bash
cargo run -p genaimage-cli -- run text-to-image \
  --preset fast \
  --set checkpoint='"my-local-checkpoint.safetensors"' \
  --set positive_prompt='"an approved, specific visual description"' \
  --version v0.1 \
  --wait
```

Values passed with `--set` are parsed as JSON first and otherwise treated as
strings. Quote JSON strings as shown in the example.

Useful job commands:

```bash
cargo run -p genaimage-cli -- jobs list
cargo run -p genaimage-cli -- jobs show <run-id>
cargo run -p genaimage-cli -- jobs reconcile
cargo run -p genaimage-cli -- jobs wait <run-id>
cargo run -p genaimage-cli -- jobs cancel <run-id>
```

Install the CLI binary:

```bash
cargo install --locked --path crates/genaimage-cli
```

## Character concept workflow (current, being retired)

The existing character workflow remains human-controlled until it is removed:

```bash
genaimage character init CHR-001 --name "Character name"
genaimage character approve CHR-001 \
  --gate gate1 \
  --approver "Human approver" \
  --note "Input is complete"
genaimage character status CHR-001
```

Approve Gate 1 through Gate 3 in order. A generation workflow is blocked before
anything reaches ComfyUI unless the required approvals are present. A multi-view
workflow additionally requires Gate 5. A successful image never advances a
gate.

Read [the complete workflow](workflows/character-concept-art/WORKFLOW.md).
Character templates remain in
[templates/character-concept-art/](templates/character-concept-art/).

## Desktop application (current, being retired)

Install and run the Tauri desktop development build:

```bash
cd apps/desktop
npm ci
npm run desktop:dev
```

Point **Settings → Workspace** at this repository or another GenAImage project.
The desktop uses a loopback ComfyUI profile and calls the Rust core directly; it
does not run an additional HTTP backend.

Create a platform package with:

```bash
cd apps/desktop
npm run desktop:build
```

## Persistent records

- `.genaimage/state.sqlite3` contains rebuildable operational state: run status,
  ComfyUI IDs, events, cancellation, and outputs.
- `characters/<id>/workflow-state.json` contains portable gate state and every
  approval/rejection decision.
- `characters/<id>/production-record.md` contains the readable production
  record. GenAImage updates only its marked gate-state block.
- `runs/` and `characters/<id>/generations/` contain resolved workflows,
  preflight records, checksummed outputs, and `run-manifest.json`.

An ambiguous prompt submission is never automatically replayed. Reconcile the
run against ComfyUI queue/history first, then decide whether a new run is
appropriate.

## Add a workflow

Each workflow has an unmodified ComfyUI API export plus a sidecar manifest ending
in `.genaimage.json`. The manifest maps named harness parameters to node inputs
and declares presets, uploaded references, outputs, generation mode, and
required gates.

See the [workflow manifest reference](docs/workflow-manifest.md) and
[architecture](docs/architecture.md).

## Development

Run all repository checks:

```bash
python3 scripts/ci_check.py
```

The command verifies repository links and skill mirrors, formats and lints the
Rust workspace, runs Rust tests, audits/tests/builds the frontend, and checks the
Tauri backend.

## Project agent and skills

In GitHub Copilot Chat, select the **GenAImage** agent. It routes requests to the
focused project skills and the implemented ComfyUI or character workflow. It
does not imply that a required ComfyUI host, model, node, or approval exists.

Project-local skills remain installed under `.agents/skills/` and mirrored
unchanged under `.claude/skills/`.
