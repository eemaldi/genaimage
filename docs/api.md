# API reference

The loopback server exposes a JSON HTTP API under `/api/v1`. The machine-readable
contract is the OpenAPI 3.1 document, checked in at
[`docs/api/openapi.json`](api/openapi.json) and served by a running server at
`GET /api/v1/openapi.json`. Errors are problem details with a
`recovery` hint. Behaviour and request details live in the
[architecture](architecture.md#local-http-server); this page only maps the areas.

| Area | Endpoints |
| --- | --- |
| Health and diagnostics | `GET /health`, `GET /doctor` |
| Settings | `GET`/`PUT /settings` (ETag with `If-Match`), `POST /settings/test` |
| Setup wizard | `GET /setup/status`, `POST /setup/detect` |
| Models | `GET /models/llm`, `GET /models/checkpoints`, LLM pulls under `/models/llm/pulls` (see [models](models.md)) |
| Workflows | `GET /workflows`, `GET /workflows/{id}`, `POST /workflows/{id}/validate`, `GET /workflows/{id}/thumbnail`, `POST /workflows/import`, `POST /workflows/import/confirm` (see [importing a workflow](workflow-manifest.md#importing-a-comfyui-workflow)) |
| Prompt templates | `GET /templates`, `POST /templates`, `GET /templates/user/{id}`, `PUT`/`DELETE /templates/user/{id}`, `GET /templates/pack/{pack_id}/{path}`, `POST /templates/render` (see [templates](templates.md#api)) |
| Uploads | `POST /uploads`, `GET /uploads/{id}` |
| Runs | `GET`/`POST /runs` (a run may carry `character: {id, spec_version}`), `GET /runs/{id}`, `cancel`, `rerun`, `reconcile`, `events`, `manifest`, `resolved-workflow`; `POST /runs/reconcile` |
| Characters | `GET`/`POST /characters`, `GET /characters/{id}`, `GET`/`POST /characters/{id}/specs`, `GET /characters/{id}/specs/{version}`, `GET`/`POST /characters/{id}/references`, `POST /characters/{id}/references/{reference_id}/remove`, `GET`/`POST /characters/{id}/decisions`, `GET /characters/{id}/outputs`, `GET /characters/{id}/runs` (see [characters](#characters)) |
| Batches | `POST /batches/preview`, `GET`/`POST /batches`, `GET /batches/{id}`, `POST /batches/{id}/resume`; runs gain `batch_id`/`batch_index` and `GET /runs?batch=` (see [batches](architecture.md#batches-api)) |
| Outputs | `GET /outputs`, `GET /outputs/{id}`, `metadata`, `thumbnail`, `annotations`, `POST /outputs/{id}/review`, `POST /outputs/{id}/spec-review` (see [spec review](#spec-review)) |
| Live events | `GET /events` (server-sent events) |
| Assistant | Conversations, messages, tool calls and confirmations under `/assistant`, plus `/assistant/prompt` and `/assistant/suggest` |

Rules that clients must keep, because the server enforces them:

- Pin a submission to the reviewed `workflow_revision`; a stale revision returns
  409 (`urn:genaimage:problem:workflow-changed`).
- Render a template only against the `sha256` you reviewed; a changed template
  returns 409 (`urn:genaimage:problem:template-changed`). A render submits
  nothing and the run record keeps the final prompt only.
- Use one idempotency key per reviewed request and retry with the same key.
- Use `POST /runs/{id}/rerun` for an identical re-run.
- Preview a batch, show its item count and confirm it before submitting; a
  batch is always pinned to a `workflow_revision` and states an explicit
  `max_items`. Never truncate a rejected sweep to fit.
- `GET /batches/{id}` is not purely a read: like `GET /runs/{id}`, it settles a
  run whose submission owner died as `unknown_submission`. `GET /batches` never
  does, so such a run shows as `pending` in the list until the batch is read.
  Preview lists the `inherited_inputs` once for the whole batch, because every
  item shares them.
- Link a run to a character with `character: {id, spec_version}` in
  `POST /runs`. The link is verified before anything is written (404 and no run
  otherwise), recorded in the run, `preflight.json` and the manifest, copied by
  a re-run and shown in run details and lists. It never changes what is
  generated. Batches carry no link, and a `character` field on a batch is a
  422.
- Mutating browser requests need a same-origin `Origin`; there is no CORS.

## Characters

A character is a durable record with append-only spec versions, references
(attached uploads) and approve, reject or clear decisions. Nothing here edits or
deletes a record, and none of it changes a generation by itself. The core rules
are in the [architecture](architecture.md#characters).

| Method and path | Result |
| --- | --- |
| `POST /characters` `{name, spec?, note?}` | 201 `{character, spec_version}`; `spec` writes version 1 in the same transaction |
| `GET /characters?limit&cursor` | `{characters, next_cursor}`, newest first |
| `GET /characters/{id}` | `{id, name, created_at, latest_spec_version}`; `latest_spec_version` is `null` until a version exists |
| `POST /characters/{id}/specs` `{based_on?, spec, note?}` | 201 `{character_id, version, created_at, based_on, spec, note}` |
| `GET /characters/{id}/specs?limit&cursor` | `{versions, next_cursor}`, newest first |
| `GET /characters/{id}/specs/{version}` | One version; `version` is digits only, with no sign or leading zero |
| `POST /characters/{id}/references` `{upload_id, role, note?}` | 201 `{id, character_id, upload_id, role, note, added_at, removed_at}`; `role` is `sketch`, `reference` or `mask` |
| `POST /characters/{id}/references/{reference_id}/remove` | 200, the reference with `removed_at` set; the record stays and a repeat returns it unchanged. Send no body (400 otherwise) |
| `GET /characters/{id}/references?include_removed&limit&cursor` | `{references, next_cursor}`; removed ones only with `include_removed=true` |
| `POST /characters/{id}/decisions` `{output_id, spec_version, decision, note?}` | 201 `{id, character_id, output_id, spec_version, decision, note, decided_at}`; `decision` is `approved`, `rejected` or `cleared` |
| `GET /characters/{id}/decisions?output_id&spec_version&limit&cursor` | The history including `cleared`, newest first: `{decisions, next_cursor}` |
| `GET /characters/{id}/outputs?state&spec_version&limit&cursor` | The current states joined with their outputs: `{outputs: [{decision, output}], next_cursor}`. `state` is `approved` or `rejected`; omit it for both. `output` is the shape of `GET /outputs/{id}/metadata`, so fetch images from `/outputs/{id}/thumbnail` and `/outputs/{id}` |
| `GET /characters/{id}/runs?spec_version&status&limit&cursor` | `{runs, next_cursor}`, the runs linked to the character |

Rules:

- The current state of an output is its latest decision across all spec versions;
  a `cleared` output has none and is not listed.
- `based_on` must be the latest version the author saw (omit it for version 1).
  A stale value is 409 `urn:genaimage:problem:character-spec-conflict` with the
  problem field `latest`, and nothing is written.
- Only the canonical output ID `{run_id}:{row}` is accepted (422 otherwise). An
  unknown output is 404, a run that has not completed cannot be approved or
  rejected (422), and an output of a run linked to another character is 409
  `urn:genaimage:problem:character-conflict`.
- Numbers in a JSON body or a path are exact whole numbers: `spec_version` and
  `based_on` reject `2.0`, strings, signs and values beyond 32 bits (422 in a
  body, 400 in a path). Query-string numbers (`spec_version`, `limit`) are parsed
  leniently, so `01` is read as `1`.
- Lists are bounded: `limit` is 1 to 200 (default 50) and `cursor` is opaque
  (422 when invalid). Pass the same filters on every page.
- A serialised spec is limited to 64 KiB (422), and a request body to 1 MiB (413).
- Every `POST` takes an optional `Idempotency-Key` (1 to 128 visible ASCII
  characters, no commas; 400 when malformed). The first write is 201. Repeating
  the same key and request writes nothing and returns the record with 200. That
  is the first record, except that a keyed reference replay returns the reference
  as it is now, so `removed_at` may be set. The same key with another request, or on another endpoint or character, is
  409 `urn:genaimage:problem:idempotency-conflict`. A failed write stores no key.
  The character key space is separate from runs and batches. A removal is always
  200, because it creates nothing. `POST` of a character or version also sets
  `Location`.
- Each write is one transaction, so a problem response means nothing was written;
  the recovery text says so.

Problem types, all `urn:genaimage:problem:`-prefixed: `character-not-found`,
`character-spec-version-not-found`, `character-reference-not-found`,
`character-upload-not-found` and `output-not-found` (404), `character-spec-conflict`,
`character-conflict` and `idempotency-conflict` (409), and
`invalid-character-request` (422).

Regenerate the contract and portal types after an API change:

```bash
cargo run -p genaimage-server --no-default-features --example openapi
python3 scripts/generate_portal_types.py
```

## Spec review

`POST /api/v1/outputs/{id}/spec-review` with `{"character_id": "...", "spec_version": 1}`
asks the configured **vision** model to check an output against one exact spec
version (`spec_version` is a whole number; `1.0`, `"1"` and unknown fields are
rejected with 4xx). The text role is never used: an unconfigured vision role
returns 503 (`urn:genaimage:problem:assistant-unavailable`) before any model
request. An unknown output, character or spec version returns 404. A spec
with no mandatory or flexible elements returns 422 before any model request;
add some in a new spec version. A model answer that is not the required JSON,
is truncated, assesses none of the reviewed elements, has an oversized summary
or lists more than 4 x the reviewed elements + 16 entries returns 502 and stores
nothing.

The 200 response is the stored annotation (`kind` is `spec_review`). `caption`
mirrors the summary and `adherence_critique` is empty, both for compatibility
with `vision_review` clients, and `tags` is empty. Clients should read
`spec_review`:

```json
{
  "character_id": "...",
  "spec_version": 1,
  "checklist": [
    {"id": "cloak", "category": "clothing", "requirement": "mandatory",
     "verdict": "met", "note": "Crimson hood"}
  ],
  "dropped": ["unknown element ids dropped: boots"],
  "summary": "Close match",
  "label": "Model opinion, not approval. Record an approval or rejection separately."
}
```

`verdict` is `met`, `deviation` or `unclear`. Every mandatory element appears
exactly once; one the model left out (or listed twice, or gave an invalid
verdict or an oversized note) is reported as `unclear` with the note
`not assessed by the model`, or `conflicting duplicate entries; not recorded`
when it was listed more than once (all copies are dropped, never picked
between), and `dropped` says why. `dropped` holds at most 50 notes followed by
one `N further items dropped` note; the unknown-ids note lists the first 20 ids
then `and N more`. Flexible elements appear
only when assessed. Notes are at most 500 bytes and the summary 1,000 bytes;
nothing is truncated. A review never records an approval or rejection; use
the decisions endpoint for that. `GET /outputs/{id}/annotations` lists both
`vision_review` and `spec_review` annotations, newest first.
