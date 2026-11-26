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
| `comfyui.version` | ComfyUI is 0.3.57 or later (single-job cancellation). | Update ComfyUI. |
| `comfyui.object_info` | `/object_info` returns the node classes. | Check the ComfyUI console for failing custom nodes; wait for it to finish loading. |
| `workflows.nodes` | Every node `class_type` of each workflow exists in ComfyUI. Output lists up to ten missing classes per workflow. | Install the custom nodes yourself, restart ComfyUI. |
| `llm.reachable` | The model listing answers. Skipped if `llm.base_url` is empty (the assistant is optional). | Start the LLM server; check `llm.base_url` and `llm.api_key`. |
| `llm.text` | `llm.text_model` is served and supports tools. | Set `llm.text_model`; declare `tools = true` under `[llm.capabilities."<model>"]` if needed. |
| `llm.vision` | `llm.vision_model` is served and accepts images. Skipped if unset. | Set `llm.vision_model`; declare `vision = true` under `[llm.capabilities."<model>"]` if needed. |
| `gpu` | ComfyUI reports a GPU device in `/system_stats`. | Install a GPU build of PyTorch; do not start ComfyUI with `--cpu`. |
| `disk` | Free space for `paths.runs`: warning under 5 GiB, error under 512 MiB. | Free space or move `paths.runs` to a larger disk. |
| `ports` | The server, ComfyUI and the LLM use different addresses, and the ComfyUI address answers like ComfyUI. | Give each service its own port. |

Doctor only contacts the endpoints in your configuration and never prints the
LLM API key.

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
