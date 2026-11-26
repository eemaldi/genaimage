# GenAImage

GenAImage is a thin, fast, local harness for controlled, inspectable ComfyUI
image generation. The target is a loopback server with an embedded web portal,
a thin CLI and local LLM assistance; see [ADR 0001](docs/adr/0001-target-architecture.md)
and the [architecture](docs/architecture.md). The loopback server offers a health
endpoint, workflows, runs, outputs, uploads, settings and a [live event stream](docs/architecture.md#live-event-stream).
The server embeds the Svelte portal: dashboard, generate, jobs, gallery,
workflows, run detail, diagnostics and settings. LLM assistance is still being
built.

Today it validates API-format workflows, submits explicit parameter values,
tracks queue state, and captures outputs and provenance.

ComfyUI remains a separate process. GenAImage does not install, start, bundle,
or replace ComfyUI, models, weights, or custom nodes.

## Current interfaces

`genaimage` is the Rust command-line application for scripts and reproducible
operation. `genaimage serve` starts the local HTTP server. Health, workflow,
preset, settings, run, job and `open` commands use that server.
`genaimage open` opens the server root (the portal in default builds);
`--print` prints its URL. Shell completion scripts do not need a running server.
See the [CLI reference](docs/cli.md) for shell installation, server selection,
image uploads and JSON.

## Current scope

- One local ComfyUI host on a loopback URL.
- ComfyUI API-format workflow JSON, never an implicit prompt-only path.
- Strict workflow manifests with typed node bindings, presets, uploads and outputs.
- Queue inspection, WebSocket progress, queued deletion, active interruption,
  and reconciliation after either process restarts.
- SQLite operational state plus portable, checksummed run manifests.

Remote/multi-host routing, cloud storage, model installation, and custom-node
management are outside the current scope. The planned outbound endpoint policy
allows explicitly configured private or Compose hosts; the server itself
binds to loopback only.

## Prerequisites

- A working local ComfyUI installation and server, normally at
  `http://127.0.0.1:8188`.
- Rust 1.80 or newer with `cargo`.
- Node.js 24 and npm for building the embedded portal from source.
- Python 3.11 or newer for repository checks.

## Quick start

Build the portal, create project configuration, then start the loopback server:

```bash
(cd apps/portal && npm ci && npm run build)
cp genaimage.example.toml genaimage.toml
cargo run -p genaimage -- serve
```

From a separate terminal, check its health:

```bash
cargo run -p genaimage -- doctor
```

Open `http://127.0.0.1:3000/` to view the embedded portal. The server starts
without ComfyUI. Health reports the image backend as `unavailable` until it
responds.

List and inspect the bundled workflow examples:

```bash
cargo run -p genaimage -- workflow list
cargo run -p genaimage -- workflow validate text-to-image --preset fast \
  --set checkpoint='"my-local-checkpoint.safetensors"' \
  --set positive_prompt='"an approved, specific visual description"'
cargo run -p genaimage -- workflow inspect text-to-image
```

Submit a generic run with every non-default value explicit:

```bash
cargo run -p genaimage -- run text-to-image \
  --preset fast \
  --set checkpoint='"my-local-checkpoint.safetensors"' \
  --set positive_prompt='"an approved, specific visual description"' \
  --version v0.1 \
  --wait
```

Values passed with `--set` must be valid JSON. Quote JSON strings as shown
in the example and specify each parameter at most once.

Useful job commands:

```bash
cargo run -p genaimage -- jobs list
cargo run -p genaimage -- jobs show <run-id>
cargo run -p genaimage -- jobs reconcile
cargo run -p genaimage -- jobs wait <run-id>
cargo run -p genaimage -- jobs cancel <run-id>
cargo run -p genaimage -- open --print
cargo run -p genaimage -- completions bash
```

Install the CLI and server binary after building the portal:

```bash
cargo install --locked --path crates/genaimage
```

For an API-only source build without Node.js or portal assets, pass
`--no-default-features` to `cargo run`, `cargo build` or `cargo install`.

## HTTP API

The local API publishes its [OpenAPI 3.1 contract](docs/api/openapi.json) at
`GET /api/v1/openapi.json`. To pin a reviewed workflow, pass its `revision` as
`workflow_revision` on validation and submission. A stale revision returns
HTTP 409 (`urn:genaimage:problem:workflow-changed`); reload and review before
submitting again. Omitting the field leaves the request unpinned. See the
[Workflows API notes](docs/architecture.md#workflows-api) for details.

Regenerate the checked-in contract after API changes:

```bash
cargo run -p genaimage-server --no-default-features --example openapi
```

The routes and contract are registered together; CI checks the generated
contract, its structure and its operations against the real router. Run `npm ci`
in `apps/portal` and `python3 scripts/generate_portal_types.py` from the
repository root to regenerate `apps/portal/src/lib/api/schema.d.ts`. CI checks
the generated file with `python3 scripts/generate_portal_types.py --check`,
using the portal's pinned generator.

## Persistent records

When `XDG_DATA_HOME` is unset, it defaults to `~/.local/share`. See the
[setup guide](docs/setup.md) for overrides and migration of existing data.

- `$XDG_DATA_HOME/genaimage/workspaces/<workspace-hash>/state.sqlite3` contains rebuildable operational state: run status,
  ComfyUI IDs, events, cancellation, and outputs.
- `$XDG_DATA_HOME/genaimage/workspaces/<workspace-hash>/runs/` contains resolved workflows, preflight records, checksummed outputs,
  and `run-manifest.json`.

An ambiguous prompt submission is never automatically replayed. Reconcile the
run against ComfyUI queue/history first, then decide whether a new run is
appropriate.

## Add a workflow

Each workflow has an unmodified ComfyUI API export plus a sidecar manifest ending
in `.genaimage.json`. The manifest maps named harness parameters to node inputs
and declares presets, uploaded references, outputs and generation mode.

See the [workflow manifest reference](docs/workflow-manifest.md) and
[architecture](docs/architecture.md).

## Development

See the [testing guide](docs/testing.md) for focused assistant scenarios,
opt-in local Ollama tests and the repository's test safety invariants.

Run all repository checks:

```bash
python3 scripts/ci_check.py
```

The command verifies repository links and skill mirrors, formats and lints the
Rust workspace, runs Rust tests, and checks the Python skill helpers. It installs
locked portal dependencies, checks generated API types, and runs portal lint,
type-check, tests and the build **before** compiling Rust. The portal steps
require Node.js 24 and npm.

### Portal development

The Svelte 5 portal has responsive navigation, system light/dark themes and
placeholder pages. For hot module replacement (HMR), run the server on the
port configured in Vite's `/api` proxy and open the loopback URL printed by Vite:

```bash
GENAIMAGE_SERVER_PORT=8190 cargo run -p genaimage --no-default-features -- serve
# Separate terminal:
cd apps/portal
npm ci
npm run dev
```

To see the development UI on the GenAImage origin instead, start GenAImage
with `serve --portal-dev-url http://127.0.0.1:5173`. GenAImage still handles
`/api` locally. Its UI proxy forwards HTTP GET/HEAD, not WebSocket upgrades;
refresh manually or open Vite directly for HMR. The flag works without the
embedded portal feature. Routes use URL fragments, such as
`#/generate?workflow=example`. Use **Keyboard shortcuts** (or `?`) to view
shortcuts or turn off character-key shortcuts. See the [portal UI handoff](docs/architecture.md#portal-ui)
for reusable components.

## Project agent and skills

In GitHub Copilot Chat, select the **GenAImage** agent. It routes requests to the
focused project skills and the implemented ComfyUI workflow. It does not imply
that a required ComfyUI host, model or node exists.

Project-local skills remain installed under `.agents/skills/` and mirrored
unchanged under `.claude/skills/`.
