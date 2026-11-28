# Documentation index

## Using GenAImage

| Document | Contents |
| --- | --- |
| [Project README](../README.md) | What GenAImage is, quick starts for Compose and a binary. |
| [Setup](setup.md) | Running the server, container, releases, configuration files, setup wizard, legacy migration. |
| [Docker Compose](compose.md) | The ComfyUI, Ollama and GenAImage stack, accelerators (CPU, NVIDIA, ROCm), volumes. |
| [Models](models.md) | Placing ComfyUI checkpoints, pulling LLM models, nothing downloads implicitly. |
| [Troubleshooting](troubleshooting.md) | Doctor checks by id, runs in `unknown_submission`, Compose and ROCm problems. |

## Reference

| Document | Contents |
| --- | --- |
| [Configuration](configuration.md) | Every key, environment variable, default, restart rule and allow-list rule. |
| [CLI](cli.md) | Commands, options, JSON shapes, exit codes, shell completions. |
| [API](api.md) | Endpoint overview and the [OpenAPI contract](api/openapi.json). |
| [Workflow packs](packs.md) | `pack.toml`, pack roots, multi-root discovery, requirement validation. |
| [Prompt templates](templates.md) | Template files, `{{name}}` placeholders and escaping, user and pack scopes, the API and its problems. |
| [Workflow manifest](workflow-manifest.md) | Manifest v2 fields, presets, outputs, and its [JSON Schema](schemas/workflow-manifest.v2.json). |
| [Extending GenAImage](extending.md) | Compile-time extension traits, registration, fake-server testing and the compiled example. |

## Contributing

| Document | Contents |
| --- | --- |
| [AGENTS.md](../AGENTS.md) | Shared rules for contributors and coding agents. |
| [ADR 0001](adr/0001-target-architecture.md), [ADR 0002](adr/0002-comfyui-image.md), [ADR 0003](adr/0003-runtime-plugin-model.md) | Architecture decisions (0003 is proposed). |
| [Architecture](architecture.md) | Crates, server, API behaviour, state, portal, events. |
| [Testing](testing.md) | Test strategy, assistant scenarios, opt-in live LLM tests. |
| [Performance](performance.md) | Budgets and how they are measured. |
| [Releasing](releasing.md) | Dry runs, cutting and recovering a release. |
