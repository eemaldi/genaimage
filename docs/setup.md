# Setup

## 1. ComfyUI

Install and configure ComfyUI separately. Start its server and confirm that the
local HTTP endpoint responds. GenAImage expects a standard local server with:

- HTTP health and object information;
- prompt submission and upload endpoints;
- queue/history endpoints;
- WebSocket events;
- output viewing.

The harness does not add missing custom nodes or install checkpoints. Export
each workflow from ComfyUI in **API format**.

## 2. Configuration

Copy the example:

```bash
cp genaimage.example.toml genaimage.toml
```

Set `profiles.local.base_url` to the loopback URL exposed by ComfyUI. The first
release rejects non-loopback hosts by design.

Environment overrides are available:

```text
GENAIMAGE_PROFILE
GENAIMAGE_COMFYUI_URL
GENAIMAGE_STATE_DATABASE
```

## 3. CLI

Install:

- Rust 1.80 or newer;
- a C toolchain and SQLite build support when a bundled SQLite cannot be used.

Check the project without submitting:

```bash
cargo run -p genaimage-cli -- workflow list
cargo run -p genaimage-cli -- workflow validate text-to-image
```

Check the actual local ComfyUI connection:

```bash
cargo run -p genaimage-cli -- doctor
```

## 4. Verification

The repository's canonical validation command is:

```bash
python3 scripts/ci_check.py
```

It expects Python 3.11 or newer and the Rust toolchain with rustfmt and clippy.
