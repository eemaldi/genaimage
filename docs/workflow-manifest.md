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

The published [JSON Schema](schemas/workflow-manifest.v2.json) defines the
structural format. Unknown fields are rejected, including the retired
`generation_mode` and `required_gates` fields. `id`, `version`, `workflow` and
`schema_version` are required; `id` and `version` must not be blank. `title`
defaults to an empty string. Optional `description`, `tags` (a list of
strings), `category` and `thumbnail` (a string reference) are descriptive
metadata; they do not affect generation or cause files to be loaded.

### Upgrading from version 1

Version 1 manifests are not migrated automatically. Remove
`generation_mode` and `required_gates`, change `schema_version` to `2`, and
optionally add metadata and UI hints. Validate the result against the schema
and run `genaimage workflow validate <manifest-path>`. Previously written run
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
Hints are optional and may be `null`; consumers can ignore unrecognised widget
names. Defaults and presets are explicit values, not UI hints.

No two named parameters may bind the same node/input. Unknown preset parameters,
missing nodes/inputs, unsupported JSON types, and invalid ranges fail
preflight. The schema checks structure, while workflow validation also checks
node/input bindings and values against kinds, ranges and allow-lists.

`image_upload` parameters contain a local path. The file is uploaded to the
configured ComfyUI input area before submission; the run stores the path,
checksum, byte size, and returned ComfyUI filename.

## Presets

A preset is a named set of parameter values. CLI values override preset
values, and the effective values are recorded in the run manifest. A preset does
not approve content.

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
