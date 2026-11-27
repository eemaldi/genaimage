# Workflow packs

A pack is a read-only directory that bundles workflow manifests, template
references and declared requirements, so features can be added without core
changes. The bundled starter workflow ships as the `core` pack
(`workflows/packs/core`). Packs never install anything: GenAImage only checks
their requirements against ComfyUI and reports what is missing.

## Roots and layout

- `paths.workflows` is the single **writable user root**. Workflow imports
  (a later feature) write there. It defaults to `workflows/comfyui` in the
  workspace and may be missing; a missing user root is not an error.
- `paths.packs` is a list of **read-only pack roots**. Each root is a
  *directory of pack directories*: every immediate child directory that
  contains `pack.toml` is a pack. Point it at the parent of the packs, not at a
  pack itself (a root that is itself a pack is reported as a diagnostic). It
  defaults to `["workflows/packs"]`. A missing root is skipped; an empty list
  disables packs. `GENAIMAGE_PACKS` overrides it with a path list using the
  platform separator (`:` on Linux and macOS, `;` on Windows); an empty value
  means no roots.
- Directories that contain `pack.toml` are never scanned as user workflows, so
  nesting a pack root inside the user root does not report workflows twice.
- Child directories without `pack.toml`, and loose files, are ignored.
- `paths.packs` is read-only in the settings API: edit it in the configuration
  file. A settings save keeps it untouched.

```text
workflows/packs/
  core/
    pack.toml
    text-to-image/
      workflow.genaimage.json
      api-workflow.json
```

## `pack.toml`

```toml
id = "core"
version = "1.0.0"
description = "Bundled starter workflows."
workflows = ["text-to-image/workflow.genaimage.json"]
templates = ["templates/portrait.md"]

[requires]
node_classes = ["KSampler", "SaveImage"]

[requires.models]
checkpoints = ["sd15.safetensors"]
loras = ["style.safetensors"]
```

Unknown fields are rejected at every level. `id` and `version` are required.

| Field | Meaning |
| --- | --- |
| `id` | 1 to 64 lowercase letters, digits, `-` or `_`, starting with a letter or digit. Unique across all roots. |
| `version` | Non-blank text of at most 256 bytes. Always a string, never a number. |
| `description` | Optional text of at most 2000 bytes. |
| `workflows` | Manifest paths relative to the pack, each ending in `.genaimage.json`. At most 64. Only declared manifests are loaded. |
| `templates` | Relative paths, at most 64. They are parsed and listed only; they are not interpreted yet. |
| `requires.node_classes` | ComfyUI node classes the pack needs. At most 256. |
| `requires.models` | A table from model folder to file names. At most 32 folders and 512 names in total. |

Relative paths use at most six plain components and no `..`, absolute paths,
backslashes or NUL bytes. A symlink that resolves outside the pack rejects the
whole pack. The file is read with a 64 KiB limit. A pack's API graph must also
resolve inside the pack, otherwise that workflow is reported as invalid.

## Discovery

The catalogue is the user root plus every valid pack. Each workflow summary
carries `source`: `user` or `pack:<id>`.

- A **duplicate workflow ID** across roots (or within one) is a discovery
  diagnostic naming every claimant. The ID is unavailable: details, validation
  and runs refuse it until the duplicate is removed. Nothing is shadowed.
- A **duplicate pack ID** rejects every claimant with a diagnostic.
- A **broken pack** (invalid `pack.toml`, escaping symlink) is a diagnostic and
  does not affect other packs. A declared file that is missing leaves the pack
  loaded and is reported on the pack.
- The workflow ID `refresh` is reserved for `POST /api/v1/workflows/refresh`.
- At most 64 packs and 512 entries per root are scanned.

A new pack directory appears without a restart: every catalogue request
rebuilds the listing, and the thumbnail index revalidates the pack roots at
most once per second. `POST /api/v1/workflows/refresh` (or
`genaimage workflow refresh`) forces a rebuild and returns the listing.

## Requirement validation

`GET /api/v1/packs/{id}/validate` and `genaimage packs validate <ID>` compare
the declared requirements with ComfyUI `/object_info` (five second limit) and
never install, download or substitute anything.

- Node classes are `available` or `missing`.
- A model folder maps to a loader input: `checkpoints` to
  `CheckpointLoaderSimple.ckpt_name`, `loras` to `LoraLoader.lora_name`, `vae`
  to `VAELoader.vae_name`, `controlnet` to `ControlNetLoader.control_net_name`,
  `upscale_models` to `UpscaleModelLoader.model_name`, `clip` and
  `clip_vision` to `CLIPLoader.clip_name` and `CLIPVisionLoader.clip_name`,
  `unet` and `diffusion_models` to `UNETLoader.unet_name`, `style_models` to
  `StyleModelLoader.style_model_name`. Both `object_info` option shapes are
  read. Path separators are compared as `/`.
- A folder without a known loader, or a loader node that does not list its
  options, gives `unknown` for each of its models, with a reason. It is never
  reported as available.
- The pack `status` is `missing` if anything is missing, otherwise `unknown` if
  anything is unknown, otherwise `ok`. If ComfyUI cannot be queried the status
  is `unavailable` with a `reason`, and every requirement is `unknown`. A pack
  that declares no requirements is `ok`.

The doctor check `packs.requirements` calls the same core function
(`pack_requirements::check_requirements`) for every pack with requirements, so
it can never be looser than the endpoint. See
[troubleshooting](troubleshooting.md#doctor-checks).

## API and CLI

| Surface | Result |
| --- | --- |
| `GET /api/v1/packs` | `{packs: [{id, version, description, root, directory, workflow_ids, templates, requires: {node_classes, models}, diagnostics}], diagnostics: [{path, error}]}` |
| `GET /api/v1/packs/{id}/validate` | `{pack_id, status, reason, node_classes: [{class, status}], models: [{folder, name, status, loader, reason}]}` |
| `POST /api/v1/workflows/refresh` | The `GET /api/v1/workflows` listing after a forced rebuild. |
| `genaimage packs list` | The packs listing. |
| `genaimage packs validate <ID>` | The validation; exit `6` unless the status is `ok`. |
| `genaimage workflow refresh` | The refreshed workflow listing. |

Core entry points: `Harness::discover_packs`, `Harness::list_pack_templates`,
`Harness::validate_pack` and `pack_requirements::check_requirements`.
