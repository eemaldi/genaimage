# Extending GenAImage

GenAImage extends at compile time. An extension is a Rust type that implements
one of the core traits and is registered in an `ExtensionRegistry` by the
binary or embedding application. There is no runtime plugin loading, no
dynamic library loading and no hot-loading of code. The architecture is set out
in [ADR 0001](adr/0001-target-architecture.md) and
[architecture](architecture.md#providers-extensions-and-deployment). Runtime
plugins were evaluated in [ADR 0003](adr/0003-runtime-plugin-model.md) (status: Proposed): adoption is
deferred, and external processes over JSON-RPC are preferred to WASM if a need
arises. Nothing here depends on it.

Extensions must keep the project's guardrails. Generation stays explicit: never
substitute a model, node, seed, image, prompt or setting. An ambiguous prompt
submission is reconciled, never blindly retried. LLM output is a proposal that
the user must confirm. The server stays on loopback. Previously written run
records are preserved and never overwritten.

Compiled, minimal stubs for every trait below live in
[`crates/genaimage-core/examples/extending.rs`](../crates/genaimage-core/examples/extending.rs).
They are built by `cargo test --workspace --all-targets`, so they cannot drift
from the real signatures. Run them with:

```bash
cargo run -p genaimage-core --example extending
```

The stubs deliberately fail closed: the image backend returns errors rather than
success, the LLM provider rejects its models and the tool refuses to run without
a confirmation.

## Where extensions plug in

| Extension | Trait | Registry | Selected by |
| --- | --- | --- | --- |
| Image backend | `ImageBackend` | `register_image_backend` | Configuration names only `comfyui` today |
| LLM provider | `LlmProvider` | `register_llm_provider` | `serve` registers `openai-compatible` when `llm.base_url` is set |
| Assistant tool | `AssistantTool` | `register_assistant_tool` | Every registered tool is offered to the assistant |
| Event sink | `EventSink` | `register_event_sink`, `register_event_sink_with` | `[[sinks]]` for built-in kinds; code for others |
| Workflow pack | `pack.toml` | `paths.packs` | Discovered from disk, no code |

Registration is explicit and fails on a duplicate name. Nothing is looked up by
a fallback name. The registry is built by the application: `serve` in
[`crates/genaimage/src/serve.rs`](../crates/genaimage/src/serve.rs) is the
reference wiring. Registering a backend or provider under a new name does not
change what the running server uses unless the wiring selects it, and today the
serve module selects `COMFYUI_BACKEND` and `LLM_PROVIDER` only.

## ImageBackend

[`image_backend.rs`](../crates/genaimage-core/src/image_backend.rs) defines the
contract. Core owns run orchestration and calls the trait; the backend only
talks to the image service. The reference implementation is `ComfyUiClient` in
[`genaimage-comfyui`](../crates/genaimage-comfyui/src/comfyui.rs), which uses
HTTP and WebSocket and maps responses into core types.

The trait methods, grouped by purpose:

- `name` and `base_url`: diagnostics and provenance. `base_url` is recorded in
  run records, so it must be the endpoint actually used.
- `health`, `object_info` and `queue`: status, node classes and queue state.
- `submit(workflow, client_id, prompt_id)`: sends the bound API-format workflow.
  Core persists `prompt_id` before the call, so the backend can reuse it. Only an
  acknowledged submission may return a different ID, and that ID must identify
  the same, unmodified submission.
- `with_timing` (optional; the default returns `None`): return a copy with new
  timeouts only if it keeps the backend identity and DNS pin. Otherwise timing
  changes wait for a restart.
- `history(prompt_id)`: evidence for that prompt only. Report `HistoryOutcome`
  honestly; `Unresolved` is the safe default when history is incomplete.
- `delete_queued`, `prepare_interrupt` and `interrupt`: cancellation. Override
  `prepare_interrupt` to report whether targeted interruption is supported. Its
  default does not check anything. Never fall back to an unscoped interrupt.
- `upload_image`, `upload_image_bytes` and `download_output`: inputs and outputs.
  Uploads must send the exact verified bytes.
- `monitor_events`: advisory progress for one client. It is not a source of truth.

Guardrails the implementation must follow:

- **Submit once.** If acceptance is uncertain (for example the response is lost),
  do not retry. Core reconciles the persisted `prompt_id` against `queue` and
  `history`. Absence from both is not proof of rejection.
- **Report unknown as unknown.** Return an error, or `Unresolved`, when the
  outcome cannot be established. Never return a successful output that was not
  confirmed.
- **Cancellation is evidence-based.** Return `CancellationNotSent` or
  `CancellationRejected` only for a definite outcome; any other error is
  treated as an unconfirmed request.
- **Never substitute.** Submit the graph given to you, unchanged.

Register it with `ExtensionRegistry::register_image_backend(name, Arc::new(...))`.
Core receives the same `Arc` through the registry.

**Testing.** Test the adapter over HTTP against a fake server.
`genaimage_test_support::FakeComfyServer` (see
[`genaimage-test-support`](../crates/genaimage-test-support/src/lib.rs)) records
exact graph submissions and scripts queue, history, completion and failure. The
ComfyUI contract tests in
[`crates/genaimage-comfyui/tests/contracts.rs`](../crates/genaimage-comfyui/tests/contracts.rs)
show the pattern: build a `ComfyUiProfile` whose `base_url` is the fake's
address, then call the adapter. Core lifecycle tests can use an in-test backend
that implements the trait, as the orchestrator tests do, without any network.

## LlmProvider

[`extensions.rs`](../crates/genaimage-core/src/extensions.rs) defines
`LlmProvider` with two methods: `chat_stream(ChatRequest) -> ChatStream` and
`list_models() -> Vec<LlmModel>`. The shipped implementation is
`OpenAiCompatibleProvider` in
[`genaimage-llm`](../crates/genaimage-llm/src/lib.rs). It speaks the
OpenAI-compatible `/v1/chat/completions` (streamed) and `/v1/models` routes,
which Ollama, llama.cpp and LM Studio expose when their compatible APIs are
enabled.

Expectations for an implementation:

- **Explicit model.** Use `ChatRequest.model` exactly. Do not pick another model
  when the requested one is missing.
- **Reject, do not degrade.** If a request needs a capability the model does not
  have (tools or vision), return `HarnessError::Configuration` naming the model
  and the capability, before returning a stream. Do not drop tools, images or
  prompts silently.
- **No role fallback.** The text and vision roles are separate (`LlmRole`).
  An implementation must never answer a vision request with a text model, and
  core never falls back between roles either.
- **Tool calls are proposals.** A `ChatDelta::ToolCall` is not permission to
  act. The tool runtime validates and asks for confirmation.
- **Cancellation.** Dropping the stream must cancel in-flight work and release
  resources.
- **Secrets are write-only.** `api_key` must never appear in `Debug` output,
  error messages or logs. `OpenAiCompatibleProviderOptions` redacts it in `Debug`
  and passes provider errors through a redaction step. Follow the same pattern.
- **Endpoints.** Outbound endpoints default to loopback. Only explicitly
  allow-listed private or Compose hosts are permitted, redirects and proxies are
  disabled, and public hosts are rejected.

Register it with `register_llm_provider(name, Arc::new(...))`. `LlmRoles` takes
the provider registered under `LLM_PROVIDER` (`openai-compatible`), so a custom
provider reaches the assistant only if the wiring registers it under that name.

**Testing.** Use `genaimage_test_support::FakeLlmServer` with a
`FakeLlmScript`: `text_chunks` for streamed text, `with_events` with
`FakeLlmEvent::tool_calls` for tool proposals, and `with_chat_error` for
provider errors. The server records requests, so assert on the exact model and
body sent. The provider contract tests in
[`crates/genaimage-llm/tests/contracts.rs`](../crates/genaimage-llm/tests/contracts.rs)
are the model to follow.

Live tests against a real local model are opt-in. They are `#[ignore]`d and run
only with `GENAIMAGE_LIVE_LLM=1` and a loopback URL. See
[testing](testing.md#opt-in-ollama-tests). Never add a live test to normal CI.

## AssistantTool

`AssistantTool` exposes `name`, `description`, `json_schema`,
`requires_confirmation`, an optional `preview` and `invoke(ToolContext, Value)`.
The built-in tools in
[`assistant/tools.rs`](../crates/genaimage-core/src/assistant/tools.rs) are the
reference. `submit_run` is the confirmation example.

**Schema.** Register-time validation accepts only a subset of JSON Schema: the
keywords `type`, `properties`, `required`, `additionalProperties`, `enum`,
`minimum`, `maximum`, `minLength`, `maxLength`, `items`, `minItems` and
`maxItems`. `type` must be one of `object`, `string`, `integer`, `number`,
`boolean` or `array`. `additionalProperties` may only be `false`. Numbers are
compared exactly, so keep integers as JSON integers and never round seeds.
`register_assistant_tool` rejects a schema outside this subset. The runtime also
validates every call's arguments against the schema before it can run.

**Confirmation.** Set `requires_confirmation` to `true` for anything that changes
state or submits work. The metadata is not the enforcement: the runtime pauses
the call with `confirmation_required`, and the turn ends as
`awaiting_confirmation`, which is not success.

- The pause carries a `preview_token`, the SHA-256 of
  `{arguments, confirmation_id, preview}`, where `preview` is the result of your
  optional `preview(arguments)` hook. The preview endpoint returns the same token.
- A confirm or edit must present that token. The runtime calls `preview` again
  (with the edited arguments for an edit) and recomputes the token. On a mismatch
  it returns `ToolConfirmationPreviewMismatch` with the fresh preview and token,
  and the confirmation stays pending. Decisions are single use.
- A reject returns a rejection result to the model.

Because every decision re-runs `preview`, it must be deterministic and free of
side effects for the same arguments: no timestamps, no randomness and no
changing state.

A tool with `requires_confirmation` runs its `invoke` only after a confirm or
edit decision. `ToolContext.confirmation_id` is set then and is the idempotency
key: pass it to the external call, as `submit_run` does with
`Harness::submit_keyed`. Refusing a missing ID is defence in depth; the runtime
does the enforcement.

**Interrupted invocations.** If the process stops after an invocation is claimed,
the confirmation settles as `outcome_unknown`. It is never re-invoked. Your tool
must therefore make its external effect idempotent under the confirmation ID, and
must not assume that a re-run is safe.

**Workflow proposals.** A tool that proposes a run must pin the reviewed
`workflow_revision` and validate the proposal against that exact revision, which
is the revision the model was shown. Drop invalid items with a note. Never
coerce them. The assistant never chooses image inputs; the user chooses them.

**Limits.** The loop is bounded to eight model steps by default. Keep results
small. Built-in tools cap results at 32 KiB; they mark truncated text and arrays,
and omit numbers or identifiers that do not fit, with a note, rather than
shortening them.

Register with `ExtensionRegistry::register_assistant_tool(Arc::new(...))`, which
uses the tool's `name()`. An embedding application passes custom tools to
`AppState::with_assistant_tools`. The GenAImage binary wires only its built-in
tools in `serve.rs`.

**Testing.** Use the scenario harness in
[`crates/genaimage-server/tests/assistant_scenarios.rs`](../crates/genaimage-server/tests/assistant_scenarios.rs).
A scenario scripts each model turn with `ModelTurn::Tool` or `ModelTurn::Text`,
lists the steps (user message, preview, confirm, edit, reject, cancel) and
declares the exact SSE sequence, persisted messages, audit rows and fake ComfyUI
submission count. Assert that the submission count is zero until a confirm
decision arrives, and that a repeated confirmation creates one run. The design
notes are in [testing](testing.md#fake-services-and-scenario-harness).

## EventSink

`EventSink` has one method: `handle(&self, &SinkNotification) -> Result<()>`. A
notification is either `Event(Box<EventEnvelope>)` or `ResyncRequired(reason)`.
The example's `KindCounter` shows both cases.

- **Never block the bus.** The publisher never waits for a sink. The registry
  moves each event into a per-sink bounded queue (256 by default, set with
  `SinkRuntime::queue_capacity`) and a worker calls `handle`. A slow sink causes
  drops, not publisher stalls.
- **Drops are explicit.** A full queue drops the newest event, counts it and
  writes a gap before the next delivered event. A bus lag is counted as a gap.
  A sink error or panic is counted and the next event is attempted, so one
  failure does not stop the sink.
- **Resynchronise on a gap.** On `ResyncRequired`, discard any state built from
  earlier events and reload authoritative state if you need it. Never present a
  partial stream as complete. The example clears its counts for this reason.
- **Keep work idempotent.** Delivery to remote receivers is at least once. The
  webhook sink in [`genaimage-webhook`](../crates/genaimage-webhook/src/lib.rs)
  adds an idempotency key from the bus ID and event ID, and receivers must
  de-duplicate.
- **Shutdown.** After shutdown, the worker delivers what is queued for up to
  three seconds, then counts the rest as dropped.

Registration:

- `register_event_sink(name, Arc<dyn EventSink>)` receives every event and
  reports with kind `extension`.
- `register_event_sink_with(name, sink, status, wants)` takes its own
  `Arc<SinkStatus>` and an `EventFilter`. Create the status with
  `SinkStatus::new(name, kind)`. The worker records delivered, failed, dropped
  and resync counts into it. A sink that keeps its own detail, such as retries
  or dead letters, records them through the same status.

Call `forward_event_sinks(&bus, SinkRuntime::default(), &shutdown, &tracker)`
once per bus, after all sinks are registered. It starts the queue and the
delivery loop of every sink through `spawn_sink`, the only forwarding path. The
doctor `event_sinks` check reads the same statuses through the bus. The
configured `[[sinks]]` (kinds `jsonl` and `webhook`) are built from the
configuration file. Code-registered sinks are only forwarded in a binary or
embedder that calls the registry directly. The reference is `start_sinks` in
[`serve.rs`](../crates/genaimage/src/serve.rs). Event semantics are described in
[core events](architecture.md#core-events) and
[event sinks](architecture.md#event-sinks).

**Testing.** Call `handle` directly with a constructed `SinkNotification` to test
the state logic. For delivery behaviour, use a sink that blocks or fails on
purpose and check the status counters and the gap handling, as the `sinks.rs`
tests do. The webhook tests in
[`crates/genaimage-webhook/tests/webhook.rs`](../crates/genaimage-webhook/tests/webhook.rs)
cover the wire behaviour, including retries and redirects.

## Workflow packs

A pack is a read-only directory that bundles workflow manifests, prompt
templates and declared requirements. Adding a pack needs no code and no
restart. The full rules are in [workflow packs](packs.md). The starter pack is
[`workflows/packs/core`](../workflows/packs/core).

```text
workflows/packs/
  my-pack/
    pack.toml
    text-to-image/
      workflow.genaimage.json
      api-workflow.json
    templates/
      portrait.toml
```

A minimal `pack.toml`:

```toml
id = "my-pack"
version = "0.1.0"
description = "Local portrait workflows."
workflows = ["text-to-image/workflow.genaimage.json"]
templates = ["templates/portrait.toml"]

[requires]
node_classes = ["KSampler", "SaveImage"]

[requires.models]
checkpoints = ["sd15.safetensors"]
```

- `id` (1 to 64 lowercase letters, digits, `-` or `_`) and `version` are
  required. Unknown fields are rejected at every level.
- `workflows` lists manifests ending in `.genaimage.json`. A manifest names its
  API-format graph in its `workflow` field. See
  [workflow manifest](workflow-manifest.md).
- `templates` lists [prompt template](templates.md) files. Invalid templates
  make the pack report a problem.
- `requires` declares node classes and model files. GenAImage compares them
  with ComfyUI and never installs or downloads anything.
- Paths are relative and must resolve inside the pack.

Discovery reads `paths.packs`, a list of roots. Each root is a directory of pack
directories, not a pack itself. The default is `workflows/packs`, and
`GENAIMAGE_PACKS` overrides it. Every catalogue request rebuilds the listing, so
a new pack appears without a restart. `POST /api/v1/workflows/refresh` or
`genaimage workflow refresh` forces a rebuild.

Check a pack before relying on it:

```bash
genaimage packs list
genaimage packs validate my-pack
```

Validation exits `0` only when the status is `ok`, `6` for `missing`, `unknown`
or `unavailable`, and `4` when the server answers with a problem. A duplicate
workflow ID makes that workflow unavailable until the duplicate is removed. The
IDs `refresh` and `import` are reserved.

## Test guidance

- **No real models in CI.** Use fake servers. `genaimage-test-support` provides
  `FakeComfyServer` (ComfyUI) and `FakeLlmServer` and `FakeOllamaServer` (LLM).
  Each listens on an ephemeral loopback port and stops with its fixture.
- **Focused tests first.** For example:
  - `cargo test -p genaimage-comfyui --test contracts`
  - `cargo test -p genaimage-llm --test contracts`
  - `cargo test -p genaimage-webhook --test webhook`
  - `cargo test -p genaimage-server --test assistant_scenarios --no-default-features`
- **Examples compile and run.** `cargo test --workspace --all-targets` builds
  `crates/genaimage-core/examples/extending.rs`, and CI also runs it with
  `cargo run -q -p genaimage-core --example extending --locked`. Clippy runs on it
  with `-D warnings`, because `ci_check.py` uses `--all-targets`.
- **Full check.** From the repository root, run `python3 scripts/ci_check.py`. It
  checks the documentation links, the skill mirrors, the generated API types, the
  portal, formatting, clippy, the Rust tests and the Python helper tests. Before
  a default build, run `npm ci && npm run build` in `apps/portal`, or use
  `--no-default-features` for an API-only build.
- **Opt-in live tests** run only with `GENAIMAGE_LIVE_LLM=1`, as described in
  [testing](testing.md).

Core's own fakes (for example `FakeImageBackend` in the orchestrator tests) are
private to their test modules. Use `genaimage-test-support` for anything that
crosses a process or network boundary.
