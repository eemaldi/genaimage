# ADR 0003: Runtime plugin model

- **Status:** Proposed

## Context

[ADR 0001](0001-target-architecture.md) makes extension points compile-time
traits registered in an `ExtensionRegistry`; there is no runtime plugin loading.
[Extending GenAImage](../extending.md) documents those traits. Issue
[#74](https://github.com/eemaldi/genaimage/issues/74) asks whether third-party,
runtime-loaded plugins may be needed later, and compares WASM components with
external processes over JSON-RPC. This ADR is a spike result. It implements
nothing.

The two targets are:

- **Assistant tools** (`AssistantTool`): `name`, `description`, `json_schema`,
  `requires_confirmation`, optional deterministic `preview`, and
  `invoke(ToolContext, Value)`. The confirmation protocol is enforced by the
  tool runtime in core: schema validation, `preview_token` binding, single-use
  decisions, the confirmation ID as idempotency key, and `outcome_unknown` for
  interrupted invocations, which are never re-invoked.
- **Image backends** (`ImageBackend`): about fourteen methods, including
  `submit` (once only, with a persisted `prompt_id`), `history` evidence,
  scoped cancellation, exact-byte uploads, output downloads and a live
  `monitor_events` stream. `base_url` is recorded in provenance and the
  endpoint must satisfy the loopback and allow-list policy.

Constraints from `AGENTS.md`: cold start under 150 ms, idle RSS under 40 MB,
binary under 25 MB (main is about 21.2 MB, so roughly 3.8 MB of headroom),
workspace `rust-version = "1.80"`, and every guardrail (no silent substitution,
no blind retry, confirmation before any submission, write-only secrets,
loopback endpoints).

## Options

- **A. WASM components** on `wasmtime` with the component model and WIT.
  Plugins are `.wasm` components; the host provides imports.
- **B. External processes** speaking JSON-RPC 2.0 over stdio, spawned and
  supervised by the server.
- **C. Compile-time only** (status quo). Third parties fork or embed
  `genaimage-core` and register their own types.

### Findings on A (WASM)

- **Sandbox.** The strongest of the options. A component has no ambient
  filesystem, network, clock or environment; it can call only the imports the
  host links. Network access can be a host-provided import such as
  `http-request(url, ...)`, so the loopback and allow-list policy is enforced
  structurally, in one place, in Rust. Linking WASI preview 2 sockets or
  `wasi:http` instead would hand the plugin the policy decision and is not
  acceptable here. Fuel and memory limits bound runaway plugins.
- **Async.** Wasmtime supports async host functions on a Tokio runtime
  (`async` feature). The component-model async ABI (`component-model-async`)
  is newer and still moving; streaming (`monitor_events`) would need it or a
  poll-style WIT design.
- **DX.** Rust authors use `cargo component` or the `wasm32-wasip2` target;
  Go, JS, Python and C have weaker or heavier toolchains (an embedded
  interpreter is several MB per plugin). WIT gives typed, versioned interfaces
  (`genaimage:plugin@0.1.0`). Authors cannot use `tokio`, `reqwest` or most
  native crates, so porting the ComfyUI client would be a rewrite against host
  imports. Debugging is harder than a native process.
- **Maintenance.** Wasmtime releases monthly; ahead-of-time (`.cwasm`) artefacts
  are valid only for the exact wasmtime version and engine configuration (a
  mismatch was observed during this spike, see below), and deserialising a
  `.cwasm` is `unsafe` in the API because it is only safe for trusted,
  host-produced input. Plugins must
  therefore ship `.wasm` and be compiled locally, which needs Cranelift in the
  host, or be compiled by a trusted tool that matches the host exactly.
- **MSRV.** `wasmtime 51.0.0` declares `rust-version = 1.97`; the workspace is
  1.80. Adopting it forces an MSRV raise of 17 releases, or isolating wasmtime
  in a separate helper binary.
- **Supply chain.** 84 to 199 additional unique crates depending on features
  (measured with `cargo tree`), including the Cranelift code generator and
  `unsafe`-heavy runtime code. Wasmtime has a mature security process, but it
  is a large attack surface that all users pay for even with no plugins.

### Findings on B (external processes over JSON-RPC)

- **Isolation.** Process isolation only: a separate address space, a crash does
  not take down the server, and the host controls the working directory,
  environment (`env_clear`), stdin and stdout. A plugin runs with the user's
  privileges and can read files and open any socket, so the loopback policy is
  **not enforceable by GenAImage** without OS facilities (network namespaces,
  Landlock, seccomp, `bwrap`), which are Linux-specific and optional. A
  plugin is therefore trusted code, in the same way as a ComfyUI custom node
  already is. This must be documented and shown at install time.
- **Lifecycle.** The server spawns one process per plugin (lazily, on first
  use), performs an `initialize` handshake (protocol version, plugin identity,
  declared tools and schemas), restarts on crash with back-off, and kills on
  shutdown. Tokio's `process` is already available through the workspace's
  `tokio = { features = ["full"] }`; no new dependency is needed.
- **Timeouts and cancellation.** Per-call deadlines, a `$/cancel` notification
  and `kill` as the last resort. A timeout or crash during an effectful call is
  an unknown outcome, never a failure and never a retry.
- **DX.** Any language, any libraries, native debugging, and a plugin can be
  tested by piping JSON. Distribution of the executable is the author's
  problem; there is no cross-platform guarantee.
- **Versioning.** A protocol identifier (`genaimage-plugin/1`) negotiated in
  `initialize`, with additive evolution and a JSON Schema for each message.
- **Cost.** The host side is an estimated 400 to 600 lines of Rust plus a protocol
  document and a fake plugin for tests.

### Mapping the guardrails across the boundary

For an assistant tool, **core remains the enforcer** and the plugin is only the
body of `invoke` and `preview`. Core registers an adapter that implements
`AssistantTool` from the plugin's declared metadata:

- `requires_confirmation` and the JSON Schema are read at registration and
  frozen. The existing register-time schema subset validation applies to the
  declaration, and the runtime validates every call's arguments before they
  reach the process. A plugin can never lower `requires_confirmation` after
  registration.
- `preview` is a pure request; core recomputes the `preview_token` itself from
  the returned value. A plugin whose preview is not deterministic fails the
  existing mismatch path.
- `invoke` carries the `confirmation_id` as the idempotency key. The plugin must
  make its external effect idempotent under it. If the process dies, times out
  or the server stops after the request was written, the confirmation settles as
  `outcome_unknown` and is never re-invoked, including after a respawn.
- A plugin cannot submit a run itself. The only generation effect is the
  built-in `submit_run`, which keeps `submit_keyed`, workflow revision pinning
  and the "assistant never chooses image inputs" rule in core. A plugin tool
  that "proposes a run" returns a proposal that core validates against the
  workflow revision shown to the model, dropping invalid items with a note.
- Secrets are passed explicitly or not at all: the LLM API key is never given to
  a plugin, the environment is cleared, and a plugin's own credentials come from
  its declared settings. Plugin stderr is captured, truncated and run through
  the existing redaction step before it reaches logs.
- Results are bounded like built-in tools (32 KiB), and truncated values are
  marked rather than silently shortened.

For an **image backend** the mapping is much harder. `submit` once semantics
and `HistoryOutcome::Unresolved` fit the same unknown-outcome rule, but
`upload_image_bytes` and `download_output` move binary data (a side channel such
as a core-owned temporary file or descriptor is needed, not base64 in JSON),
`monitor_events` is a stream, `with_timing` and the DNS pin are in-process
concepts, and `base_url` must be validated by core against the endpoint policy
rather than trusted from the plugin. There is also no second backend today. The
sound design is a smaller, higher-level backend protocol, not a transport for
the current trait, and it should wait for a concrete need.

## Criteria

| Criterion | A. WASM (wasmtime) | B. JSON-RPC process | C. Compile-time only |
| --- | --- | --- | --- |
| Isolation | Strong: capability sandbox, no ambient fs or net, fuel and memory limits | Process only; plugin has user privileges and can open any socket | None needed; first-party code |
| Loopback policy | Enforced by host imports, if WASI sockets and `wasi:http` are not linked | Not enforceable without OS sandboxing; document as trusted code | Enforced by the existing adapters |
| Confirmation, idempotency, `outcome_unknown` | Fits: core adapter, same mapping; traps map to unknown outcome | Fits: core adapter, same mapping; crash and timeout map to unknown outcome | Native |
| Silent substitution | Core validates declarations and results | Core validates declarations and results | Native |
| Performance | Instantiate 15 to 25 us, call 0.2 us (measured, trivial component, AOT); real components compile in tens to hundreds of ms (not measured) | Spawn 0.23 to 0.32 ms, round trip about 2 us (measured, trivial native plugin); interpreters add 20 to 100 ms start-up (estimated, not measured) | None |
| Binary size vs 25 MB budget | +1.0 MB (no WASI, AOT only, unusable by real plugins) to +4.1 MB (with WASI p2, AOT) to +15.3 MB (WASI, Cranelift); main at 21.2 MB reaches 22.2, 25.3 and 36.5 MB | About 0 MB (existing `tokio` and `serde_json`) | 0 MB |
| Idle memory | +3 to 10 MB RSS with an engine (measured: 3.2 to 11.4 MB against 2.1 MB for an empty binary) | Per plugin process, only while running | 0 |
| Build time | +20 to 75 s clean, 84 to 199 crates (measured) | Negligible | 0 |
| MSRV (workspace 1.80) | wasmtime 51 requires 1.97 | None | None |
| DX for authors | Typed WIT; Rust good, other languages heavier; no native crates | Any language, native tooling | Rust only, recompile |
| Supply chain | Large; JIT and runtime are `unsafe`-heavy | Small; plugin code itself is untrusted | Own code |
| Maintenance | Monthly wasmtime upgrades, WIT and AOT version coupling | Small protocol to version and document | None |

## Measurements

All measured in a scratch directory outside the repository on 2026-11-28,
Linux x86_64, `rustc 1.100.0`, `wasmtime 51.0.0` (with `wasmtime-wasi 51.0.0`,
`tokio 1`), release profile `strip = "symbols"` and `codegen-units = 1` like
the workspace profile, a clean build per variant. The component is a minimal
hand-written WAT component exporting `add(u32, u32) -> u32` (144 bytes;
13,856 bytes as a precompiled `.cwasm`). Binary sizes include a roughly
0.34 MB empty-`main` baseline, so the deltas below are within 0.4 MB of
the added cost.

| Host variant | Wasmtime features | Binary | Delta to main (21.2 MB) | Load or compile | Instantiate | Call | Clean build | Crates |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| empty `main` | none | 0.34 MB | n/a | n/a | n/a | n/a | n/a | n/a |
| AOT, no WASI | `runtime`, `component-model` | 1.29 MB | about +1.0 MB | 60 to 120 us | 7 to 13 us | 30 ns | about 22 s | 84 |
| AOT with WASI p2 | + `async`, `wasmtime-wasi` | 4.47 MB | about +4.1 MB | 58 to 90 us | 15 to 25 us | 200 ns | about 30 s | 166 |
| JIT, Cranelift | `runtime`, `component-model`, `cranelift` | 12.54 MB | about +12.2 MB | 0.5 to 0.7 ms | 5 to 11 us | 30 ns | 69 s | 119 |
| JIT with WASI p2 | + `async`, `wasmtime-wasi` | 15.68 MB | about +15.3 MB | 0.6 to 0.8 ms | 22 to 34 us | 200 ns | 76 s | 199 |
| JIT, default features | defaults | 16.77 MB | about +16.4 MB | 0.6 to 0.8 ms | 9 to 12 us | 195 ns | 75 s | 166 |

The workspace has about 307 unique normal dependencies already (`cargo tree`).
The "call" figure is per call over 100,000 calls, including `post_return`, for a
pure computation; it says nothing about host imports, which cost more.

Peak RSS of a one-shot process (`/usr/bin/time`): empty 2.1 MB, AOT without
WASI 3.2 MB, AOT with WASI 5.2 MB, JIT 11.4 MB, JIT default 12.9 MB.

JSON-RPC stdio, two release Rust processes, a host that spawns a plugin that
echoes each newline-delimited request, single-threaded blocking I/O, a 120-byte
`tool.invoke` request, 20,000 sequential round trips, three runs:

| Measure | Result |
| --- | --- |
| Spawn plus first round trip | 0.23 to 0.32 ms |
| Round trip p50 | 1.75 to 2.12 us |
| Round trip p95 | 2.3 to 3.4 us |
| Round trip p99 | 3.1 to 3.9 us |

These round-trip figures are a lower bound: no JSON parsing, no Tokio, an
idle machine, a native plugin. Expect tens of microseconds in the real server,
negligible against LLM and ComfyUI latency and the 20 ms p95 API budget.
Interpreted plugin start-up (Python, Node) is an estimate and was not measured.

Method and caveats:

- Binary sizes are for a standalone host, not the real server. In the real
  binary some code is shared (`std`, `tokio`, `serde`), so the true delta can
  be slightly smaller. The deltas are not linear if features are added.
- The AOT hosts load a `.cwasm` compiled by a tool with the same wasmtime
  version and features. A first attempt failed with "module was compiled with GC
  however GC is disabled in the host" and then "compiled without concurrency
  support but it is enabled for the host", confirming that precompiled
  artefacts are bound to an exact engine configuration.
- A real plugin written in Rust for `wasm32-wasip2` imports WASI interfaces, so
  "AOT, no WASI" is a lower bound, not a viable host. A real component also
  needs host imports for HTTP, which were not built.
- Build times are approximate: variants shared one dependency cache, so the
  lighter variants benefit from earlier builds. They are dominated by
  dependencies on a 12-thread machine. Not measured: real-component compile time, memory of a loaded
  real component, async host-call overhead, `cargo component` build times.

## Decision

1. **Do not adopt a runtime plugin model now.** There is no concrete third-party
   plugin or second backend, the compile-time registry covers first-party
   needs, and both runtime options add a guardrail surface that is easier to
   design against a real use case. Option C stays the supported model and
   ADR 0001 stands.
2. **If a runtime model is needed, use external processes over JSON-RPC (B),
   for assistant tools only.** It costs about 0 bytes of binary, fits the
   thin-harness budgets, keeps `rust-version = 1.80`, works in any language and
   maps cleanly onto the existing confirmation protocol because core stays the
   enforcer. Its weakness is isolation: plugins are trusted code, the loopback
   policy cannot be enforced against them and this must be stated in the
   documentation and in the install and enablement flow.
3. **Do not adopt WASM (A) in the main binary.** It breaches the binary budget
   in every viable configuration (25.3 MB with WASI and AOT only, 36.5 MB with
   Cranelift, against a 25 MB limit and 21.2 MB baseline), needs a toolchain
   17 releases newer than the workspace MSRV, and adds 84 to 199 crates. Its
   isolation and structural loopback enforcement are real advantages. If strong
   sandboxing becomes necessary, run wasmtime in a separate optional helper
   executable that speaks the same JSON-RPC protocol, so the main binary stays
   within budget and the plugin protocol is shared.
4. **Image backends stay compile-time.** Their trait surface, binary transfers,
   streams and provenance rules do not map onto a generic transport. Revisit
   when a second real backend exists, with a purpose-built protocol.

The protocol outline for B, to be written in a follow-up and not binding yet:

- Transport: newline-delimited JSON-RPC 2.0 on the child's stdin and stdout;
  stderr is diagnostics only.
- Methods: `initialize` (negotiates `genaimage-plugin/1`, returns the tool
  list with schemas and `requires_confirmation`), `tool.preview`,
  `tool.invoke` (with `confirmation_id`), `$/cancel`, `shutdown`.
- Registration: explicit, user-enabled plugin entries in configuration naming
  an executable path and optional SHA-256. No discovery, no download, no
  installation by GenAImage, and no fallback names.
- Process: cleared environment, no inherited secrets, working directory set to
  a plugin-owned directory, per-call timeouts and bounded response sizes.

## Consequences

- No code, dependency or binary-size change now. `extending.md` keeps the
  compile-time model as the only supported one and links this ADR.
- The follow-up draft (not filed; kept outside the repository until a trigger or
  a maintainer decision) describes an experimental `process` tool provider.
- Plugins, if adopted, are trusted code. Documentation, the portal and the
  doctor checks must say so, and the assistant API must show which tools come
  from a plugin.
- Declaring protocol and schema now commits the project to a compatibility
  surface; keep it small and versioned.

## Revisit when

- A real third party needs tools or backends without forking, or a first-party
  feature (for example a second image backend) would benefit from isolation.
- Untrusted plugins must be run, so process isolation is not enough: reconsider
  A as an optional helper executable, or OS sandboxing for B.
- Wasmtime's MSRV, binary footprint or a stable component-model async story
  changes the numbers; re-run the measurements above.
- The workspace MSRV is raised above 1.97 for other reasons.
- The binary budget or headroom changes materially.
