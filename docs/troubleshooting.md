# Troubleshooting

## `doctor` cannot connect

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
