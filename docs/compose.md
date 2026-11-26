# Docker Compose

`compose.yaml` runs GenAImage, ComfyUI and Ollama together. Everything is local:
every published port is bound to `127.0.0.1`, and the services talk to each
other over the private Compose network. See [setup](setup.md#container) for the
GenAImage image itself and [ADR 0002](adr/0002-comfyui-image.md) for the
ComfyUI image decision.

## Quick start

```sh
docker compose up -d
```

Then open `http://127.0.0.1:8190`. The first run builds the GenAImage and
ComfyUI images from this repository, which takes several minutes and downloads
the PyTorch wheels; there is no prebuilt image yet. This default stack runs
ComfyUI on the CPU, which is slow but needs no GPU.

The stack contains no models. It never downloads checkpoints or LLM models for
you and never picks one silently: until you add a checkpoint and select an LLM
model, the portal reports what is missing.

## Accelerators

| Name | Command | ComfyUI torch index |
| --- | --- | --- |
| CPU (default) | `docker compose up -d` | `cpu` |
| NVIDIA | `docker compose -f compose.yaml -f compose.nvidia.yaml up -d` | `cu130` |
| AMD ROCm | `docker compose -f compose.yaml -f compose.rocm.yaml up -d` | `rocm7.2` |

The accelerators are Compose override files rather than profiles, because the
service must always be named `comfyui` for GenAImage to reach it. To avoid
repeating `-f`, set `COMPOSE_FILE=compose.yaml:compose.nvidia.yaml` (for example
in a `.env` file). Use the same files for `down`, `logs` and `build`.

GPU variants never fall back to the CPU. If the GPU is not available, the
container fails to start (a missing NVIDIA runtime or `/dev/kfd`), or ComfyUI
exits at once with `COMFYUI_REQUIRE_GPU=1 but torch sees no GPU`. Check
`docker compose logs comfyui`.

- **NVIDIA** needs the NVIDIA Container Toolkit and a driver that supports
  CUDA 13 (580 or later). Ollama also receives the GPU.
- **AMD** needs a ROCm 7.2 supported card and the `amdgpu` driver on the host.
  The containers get `/dev/kfd` and `/dev/dri` and join the `video` and `render`
  groups by numeric ID; set `VIDEO_GID` and `RENDER_GID` if they differ from
  `44` and `992` (`getent group video render`). Some cards need extra settings,
  such as `HSA_OVERRIDE_GFX_VERSION`; add them in your own override file.

Older drivers can build another PyTorch variant by overriding the `TORCH_INDEX`,
`TORCH_VERSION` and `TORCHVISION_VERSION` build arguments in `docker/comfyui`.

## Adding models

Models live in named volumes, which Compose prefixes with the project name
(`genaimage_`).

| Volume | In-container path |
| --- | --- |
| `genaimage_comfyui-models` | `/opt/comfyui/models` (checkpoints in `/opt/comfyui/models/checkpoints`) |
| `genaimage_comfyui-custom-nodes` | `/opt/comfyui/custom_nodes` |
| `genaimage_comfyui-input` | `/opt/comfyui/input` |
| `genaimage_comfyui-output` | `/opt/comfyui/output` |
| `genaimage_comfyui-user` | `/opt/comfyui/user` |
| `genaimage_ollama-models` | `/root/.ollama` |
| `genaimage_data` | `/data` (GenAImage state, runs, uploads, settings) |

Copy a checkpoint into the ComfyUI volume:

```sh
docker compose cp ./model.safetensors comfyui:/opt/comfyui/models/checkpoints/
```

ComfyUI lists new files without a restart after a refresh. Pull an Ollama model
yourself, then choose it in the portal settings or set `GENAIMAGE_LLM_TEXT_MODEL`
(and optionally `GENAIMAGE_LLM_VISION_MODEL`) in the environment of
`docker compose up`:

```sh
docker compose exec ollama ollama pull <model>
```

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `GENAIMAGE_PORT` | `8190` | GenAImage host and container port |
| `COMFYUI_PORT` | `8188` | Host port of the ComfyUI interface |
| `OLLAMA_PORT` | `11434` | Host port of the Ollama API |
| `GENAIMAGE_IMAGE` | `genaimage:local` | GenAImage image to run or build |

GenAImage checks the `Host` header against its listener port, so the host and
container ports must be equal; `GENAIMAGE_PORT` sets both. The other two ports
can differ because only the host side changes.

GenAImage receives `GENAIMAGE_COMFYUI_URL=http://comfyui:8188`,
`GENAIMAGE_LLM_URL=http://ollama:11434/v1` and the allow-lists
`GENAIMAGE_COMFYUI_ALLOWED_HOSTS=comfyui` and `GENAIMAGE_LLM_ALLOWED_HOSTS=ollama`.
Compose resolves those names to private bridge addresses, which the outbound
policy accepts; public endpoints stay refused. GenAImage starts after ComfyUI and
Ollama have been created, but does not wait for them to be healthy: ComfyUI can
take a minute to start and GenAImage reports backend status itself, so an
unready backend never keeps the portal away.

## Operating the stack

```sh
docker compose ps                  # health of every service
docker compose logs -f comfyui
docker compose stop                # stop, keep data
docker compose down                # remove containers, keep volumes
docker compose down --volumes      # also delete models, outputs and run data
git pull && docker compose up -d --build   # update
```

Volumes persist across `down` and updates. `down --volumes` deletes everything,
including downloaded models and run records.
