# Prompt templates

A prompt template is a reusable piece of prompt text with named holes. Filling
the holes is deterministic string substitution: no logic, no filters, no
nesting and no LLM. The result is ordinary text that you review and put in a
prompt parameter yourself.

This is not the assistant's system-prompt template (`assistant/prompts`), which
is a separate concept.

Status: the file format, both scopes, the API and the CLI are implemented. The
portal picker is **not yet implemented**; use the API or the CLI for now.

## File format

A template is one TOML file. Unknown fields are rejected at every level.

```toml
id = "portrait-studio"
title = "Studio portrait"
description = "optional"
text = "A {{subject}} photographed in a studio, {{lighting}} lighting"

[variables.subject]
description = "Who or what"
required = true

[variables.lighting]
description = "optional"
default = "soft"
```

| Field | Rule |
| --- | --- |
| `id` | `[a-z0-9][a-z0-9_-]{0,63}`. For a user template it must equal the file name without `.toml`. |
| `title` | Non-blank, at most 200 bytes. |
| `description` | Optional, at most 2000 bytes. |
| `text` | 1 to 16 384 bytes. |
| `variables.<name>` | `name` matches `[a-z][a-z0-9_]{0,63}`. At most 32 variables. |
| `variables.<name>.required` | Optional boolean, default `false`. |
| `variables.<name>.default` | Optional string of at most 4096 bytes. |
| `variables.<name>.description` | Optional, at most 2000 bytes. |

A file is at most 128 KiB. Variables are listed ordered by name, not by their
position in the file.

### Placeholders and escaping

- `{{name}}` is a placeholder. There are no spaces: `{{ name }}` is malformed.
- `{{{{` is a literal `{{`. It is the only escape. To print the text
  `{{subject}}`, write `{{{{subject}}`.
- Matching is left to right. A `{{` that does not start `{{{{` must be the
  start of a complete `{{name}}`; anything else (`{{x`, `{{}}`, `{{Name}}`,
  `{{{x}}`) makes the template invalid. `{{{{{{name}}` is a literal `{{`
  followed by the placeholder `name`.
- A single `{`, a single `}` and `}}` outside a placeholder are literal text.
  There is no escape for them because none is needed.
- Substituted values are inserted verbatim and never scanned again, so a value
  containing `{{other}}` stays as typed.

### Invalid templates

A template is invalid, listed only in `diagnostics` and unusable, when:

- a placeholder is not declared under `[variables]`;
- a declared variable is not used in `text`;
- a variable is `required = true` and also has a `default`;
- a variable is neither required nor given a `default` (an empty default
  `default = ""` is allowed). Without this rule an absent value would have to
  be guessed;
- a brace sequence is malformed;
- a bound is exceeded: 16 KiB of text, 32 variables, 4 KiB per default;
- the file is not UTF-8, is not strict TOML, has an unknown field, or (user
  templates) its `id` differs from its file name.

## Scopes

A template reference (`ref`) names one template:

| Scope | Ref | Writable | Where |
| --- | --- | --- | --- |
| User | `user:<id>` | yes | `paths.templates` |
| Pack | `pack:<pack_id>:<path>` | no | declared by `pack.toml` `templates = [...]` |

- **User** templates are one `<id>.toml` file each in `paths.templates`
  (`GENAIMAGE_TEMPLATES`). The default is
  `$XDG_DATA_HOME/genaimage/workspaces/<hash>/templates`, next to the state
  database and outside the repository, and the directory is created on the
  first write. A configuration without `paths.templates` works unchanged. Only
  files ending in `.toml` and not starting with `.` are read, and symlinked
  files are refused. At most 1024 directory entries are scanned.
  `.templates.lock` in that directory is a lock file; leave it alone.
- **Pack** templates are read-only. `<path>` is the relative path declared in
  `pack.toml` and is looked up in the declared list, never opened as a file
  name. The file must resolve inside its pack (see [packs](packs.md)). A pack
  template is any valid template file, whatever its extension; its `id` needs
  not match its file name.

Pack template problems (invalid content, a symlink out of the pack) appear in
`diagnostics` of `GET /api/v1/templates`. A missing declared file is a pack
diagnostic as before. `GET /api/v1/packs/{id}/validate` does not parse
templates, so its status does not reflect template validity.

## API

All routes are under `/api/v1`. Every template carries `sha256`, the SHA-256 of
its file bytes. It identifies the version the user reviewed; writes and renders
are pinned to it.

| Route | Purpose |
| --- | --- |
| `GET /templates` | List usable templates and diagnostics. |
| `GET /templates/user/{id}` | One user template. |
| `GET /templates/pack/{pack_id}/{path}` | One pack template. `{path}` is the declared path as one percent-encoded segment, so `t/a.toml` is `t%2Fa.toml`. |
| `POST /templates` | Create a user template (`201`). |
| `PUT /templates/user/{id}` | Replace a user template. |
| `DELETE /templates/user/{id}?expected_sha256=` | Delete a user template (`204`). |
| `POST /templates/render` | Render a reviewed template. |

`PUT` and `DELETE` on a pack path answer `405`.

### List

```json
{
  "templates": [
    {
      "ref": "user:portrait-studio",
      "scope": "user",
      "pack_id": null,
      "id": "portrait-studio",
      "title": "Studio portrait",
      "description": null,
      "variables": [
        {"name": "lighting", "description": null, "required": false, "default": "soft"},
        {"name": "subject", "description": "Who or what", "required": true, "default": null}
      ],
      "sha256": "<64 hex digits>",
      "text": "A {{subject}} photographed in a studio, {{lighting}} lighting"
    }
  ],
  "diagnostics": [
    {"ref": "user:broken", "path": "/data/templates/broken.toml", "error": "text: uses the undeclared placeholder '{{x}}'; declare it under [variables.x]"}
  ]
}
```

The list holds at most 256 templates (user first, then packs by pack ID) and
100 diagnostics; an omission is noted as a final diagnostic with an empty
`path`. Diagnostic `ref` is absent when the file name is not a valid id.

### Create and replace

`POST /templates` takes the file fields:

```json
{
  "id": "portrait-studio",
  "title": "Studio portrait",
  "text": "A {{subject}} photographed in a studio, {{lighting}} lighting",
  "variables": {
    "subject": {"description": "Who or what", "required": true},
    "lighting": {"default": "soft"}
  }
}
```

Create never overwrites: the file is written to a temporary file and hard
linked into place, so an existing id answers `409` and nothing is written.
`paths.templates` must be on a filesystem that supports hard links.

`PUT /templates/user/{id}` takes the same fields except `id`, plus
`expected_sha256`. The id cannot change. The server takes a lock, compares
`expected_sha256` with the hash of the file as it is now, writes a temporary
file and renames it into place, all under that lock (an in-process mutex plus
an advisory file lock), so there is no gap between the check and the write.
A replace may repair a template whose file is currently invalid, if the hash of
those bytes is supplied. Unknown request fields are rejected.

`DELETE` checks `expected_sha256` the same way.

### Render

```json
{"ref": "user:portrait-studio", "sha256": "<reviewed sha256>", "variables": {"subject": "cat"}}
```

returns

```json
{"text": "A cat photographed in a studio, soft lighting", "ref": "user:portrait-studio", "sha256": "<same sha256>"}
```

Rules:

- `variables` maps names to strings. A **default applies only when the
  variable is absent**. An empty string is a value: `"lighting": ""` renders
  as nothing, not as `soft`.
- Nothing is trimmed or normalised. Rendering the same input twice returns the
  same bytes.
- Values are at most 4 KiB each and the output at most 64 KiB.
- Rendering submits nothing and records nothing. Put the returned text in a
  prompt parameter and review it as with any other prompt.

### Problems

All are `application/problem+json` with a `recovery` hint.

| Status | `type` | When |
| --- | --- | --- |
| 400 | `urn:genaimage:problem:template-ref-invalid` | A malformed ref or id, or a missing `expected_sha256` on `DELETE` (plain 400). |
| 404 | `urn:genaimage:problem:template-not-found` | No such template. |
| 409 | `urn:genaimage:problem:template-exists` | Create with an existing id. Nothing is written. |
| 409 | `urn:genaimage:problem:template-changed` | `expected_sha256` or render `sha256` differs from the current file. The `current_sha256` field holds the current hash. Nothing is written or rendered. |
| 422 | `urn:genaimage:problem:template-invalid` | A create or replace body, or a stored template, is invalid. `errors` is `[{key, reason}]`, at most 20. |
| 422 | `urn:genaimage:problem:template-render-invalid` | A required variable is missing, a variable is unknown, or a value is not a string or too long. `errors` is `[{key, reason}]`, sorted by key, at most 20, with a final `{key: "", reason: "N more problems were omitted"}` when truncated. |

A render never drops an unknown variable silently, and never substitutes an
empty string for a missing one.

## CLI

`genaimage templates list` and `genaimage templates render <REF> --var
NAME=VALUE` call these routes; see [CLI](cli.md). The CLI has no create,
replace or delete command; use the API.

## Provenance

A run record stores the final prompt, not the template reference or the
variables. Recording the template ref, `sha256` and variables with the run is a
possible follow-up and is **not implemented**.

## Core entry points

`genaimage_core::templates`: `Harness::list_templates`, `Harness::template`,
`Harness::create_template`, `Harness::replace_template`,
`Harness::delete_template`, `Harness::render_template`. All validation lives
there; the server only maps HTTP.
