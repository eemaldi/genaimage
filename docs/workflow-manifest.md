# Workflow manifest

A harness workflow consists of two files:

1. an unmodified ComfyUI **API-format** JSON workflow;
2. a `*.genaimage.json` manifest owned by GenAImage.

The API workflow is keyed by node ID and each node has a `class_type` and
`inputs` object. A UI export containing top-level `nodes` and `links` is
rejected with a request to export API format.

## Schema

```json
{
  "schema_version": 2,
  "id": "workflow-id",
  "version": "1.0.0",
  "title": "Readable workflow title",
  "workflow": "api-workflow.json",
  "description": "A short description of the workflow.",
  "tags": ["example"],
  "category": "image generation",
  "thumbnail": "thumbnail.png",
  "parameters": {
    "seed": {
      "node": "3",
      "input": "seed",
      "kind": "seed",
      "required": true,
      "default": 42,
      "widget": "number",
      "step": 1,
      "group": "Sampling",
      "advanced": false
    },
    "reference": {
      "node": "12",
      "input": "image",
      "kind": "image_upload",
      "required": false,
      "description": "Optional source reference."
    }
  },
  "presets": {
    "preview": {
      "seed": 42
    }
  },
  "outputs": [
    {
      "node": "9",
      "kind": "image"
    }
  ]
}
```

Manifests can be bundled with requirements in a [workflow pack](packs.md).

The published [JSON Schema](schemas/workflow-manifest.v2.json) defines the
structural format. Unknown fields are rejected, including the retired
`generation_mode` and `required_gates` fields. `id`, `version`, `workflow` and
`schema_version` are required; `id` and `version` must not be blank. Catalogue
discovery excludes the IDs `.` and `..` with a diagnostic because browsers treat
them as URL path segments; choose a different ID. The structural schema and
loader still accept them for direct, non-catalogue use. `title` defaults to an
empty string. Optional `description`, `tags` (a list of strings), `category`
and `thumbnail` (a string reference) are descriptive metadata; they do not
affect generation. Discovery reports broken links, inaccessible directories
and unreadable references as non-fatal diagnostics. An inaccessible directory
or unreadable manifest leaves the catalogue incomplete, and authoritative ID
resolution refuses to proceed until uniqueness can be established. The workflows
API serves a declared
thumbnail only when it is a supported image within the manifest directory; see
[the API description](architecture.md#local-http-server) for restrictions.

### Upgrading from version 1

Version 1 manifests are not migrated automatically. Remove
`generation_mode` and `required_gates`, change `schema_version` to `2`, and
optionally add metadata and UI hints. Validate the result against the schema
and use `genaimage workflow validate <ID>` against the server catalogue (see
the [CLI reference](cli.md)). Previously written run
records remain readable and are not rewritten. For compatibility with the
existing run database, new records still store `generic` in its historical
`generation_mode` column; this does not change binding or generation.

## Parameter fields

| Field | Meaning |
| --- | --- |
| `node` | API workflow node ID as a string |
| `input` | Existing key in that node's `inputs` object |
| `kind` | `string`, `integer`, `number`, `boolean`, `seed`, or `image_upload` |
| `required` | The harness must receive a value or a default |
| `description` | Optional string describing the parameter |
| `default` | Optional explicit default |
| `allowed_values` | Optional JSON value allow-list |
| `minimum` / `maximum` | Optional numeric range |
| `widget` | Optional string naming a suggested control |
| `step` | Optional numeric increment suggested for a control |
| `options` | Optional list of suggested JSON values |
| `group` | Optional string for grouping controls |
| `advanced` | Optional boolean marking a control as advanced |
| `placeholder` | Optional string for a control's placeholder |

UI hints are advisory only: `options` does not restrict accepted values (use
`allowed_values` for that), and `step` does not round or constrain values.
Hints are optional and may be `null`, but when present they must be usable:

| Kind | Supported `widget` values | `step` |
| --- | --- | --- |
| `string` | `text`, `textarea`, `select` | Not supported |
| `image_upload` | `file` | Not supported |
| `integer`, `seed` | `number`, `slider`, `select` | Positive whole number |
| `number` | `number`, `slider`, `select` | Positive number |
| `boolean` | `checkbox`, `select` | Not supported |

An `options` list must be non-empty, and each option must satisfy the kind,
range and `allowed_values` constraints. If a default is present, it must appear
in `options`. Presets and overrides may still select any valid binding, even
one not in `options` or not aligned to `step`.

No two named parameters may bind the same node/input. Unknown preset parameters,
missing nodes/inputs, unsupported JSON types, and invalid ranges fail
preflight. The published schema checks structure, kind-specific default and
option types, widgets and steps, and rejects non-scalar preset values. JSON
Schema draft 7 cannot compare a value to another property in the instance or
look up a preset key in the parameter map. Thus, for example, it cannot check a
default against that parameter's `minimum`, `maximum` or `allowed_values`,
check that options contain the default, or validate a preset value against its
named parameter. Its mathematical `integer` type also permits `1.0`, whereas
Rust binding expects an integer JSON representation within its supported
integer range. Also run `genaimage workflow validate <ID>` with your intended parameters:
the loader enforces these cross-field rules and checks workflow node/input
bindings. The bundled-manifest tests check the schema and the loader.

For `integer` and `seed`, the supported binding range is
`-9223372036854775808` to `18446744073709551615` inclusive. The schema bounds
these kinds' defaults and options; preset values are checked by the loader
because their parameter kinds cannot be resolved by the schema. Schema
consumers need a JSON parser and validator with sufficient integer precision
at these limits. Parameter `minimum` and `maximum` are floating-point range
bounds, not integer values themselves: fractional or wider limits are valid and
do not expand the supported range of integer bindings.

`image_upload` parameters are strings. When using the HTTP API, set the
parameter to the `upload_id` returned by `POST /api/v1/uploads`
(`sha256:<lowercase hex digest>`), not a filesystem path. The core verifies
the stored bytes and uploads them to the configured ComfyUI input area before
prompt submission. Run provenance records the upload ID, SHA-256, sanitised
original filename, byte size and returned ComfyUI filename. To send a local
reference through the CLI, use `run --image NAME=PATH`; `--set` accepts an
already stored upload ID. See the [CLI reference](cli.md#runs-and-jobs).

## Presets

A preset is a named set of parameter values. CLI values override preset
values, and the effective values are recorded in the run manifest. A preset does
not approve content.

## Importing a ComfyUI workflow

Writing a manifest by hand is optional. The server can propose one from a
ComfyUI **API-format** export. This is the API behind the import wizard; the
portal wizard itself is **not yet implemented** (issue #68). The CLI prints a
proposal with `genaimage workflow import <file> [--name NAME]`.

1. `POST /api/v1/workflows/import` with `{"graph": <API-format JSON>, "name": "optional"}`
   (at most 2 MiB). It is read-only and returns a proposal.
2. The user reviews and may edit the proposed manifest.
3. `POST /api/v1/workflows/import/confirm` with
   `{"graph", "graph_sha256", "manifest"}` (at most 4 MiB) writes
   `<paths.workflows>/<id>/api-workflow.json` and then
   `workflow.genaimage.json`. The user workflow root is created if missing.

A **UI-format** export (it has `nodes` and `links` arrays) is refused with a 422
problem of type `urn:genaimage:problem:workflow-ui-format`. In ComfyUI, use
Workflow, then Export (API); in older versions, enable Dev mode in the settings
and use Save (API Format).

### What is recognised

Parameters are created only for these node classes. Defaults are copied exactly
from the graph (large seeds, `2.0` and `-0.0` are not rounded or rewritten), and
a value that fails the parameter's own validation is reported as an unbound
input instead of being corrected.

| Class | Inputs and parameter names |
| --- | --- |
| `CheckpointLoaderSimple` | `ckpt_name` as `checkpoint` |
| `CLIPTextEncode` | `text` (textarea) as `prompt` or `negative_prompt`, by the `KSampler` `positive` or `negative` link that feeds it directly; otherwise `text_<node>` with a warning |
| `KSampler` | `seed` (seed), `steps` (minimum 1), `cfg`, `sampler_name`, `scheduler`, `denoise` (0 to 1) |
| `EmptyLatentImage` | `width`, `height` (16 to 16384, step 8), `batch_size` (1 to 4096) |
| `LoadImage` | `image` as an `image_upload`, required, with no default (the ComfyUI-side file name is not used) |
| `LoraLoader`, `LoraLoaderModelOnly` | `lora_name`, `lora_strength_model`, `lora_strength_clip` (-100 to 100) where present |
| `SaveImage` | an entry in `outputs`, not a parameter |

If several nodes would share a name, every one of them gets a `_<node id>`
suffix (for example `lora_name_2` and `lora_name_3`). Connections
(`[node, output]` inputs) are never bindable. Every other node is listed in
`unknown_nodes`, including standard nodes such as `VAEDecode`. The list means
"nodes without import rules", not "unsupported nodes": such a node keeps its
workflow values and nothing is guessed. The proposal is deterministic: the same
graph and name give the same bytes. The suggested `id` is a slug of `name` (or
`imported-workflow`), made unique against existing workflows. Each proposal and
each confirmation runs a full workflow discovery to check the ID, so both cost
a catalogue scan. `step` is written as a floating-point number, because that is
the manifest's type for it. The graph's node IDs `nodes` and `links` are refused
with 422 `workflow-import-invalid`, because UI-format exports use those keys.

### Confirmation rules

- The graph is normalised (keys sorted, compact JSON, trailing newline) and its
  SHA-256 must equal `graph_sha256`; otherwise the request is refused with 409
  `urn:genaimage:problem:workflow-graph-mismatch`.
- The manifest is validated with the same rules as discovery and returned as
  `errors` (422 `urn:genaimage:problem:workflow-manifest-invalid`), never
  corrected. At most 20 errors are listed, followed by an `and N more` entry.
  The `id` must be a safe slug (`[a-z0-9][a-z0-9_-]{0,63}`), `refresh`
  and `import` are reserved, `workflow` must be `api-workflow.json` and
  `thumbnail` must be absent. A parameter `default` of `null` is refused
  (omit the field instead); discovery refuses it in a manifest file as well.
- Nothing is overwritten. An id already declared by the user root or any pack,
  including an invalid or duplicate claimant, or an existing directory of that
  name, gives 409 `urn:genaimage:problem:workflow-exists` with a `suggested_id`
  field. A graph that does not match `graph_sha256` gives 409
  `workflow-graph-mismatch` with an `expected_graph_sha256` field.
- Files are created with no-clobber hard links of synced temporary files, so
  `paths.workflows` must be on a filesystem that supports hard links (a refusal
  is a 5xx that says so). The graph is published first, then the directory is
  synced, then the manifest, so a crash may leave an undiscoverable directory
  that an identical retry completes. A directory that holds only the identical
  graph, and perhaps stale `.*.tmp` files from this scheme, is completed by
  publishing the manifest; those files are never deleted. Any other content
  gives `workflow-exists`. An identical repeat of a successful confirmation
  returns 200 with the same result.
- If another manifest declares the written ID by the time the catalogue is
  rebuilt, the answer is 409 `urn:genaimage:problem:workflow-ambiguous` with
  `id` and `manifest_path`. The workflow **was written** and stays unavailable
  until the duplicate is removed or renamed. It is not rolled back, and
  confirming it again creates nothing new; it gives the same 409 until the
  duplicate is gone, then 200.
- The response is `{id, revision, manifest_path, source: "user"}`. The catalogue
  is rebuilt first, so the workflow is listed immediately.

## Outputs and manifests

The `outputs` declaration documents expected ComfyUI output nodes. Captured
history is scanned for output objects returned by ComfyUI, then atomically
written under the run's output directory.

Every completed run records:

- original manifest and API workflow;
- resolved API workflow;
- parameter values and preset;
- source upload metadata;
- ComfyUI client/prompt IDs and history;
- output paths, byte sizes, and SHA-256 hashes.

Neither an output nor the run manifest may overwrite an existing file.
