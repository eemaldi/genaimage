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
  "schema_version": 1,
  "id": "workflow-id",
  "version": "1.0.0",
  "title": "Readable workflow title",
  "workflow": "api-workflow.json",
  "generation_mode": "initial_concept",
  "parameters": {
    "seed": {
      "node": "3",
      "input": "seed",
      "kind": "seed",
      "required": true,
      "default": 42
    },
    "reference": {
      "node": "12",
      "input": "image",
      "kind": "image_upload",
      "required": false,
      "description": "Optional approved source reference."
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
  ],
  "required_gates": ["gate3"]
}
```

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

No two named parameters may bind the same node/input. Unknown preset parameters,
missing nodes/inputs, unsupported JSON types, and invalid ranges fail
preflight.

`image_upload` parameters contain a local path. The file is uploaded to the
configured ComfyUI input area before submission; the run stores the path,
checksum, byte size, and returned ComfyUI filename.

## Presets

A preset is a named set of parameter values. CLI/Tauri values override preset
values, and the effective values are recorded in the run manifest. A preset does
not approve content or bypass gate checks.

## Generation modes and gates

The manifest's `generation_mode` adds an invariant gate:

- `generic`, `delivery`: no automatic gate;
- `initial_concept`, `correction`: Gate 3;
- `refinement`: Gate 4;
- `multi_view`: Gate 5.

`required_gates` can add more gates but cannot remove the mode-derived one.

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
- gate snapshot, when applicable;
- output paths, byte sizes, and SHA-256 hashes.

Neither an output nor the run manifest may overwrite an existing file.
