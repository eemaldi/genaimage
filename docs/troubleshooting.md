# Troubleshooting

## `doctor` cannot connect

Confirm that ComfyUI is running independently and that `genaimage.toml` points
to its loopback HTTP URL. If ComfyUI's port changed, update the
`[profiles.local]` section.

Only `localhost`, `127.0.0.1`, and `::1` are accepted in this release.

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
lists valid workflows. If a sidecar has the retired `required_gates` field,
remove that field and run `workflow validate <manifest-path>` again.

The obsolete `paths.characters` setting in `genaimage.toml` is ignored. You
can remove it without changing the state database or existing run manifests.
