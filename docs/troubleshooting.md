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
genaimage workflow inspect <workflow>
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

After ComfyUI queue/history proves the first prompt absent, create a new run
only with an explicit decision.

## A run is stuck after restart

Use `jobs reconcile` to compare persisted prompt/client IDs with ComfyUI queue
and history. A WebSocket failure is recoverable while HTTP reconciliation
remains available, and the event is kept in the run history.

## A legacy workflow is missing from the list

`workflow list` warns on stderr about sidecars it could not load and still
lists valid workflows. Upgrade a version 1 sidecar as described in the
[manifest guide](workflow-manifest.md#upgrading-from-version-1), then run
`workflow validate <manifest-path>` again. If an
invalid sidecar shares an ID with a valid one, select an explicit manifest path
rather than using the shared ID.

The obsolete `paths.characters` setting is rejected. Remove it; this does not
change the state database or existing run manifests. See the [setup guide](setup.md)
for migrating old profiles and data paths.
