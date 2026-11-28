# Workflow packs

A pack is a read-only directory that bundles workflow manifests, template
references and declared requirements, so features can be added without core
changes. The bundled starter workflow ships as the `core` pack
(`workflows/packs/core`). Packs never install anything: GenAImage only checks
their requirements against ComfyUI and reports what is missing.

## Roots and layout

- `paths.workflows` is the single **writable user root**. It defaults to
  `workflows/comfyui` in the workspace and may be absent: a missing user root
  is not an error and the catalogue then holds only pack workflows. Workflow
  import (issue #67) is planned and not yet implemented; when it lands, the
  import will create the root on its first write.
- `paths.packs` is a list of **read-only pack roots**. Each root is a
  *directory of pack directories*: every immediate child directory that
  contains `pack.toml` is a pack. Point it at the parent of the packs, not at a
  pack itself (a root that is itself a pack is reported as a diagnostic). It
  defaults to `["workflows/packs"]`. A missing root is skipped; an empty list
  disables packs. `GENAIMAGE_PACKS` overrides it with a path list using the
  platform separator (`:` on Linux and macOS, `;` on Windows); an empty value
  means no roots.
- A directory that contains `pack.toml` **inside a configured pack root** is
  never scanned as user workflows, so nesting a pack root inside the user root
  does not report workflows twice. A directory that contains `pack.toml`
  anywhere else in the user root keeps its workflows visible and gets a
  discovery diagnostic: its pack is ignored, and pack directories belong in
  `paths.packs`.
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
templates = ["templates/portrait.toml"]

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
| `templates` | Relative paths to [prompt template](templates.md) files, at most 64. They are read-only and are parsed strictly when `GET /api/v1/templates` lists them; a missing or escaping file is a pack diagnostic, an invalid one is reported in the template diagnostics and also as a pack problem, so the pack is never `ok` (a declared `.md` file that is not a TOML template counts as invalid). |
| `checkpoint_profiles` | Optional relative path to a TOML file of [checkpoint profiles](#checkpoint-profiles), ending in `.toml`. Confined to the pack like every other declared file. |
| `requires.node_classes` | ComfyUI node classes the pack needs. At most 256. |
| `requires.models` | A table from model folder to file names. At most 32 folders and 512 names in total. |

Relative paths use at most six plain components and no `..`, absolute paths,
backslashes or NUL bytes. Symlinks are not rejected as such: every file a pack
uses (`pack.toml`, workflow manifests, API graphs, templates and thumbnails)
must **resolve inside the pack directory**, and a symlink that stays inside it
is accepted. The path is resolved before anything is read and the resolved
path is the one opened. A `pack.toml`, declared manifest or template that
resolves outside rejects the pack or workflow; an API graph that resolves
outside makes that workflow invalid. A file that fails this check is never
read, so its contents cannot appear in a diagnostic. `pack.toml` is read with a
64 KiB limit. Thumbnails are confined to the pack directory and are opened
without following symlinks.

## Discovery

The catalogue is the user root plus every valid pack. Each workflow summary
carries `source`: `user` or `pack:<id>`.

- A **duplicate workflow ID** across roots (or within one) is a discovery
  diagnostic naming every claimant. The ID is unavailable: details, validation,
  thumbnails, submissions and reruns answer `409` with the problem type
  `urn:genaimage:problem:workflow-ambiguous` and a message naming every
  claimant's source, until the duplicate is removed. Nothing is shadowed. Every
  claimant stays in the workflow listing with `available: false` and an
  `unavailable_reason` naming the other claimants; the assistant never lists an
  unavailable ID. `GET /api/v1/packs` reports the same problem in the pack's
  `diagnostics`.
- A **duplicate pack ID** rejects every claimant with a diagnostic.
- A **broken pack** (invalid `pack.toml`, escaping symlink) is a diagnostic and
  does not affect other packs. A declared file that is missing leaves the pack
  loaded and is reported on the pack.
- The workflow IDs `refresh` and `import` are reserved for `POST /api/v1/workflows/refresh` and `POST /api/v1/workflows/import`.
- At most 64 packs and 512 entries per root are scanned.

A new pack directory appears without a restart: every catalogue request
rebuilds the listing, and the thumbnail index revalidates the pack roots at
most once per second. `POST /api/v1/workflows/refresh` (or
`genaimage workflow refresh`) forces a rebuild and returns the listing.

## Upgrading from a release without packs

The bundled `text-to-image` workflow moved from `workflows/comfyui/text-to-image`
to `workflows/packs/core/text-to-image`. If your workspace still has a copy of
it in the user root, its ID now clashes with the `core` pack and
`text-to-image` becomes unavailable (`409`, `workflow-ambiguous`). Remove your
copy, or change its `id` in the manifest. Runs recorded earlier still rerun
once the ID resolves again, provided the graph and manifest are byte-identical,
whichever root they live in.

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
- The response also carries `diagnostics`: problems with the pack's own files
  and workflows, that is a declared workflow or template that is missing or
  rejected, an invalid workflow manifest, or a workflow ID that is duplicated
  or unavailable. The status is never `ok` while `diagnostics` is non-empty.
- The pack `status` is `missing` if a requirement is missing or a declared
  file or workflow is missing, rejected or duplicated; otherwise `unknown` if
  anything is unknown; otherwise `ok`. If ComfyUI cannot be queried the status
  is `unavailable` with a `reason`, and every requirement is `unknown`. A pack
  that declares no requirements and has no diagnostics is `ok`; one with
  diagnostics is `missing`, even though ComfyUI is not queried for it.

The doctor check `packs.requirements` calls the same core functions
(`pack_requirements::check_requirements` and the pack diagnostics) for every
pack, so it can never be looser than the endpoint. Pack diagnostics are
reported as a warning even when `/object_info` is unknown; only the node and
model part is skipped then. See [troubleshooting](troubleshooting.md#doctor-checks).

## Checkpoint profiles

A pack may declare parameter values for exact checkpoint names in a file named
by `checkpoint_profiles` in `pack.toml`, conventionally `checkpoints.toml`:

```toml
[[profiles]]
id = "sdxl-base"
label = "SDXL base"
checkpoints = ["sd_xl_base_1.0.safetensors"]
note = "Recommended settings for this checkpoint."

[profiles.values]
steps = 30
cfg = 7.0
sampler = "dpmpp_2m"
```

- `id`: as a pack ID, unique in the file. `label`: one line of at most 256 bytes.
  `checkpoints`: 1 to 64 exact names, each listed once. `note`: optional text.
  `values`: parameter name to a string, number or boolean, at most 64.
  At most 64 profiles; the file is at most 64 KiB; unknown fields are rejected.
- Matching is string equality on the checkpoint name. Nothing infers a model
  family from a file name, so a checkpoint that no profile lists has no profile.
- A profile is never applied implicitly. A run request or validation names it
  with `checkpoint_profile`. The server then requires that the pack declares the
  profile, that the workflow has a single `CheckpointLoaderSimple` `ckpt_name`
  parameter and that its effective value (user, then preset, then default) is
  one of the profile's names; otherwise it answers `422`. A profile cannot set
  that checkpoint parameter itself.
- The values apply to the workflows of the pack that define the parameter; each
  value is validated by that parameter exactly as a preset value is (numbers are
  compared exactly, never through `f64`). TOML integers are 64-bit signed and
  floats keep their literal form, so `2.0` stays `2.0` and `-0.0` stays `-0.0`.
- Precedence: default, then profile, then preset, then user value. The run's
  `preflight.json` and run manifest record `checkpoint_profile` (the ID) only
  when one was named. The resolved values are in the run parameters, so a rerun
  replays them without reading the profile file again.
- A malformed file loads no profile at all and is a pack problem. A value that a
  workflow's parameter would refuse, or a parameter that no workflow of the pack
  defines, is also a pack problem. `validate`, `doctor` and the listings all
  report it through the same `pack_problems` function.

`GET /api/v1/workflows/{id}` lists the pack's profiles as `checkpoint_profiles`
(`id`, `label`, `checkpoints`, `note`, `values` for the parameters that
workflow defines) and the matched parameter as `checkpoint_parameter`.
`GET /api/v1/models/checkpoints` adds `profiles`: for every installed checkpoint
name, the `{pack, id, label}` of each profile that lists it (an empty list when
none does) and a `profile_note`.

## API and CLI

| Surface | Result |
| --- | --- |
| `GET /api/v1/packs` | `{packs: [{id, version, description, root, directory, workflow_ids, templates, requires: {node_classes, models}, diagnostics}], diagnostics: [{path, error}]}` |
| `GET /api/v1/packs/{id}/validate` | `{pack_id, status, reason, node_classes: [{class, status}], models: [{folder, name, status, loader, reason}], diagnostics: [string]}`; `404` for an unknown or unavailable pack ID. |
| `POST /api/v1/workflows/refresh` | The `GET /api/v1/workflows` listing after a forced rebuild. |
| `genaimage packs list` | The packs listing. |
| `genaimage packs validate <ID>` | The validation. Exit `0` only for `ok`; exit `6` for `missing`, `unknown` and `unavailable`; exit `4` when the server answers with a problem, such as `404` for an unknown pack ID. |
| `genaimage run <ID> --checkpoint-profile <PROFILE>` and `genaimage workflow validate <ID> --checkpoint-profile <PROFILE>` | Name a [checkpoint profile](#checkpoint-profiles); never implied. |
| `genaimage workflow refresh` | The refreshed workflow listing. |

Pack templates are consumed by the template library; see [templates](templates.md#scopes).

Core entry points: `Harness::discover_packs`, `Harness::list_templates`,
`Harness::validate_pack` and `pack_requirements::check_requirements`.
