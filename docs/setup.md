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

## 4. Desktop on Linux

Tauri 2 needs the current WebKit 4.1 and GTK development libraries. On
Ubuntu 22.04 or newer:

```bash
sudo apt update
sudo apt install \
  build-essential \
  pkg-config \
  libdbus-1-dev \
  libssl-dev \
  libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev
```

Ubuntu 20.04's WebKit 4.0 ABI is not sufficient for this Tauri 2 target. Use a
newer operating system, the CI container, or a supported Linux distribution.

Install frontend dependencies and launch:

```bash
cd apps/desktop
npm ci
npm run desktop:dev
```

On first launch, select the repository/project root in **Settings**. ComfyUI
still runs separately.

## 5. Verification

The repository's canonical validation command is:

```bash
python3 scripts/ci_check.py
```

It expects the Rust, Node, Python, and Tauri system prerequisites above.
