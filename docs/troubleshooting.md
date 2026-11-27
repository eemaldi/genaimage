# Troubleshooting

## Doctor checks

`genaimage doctor` (also `GET /api/v1/doctor`) reports one check per id below.
Each check that is not `ok` carries a `fix`; `skipped` means the check was not
run because an optional service is not configured or a check it depends on
failed. Fix the first failing check and run `doctor` again.

| Id | What it checks | Typical fix |
| --- | --- | --- |
| `server` | The GenAImage server answers. | `genaimage serve`; pass `--server` or set `GENAIMAGE_SERVER` for another address. |
| `config` | Endpoint settings are valid and no saved setting awaits a restart. | Correct the setting on the Settings page, then restart `genaimage serve`. |
| `comfyui.reachable` | ComfyUI answers `/system_stats` within two seconds. | Start ComfyUI and check `comfyui.base_url`. |
| `comfyui.version` | ComfyUI reports an exact `major.minor.patch` release of 0.3.57 or later (single-job cancellation); pre-release or four-part versions are unrecognised and warn. | Update ComfyUI. |
| `comfyui.object_info` | `/object_info` returns the node classes. | Check the ComfyUI console for failing custom nodes; wait for it to finish loading. |
| `workflows.nodes` | Every node `class_type` of each workflow exists in ComfyUI. Output lists up to ten missing classes per workflow. Workflows that cannot be loaded, or lie beyond the first 200, are counted and warned about, never skipped silently. | Install the custom nodes yourself, restart ComfyUI. |
| `packs.requirements` | Every pack that declares requirements has its node classes and models in ComfyUI, using the same function as `genaimage packs validate`. Missing items are listed per pack (`error` when every such pack is affected, otherwise `warning`). A model folder GenAImage cannot check, and pack discovery problems, are a `warning`, never `ok`. Skipped when `/object_info` is unknown. | Install the missing nodes and models in ComfyUI yourself; repair or remove broken packs ([packs](packs.md)). |
| `llm.reachable` | The model listing answers. Skipped if `llm.base_url` is empty (the assistant is optional). | Start the LLM server; check `llm.base_url` and `llm.api_key`. |
| `llm.text` | `llm.text_model` is served and supports tools. | Set `llm.text_model`; declare `tools = true` under `[llm.capabilities."<model>"]` if needed. |
| `llm.vision` | `llm.vision_model` is served and accepts images. Skipped if unset. | Set `llm.vision_model`; declare `vision = true` under `[llm.capabilities."<model>"]` if needed. |
| `gpu` | ComfyUI reports a GPU device in `/system_stats`. A CPU-only ComfyUI, such as the default Compose stack, is a warning, not an error. | Install a GPU build of PyTorch and do not start ComfyUI with `--cpu`; under Compose pick an accelerator ([Compose](compose.md#accelerators)). |
| `disk` | Free space for `paths.runs`: warning under 5 GiB, error under 512 MiB. | Free space or move `paths.runs` to a larger disk. |
| `ports` | The server, ComfyUI and the LLM use different addresses, and the ComfyUI address answers like ComfyUI. | Give each service its own port. |

The portal shows the same report on the Diagnostics page (`#/diagnostics`).
Doctor only contacts the endpoints in your configuration and never prints the
LLM API key.

The sections below cover the most common causes. Exit code `6` from `doctor`
means at least one check is an `error`; warnings and skipped checks exit `0`.

## `doctor` cannot connect

When the server is not running, `doctor` still prints a report: `server` is an
error and every other check is skipped. If `ports` is also an error, another
program owns the server port. Find it (for example `lsof -i :3000` or
`netstat -ano`), stop it, or choose another `server.port` and pass `--server`.

Confirm that ComfyUI is running independently and that `genaimage.toml` points
to its HTTP URL. If ComfyUI's port changed, update `[comfyui]`.
Loopback hosts are allowed; private or Compose hosts must be explicitly listed
in `comfyui.allowed_hosts` and resolve only to private or loopback addresses.

## Workflow is identified as a UI export

The file contains a ComfyUI editor graph (`nodes` and `links`) rather than its
API graph. In ComfyUI developer mode, use the API-format export and keep the
result unmodified beside the GenAImage manifest.

## A node type is unavailable

The local `/object_info` response does not list the `class_type` named in the
workflow. Install/configure the needed custom node in ComfyUI separately, then
restart ComfyUI and retry validation. The harness never installs a node for you.

## A required parameter is missing

Run:

```bash
genaimage workflow inspect <ID> --json
```

Provide every parameter marked required through a preset or `--set NAME=JSON`.
The CLI displays the targeted node/input, type, default, and allowed range.

## A run is `unknown_submission`

Do not resubmit immediately. The HTTP request may have reached ComfyUI even
though the harness did not receive a response:

```bash
genaimage jobs reconcile <run-id>
genaimage jobs show <run-id>
```

Check the reconciliation `outcome`, not just the run status:

- `active`: the prompt was found pending or running. Keep tracking the original run.
- `terminal`: the run has ended. Check its status and outputs.
- `in_progress_elsewhere`: the original submission is still in progress. Wait before checking again.
- `still_ambiguous`: the evidence is inconclusive. If `settles_at` is shown,
  wait until then before reconciling again; otherwise investigate ComfyUI.
- `not_found`: the prompt was absent from queue and history after the settle
  window. ComfyUI cannot prove absence, so very late admission is still
  possible and another submission could cause duplicate generation. The original record stays
  `unknown_submission`. If you decide to submit again, review the parameters
  and confirm a separate run in Jobs when server-side rerun is available.
  GenAImage does not retry automatically.

## A run is stuck after restart

Use `jobs reconcile` to compare persisted prompt/client IDs with ComfyUI queue
and history. A WebSocket failure is recoverable while HTTP reconciliation
remains available, and the event is kept in the run history.

## A legacy workflow is missing from the list

`workflow list` shows discovery diagnostics with the catalogue and still
lists valid workflows. Upgrade a version 1 sidecar as described in the
[manifest guide](workflow-manifest.md#upgrading-from-version-1), then run
`workflow validate <ID>` again. If an invalid sidecar shares an ID with a
valid one, repair it or move it out of the server’s workflow directory before
using that ID. See the [CLI reference](cli.md).

The obsolete `paths.characters` setting is rejected. Remove it; this does not
change the state database or existing run manifests. See the [setup guide](setup.md)
for migrating old profiles and data paths.

## HTTP 421 (misdirected request)

The server accepts only a loopback `Host` that carries its real listener port.
Opening it through a remapped port, a reverse proxy or a non-loopback name
returns 421. Under Compose the host and container ports must be equal; set
`GENAIMAGE_PORT` (it sets both sides) instead of remapping `8190:<other>`, and
publish on `127.0.0.1` only. See [setup](setup.md#container).

## Compose and ROCm

- **ComfyUI exits with `COMFYUI_REQUIRE_GPU=1 but torch sees no GPU`.** The GPU
  variants never fall back to the CPU. Check `docker compose logs comfyui`, the
  `COMPOSE_FILE` line in `.env` and that `/dev/kfd` and `/dev/dri` exist (AMD)
  or the NVIDIA Container Toolkit is installed.
- **`libatomic` errors in the ComfyUI container.** The repository's ComfyUI
  image installs `libatomic1`; rebuild with `docker compose build comfyui`
  rather than using an older or third-party image.
- **ROCm permission errors on `/dev/kfd`.** Set `VIDEO_GID` and `RENDER_GID` in
  `.env` to the numeric IDs from `getent group video render`; the defaults (44
  and 992) are not the same on every host.
- **Two GPUs (an integrated and a discrete card).** ComfyUI uses `cuda:0`, and
  the `gpu` check lists both devices. Compare `vram_total` in
  `curl -s http://127.0.0.1:8188/system_stats`. To hide one, pass
  `HIP_VISIBLE_DEVICES` through your own override file; Compose does not
  forward `.env` values into containers by itself.
- **A model variable stays empty or cannot be changed.** An empty
  `GENAIMAGE_LLM_TEXT_MODEL=` in `.env` is still "set", which locks the setting
  to empty and makes it read-only in the portal. Remove the line or comment it
  out instead, then `docker compose up -d --wait genaimage`.
- **The portal says a model or checkpoint is missing.** Nothing is downloaded
  for you. See [models](models.md) and the Models page (`#/models`).
- **A saved setting has no effect.** Most settings apply after a restart; see
  the [configuration reference](configuration.md#keys).
