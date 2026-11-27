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
| Workflows | `GET /workflows`, `GET /workflows/{id}`, `POST /workflows/{id}/validate`, `GET /workflows/{id}/thumbnail` |
| Uploads | `POST /uploads`, `GET /uploads/{id}` |
| Runs | `GET`/`POST /runs`, `GET /runs/{id}`, `cancel`, `rerun`, `reconcile`, `events`, `manifest`, `resolved-workflow`; `POST /runs/reconcile` |
| Outputs | `GET /outputs`, `GET /outputs/{id}`, `metadata`, `thumbnail`, `annotations`, `POST /outputs/{id}/review` |
| Live events | `GET /events` (server-sent events) |
| Assistant | Conversations, messages, tool calls and confirmations under `/assistant`, plus `/assistant/prompt` and `/assistant/suggest` |

Rules that clients must keep, because the server enforces them:

- Pin a submission to the reviewed `workflow_revision`; a stale revision returns
  409 (`urn:genaimage:problem:workflow-changed`).
- Use one idempotency key per reviewed request and retry with the same key.
- Use `POST /runs/{id}/rerun` for an identical re-run.
- Mutating browser requests need a same-origin `Origin`; there is no CORS.

Regenerate the contract and portal types after an API change:

```bash
cargo run -p genaimage-server --no-default-features --example openapi
python3 scripts/generate_portal_types.py
```
