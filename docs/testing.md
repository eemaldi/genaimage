# Testing

GenAImage tests use no real models by default. Run the complete repository checks with:

```bash
python3 scripts/ci_check.py
```

The script checks documentation links and skill mirrors, generated API types,
portal linting and tests, the Rust workspace formatting and Clippy checks, Rust
tests, and the Python helper tests. The assistant regression scenarios are in
the normal `genaimage-server` test suite and therefore run as part of the
workspace `cargo test` command.

For a focused API-only run:

```bash
cargo test -p genaimage-server --test assistant_scenarios --no-default-features
```

## Fake services and scenario harness

`crates/genaimage-test-support` supplies the loopback `FakeLlmServer` and fake
ComfyUI server used by the integration tests. `FakeLlmServer` scripts OpenAI
-compatible streamed text, tool-call deltas, delays, hangs and provider errors.
The fake ComfyUI records exact graph submissions and controls queue, history,
completion and failure states. Every server listens on an ephemeral loopback
port and is stopped with its test fixture.

`crates/genaimage-server/tests/assistant_scenarios.rs` adds a small declarative
harness. A scenario is an ordered list of steps such as a user message and its
scripted model turns, a preview, a confirmation decision, a backend completion
or a stream cancellation. Each step declares the exact SSE event sequence and
terminal state. The final declaration lists the expected persisted message
roles and statuses, audit rows, ComfyUI submission count and durable run count.

To add a scenario:

1. Start `ScenarioHarness` for the required tool-capability state.
2. Use `ModelTurn::Tool` or `ModelTurn::Text` to script each model response.
3. Add `Step` entries for user, preview, confirm, edit, reject or cancel actions.
4. Declare every SSE event in order, including `done`, `error` or
   `awaiting_confirmation`.
5. Declare the final messages, audit decisions and exact submission/run counts.
6. Keep the scenario deterministic and under the suite's roughly ten-second
   budget; do not wait for real models or ComfyUI.

The current scripted scenarios cover:

- multi-step `list_workflows`, `inspect_workflow`, `propose_run`,
  `submit_run`, confirmation and `run_status` use;
- invalid arguments returned to the model without invocation;
- confirmation rejection without a run;
- the tool-loop guard;
- editing after a fresh preview token;
- refusal of a stale preview token while the confirmation remains pending;
- double confirmation creating one run;
- cancellation during an active stream; and
- a clear preflight error for a text model without tool support; and
- the read-only chat-only tools (`assistant_read_tools.rs`): a fake-LLM
  conversation that lists checkpoints with their exactly matching profiles,
  inspects a workflow for its layouts and profiles, and reads a character, plus
  direct tool tests for bounds, cursors, filters, exact numbers
  (`2.0`, `-0.0`, large seeds), an error rather than an empty list when ComfyUI
  is unreachable, paging that visits every row exactly once, whole-element
  spec paging, pack problems for a malformed `checkpoints.toml`, a proof that
  no tool writes, and model-opinion annotations.

The suite protects these safety invariants: model output never submits a run;
submission requires a valid confirm or edit decision; preview tokens are bound
to reviewed arguments; invalid calls are not executed; decisions are single-use;
cancellation and loop exhaustion persist terminal state; audit and message
history remain inspectable; and ComfyUI submission counts stay exact.

## Opt-in Ollama tests

The live tests are `#[ignore]`d and excluded from normal CI. Ollama must expose
its OpenAI-compatible API on a loopback host. The test checks the configured URL
and refuses to contact a non-loopback host.

Example with a locally installed tool-capable text model and optional vision
model:

```bash
GENAIMAGE_LIVE_LLM=1 \
GENAIMAGE_LIVE_LLM_URL=http://127.0.0.1:11434/v1 \
GENAIMAGE_LIVE_LLM_TEXT_MODEL=qwen2.5:7b \
GENAIMAGE_LIVE_LLM_VISION_MODEL=llava:7b \
cargo test -p genaimage-server --test assistant_scenarios \
  --no-default-features -- --ignored live_ollama_stream_terminates --nocapture
```

The URL defaults to `http://127.0.0.1:11434/v1` and
`GENAIMAGE_LIVE_LLM_TIMEOUT_SECONDS` defaults to 240. Increase the latter for a
slow local model. Set the text-model variable to
a model available in the local Ollama instance; a tool-capable Qwen-family
model is a reasonable starting point. The vision-model variable is optional;
when used, choose a locally available multimodal model such as an LLaVA-family
model. Model availability and capability vary by installation, so verify the
chosen names with `ollama list` before running the test.

Live assertions deliberately avoid exact model text. They verify that the SSE
stream terminates with well-formed events, persisted state is readable, and no
ComfyUI submission occurs before a confirmation decision. The test rejects any
observed confirmation proposal and checks that the run and fake-ComfyUI counts
remain zero. Live-model nondeterminism can still produce an ordinary text
response, tool errors, or a bounded loop failure; those are valid outcomes as
long as the safety checks hold.
