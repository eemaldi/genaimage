# Docker Compose

`compose.yaml` runs GenAImage, ComfyUI and Ollama together. Everything is local:
every published port is bound to `127.0.0.1`, and the services talk to each
other over the private Compose network. See [setup](setup.md#container) for the
GenAImage image itself and [ADR 0002](adr/0002-comfyui-image.md) for the
ComfyUI image decision.

## Quick start

```sh
cp .env.example .env    # edit it: accelerator, group IDs, models
docker compose up -d
```

The default `.env.example` selects the AMD ROCm override; remove its
`COMPOSE_FILE` line for the CPU stack. Then open `http://127.0.0.1:8190`. The first run builds the GenAImage and
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
in `.env`, see `.env.example`). Use the same files for `down`, `logs` and `build`.

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
  See [AMD ROCm (RDNA4)](#amd-rocm-rdna4) for a verified setup.

Older drivers can build another PyTorch variant by overriding the `TORCH_INDEX`,
`TORCH_VERSION` and `TORCHVISION_VERSION` build arguments in `docker/comfyui`.

### AMD ROCm (RDNA4)

Verified on Nobara Linux 44 (kernel 7.3) with a Radeon RX 9070 XT (gfx1201) next
to the Ryzen integrated GPU, using the stock `rocm7.2` PyTorch 2.14.1 wheels and
Ollama `0.40.18-rocm`. No `HSA_OVERRIDE_GFX_VERSION` was needed.

```sh
getent group video render        # video:x:39: and render:x:105: on that machine
# .env: COMPOSE_FILE=compose.yaml:compose.rocm.yaml, VIDEO_GID=39, RENDER_GID=105
docker compose up -d --build --wait
```

Use the numeric group IDs from your host (the user running Docker need not be a
member, because the daemon opens the devices); keep them in `.env`. The ComfyUI image needs `libatomic1`, which the Dockerfile installs.

- **GPU selection.** ComfyUI lists the integrated GPU as a second device
  (`cuda:1`, named after the CPU) and uses `cuda:0`, the discrete card, by
  default. Check `curl -s http://127.0.0.1:8188/system_stats`: the discrete card
  has the larger `vram_total`, and the doctor `gpu` check lists both. Ollama
  drops the unsupported iGPU (`gfx1036`) with a warning and logs
  `library=ROCm compute=gfx1201 ... RX 9070 XT`. To hide a device for both
  services, add `HIP_VISIBLE_DEVICES` (or `ROCR_VISIBLE_DEVICES`) to the
  `environment` of your own override file; GenAImage never sets it for you.
- **Check.** `docker compose exec comfyui python -c "import torch; print(torch.cuda.get_device_name(0))"`
  prints `AMD Radeon Graphics` (the generic name of the RX 9070 XT here).
- **Size.** The ROCm ComfyUI image is about 25 GB on disk.
- **First models and image.** Verified with SDXL base 1.0 and `qwen3:8b`.
  Copy the checkpoint in, check its SHA-256 against the Hugging Face file page,
  then pull the LLM and recreate GenAImage with it as the text model:

  ```sh
  docker compose cp sd_xl_base_1.0.safetensors comfyui:/opt/comfyui/models/checkpoints/
  docker compose exec genaimage genaimage models pull qwen3:8b
  # set GENAIMAGE_LLM_TEXT_MODEL=qwen3:8b in .env, then:
  docker compose up -d --wait genaimage
  docker compose exec genaimage genaimage run text-to-image --wait --json \
    --set 'checkpoint="sd_xl_base_1.0.safetensors"' \
    --set 'positive_prompt="a lighthouse on a rocky coast at sunset, detailed, photograph"' \
    --set 'negative_prompt="blurry, low quality"' --set seed=42 --set width=1024 --set height=1024
  ```

  `.env` keeps the settings for every later `up`. On the RX 9070
  XT the 1024x1024, 20-step run took about 15 s, and `qwen3:8b` loaded all 37
  layers on the GPU. Ollama logs a harmless `rocblaslt ... gfx1201` warning.

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
yourself, then choose it in the portal settings. Alternatively set
`GENAIMAGE_LLM_TEXT_MODEL` (and optionally `GENAIMAGE_LLM_VISION_MODEL`) in `.env`
or the environment of `docker compose up`, but leave them unset rather than empty: an
empty value locks the setting to empty through the environment.

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

Named volumes are initialised from the image on first use (ownership and the
`models`, `custom_nodes`, `input`, `output` and `user` scaffolding). Upgrades do
not refresh that scaffolding in a volume that already exists. Bind mounts
instead of named volumes get none of it: no copy-up and no image ownership, so
make the host directory writable by uid 65532 (ComfyUI) yourself.

## Continuous integration

The `Compose stack` workflow (`.github/workflows/compose.yml`) builds and starts
the CPU stack, checks that every service is healthy and that GenAImage reaches
ComfyUI, validates all three file combinations (the resolved configuration must
publish only on `127.0.0.1` and never use `network_mode`), and checks that the
pinned PyTorch wheels exist on the `cpu`, `cu130` and `rocm7.2` indexes. It runs
only when Compose, Docker or its own scripts change, plus weekly and on demand,
not on every pull request. GPU variants are validated but never started in CI.
