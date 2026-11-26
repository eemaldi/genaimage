# ADR 0002: ComfyUI container image for Docker Compose

- **Status:** Accepted

## Context

The Compose stack (`compose.yaml`) needs a ComfyUI image that runs on CPU, NVIDIA
and AMD ROCm hardware. ComfyUI, its models and custom nodes stay external to
GenAImage ([ADR 0001](0001-target-architecture.md)); the stack must not silently
install or substitute them. Upstream ComfyUI publishes source tags, not an
official container image.

Options considered:

- **Community images** (for example `yanwk/comfyui-boot`, `ai-dock/comfyui`,
  `mmartial/comfyui-nvidia-docker`): convenient, but each is maintained by an
  individual or a small team, usually targets a single accelerator, tracks
  moving tags, often bundles ComfyUI-Manager and other custom nodes, or
  downloads models and nodes at start-up, and commonly runs as root. That is
  weak provenance and conflicts with the no-silent-installation guardrail.
- **A thin Dockerfile from a pinned upstream tag** (`docker/comfyui/Dockerfile`):
  we control exactly what is inside, one file covers every accelerator through a
  PyTorch wheel index, and every input can be pinned.

## Decision

Build ComfyUI from a thin Dockerfile in `docker/comfyui/`:

- Base `python:3.12-slim-bookworm`, pinned by digest.
- ComfyUI `v0.46.0` from `Comfy-Org/ComfyUI`, pinned by tag and verified against
  the full commit SHA (`COMFYUI_COMMIT`) at build time; the `.git` directory is
  removed.
- `torch==2.14.1` and `torchvision==0.29.1` from the PyTorch wheel index of the
  chosen accelerator (`cpu`, `cu130`, `rocm7.2`), then ComfyUI's
  `requirements.txt` from the same tag. One build argument (`TORCH_INDEX`)
  selects the accelerator.
- Runs as uid 65532 with no custom nodes, no ComfyUI-Manager and no models.
  Models, custom nodes, input, output and user data are named volumes.
- The GPU variants set `COMFYUI_REQUIRE_GPU=1`; the entrypoint then exits
  non-zero if PyTorch sees no GPU, so a GPU stack never falls back to the CPU
  silently.
- The Ollama image is the official `ollama/ollama`, pinned by digest (the `-rocm`
  tag for AMD).

Out of scope: publishing the ComfyUI image to a registry, installing custom
nodes, downloading models (see the model bootstrap work) and multi-architecture
GPU builds (the stack targets `linux/amd64`).

## Consequences

- Provenance is limited to the upstream ComfyUI repository, PyTorch, Debian,
  Python and Ollama, all pinned. Upgrades are explicit edits to build arguments
  and digests, reviewed in a pull request.
- The first `docker compose up` builds the ComfyUI image locally, which downloads
  several hundred megabytes (CPU) to several gigabytes (CUDA, ROCm). There is no
  prebuilt image yet.
- The newer PyTorch wheels need an up-to-date NVIDIA driver (CUDA 13: 580 or
  later) or ROCm 7.2 support for the card; users with older drivers override
  `TORCH_INDEX` and the torch versions.
- Pinned wheels and digests age; a routine update is needed for security fixes.
