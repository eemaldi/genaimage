@AGENTS.md

Follow the shared guidance above and read [ADR 0001](docs/adr/0001-target-architecture.md)
for the accepted target architecture.

The loopback server embeds the Svelte portal by default. It includes the
dashboard, generate, jobs, gallery, workflows, run detail and settings pages,
and an assistant chat panel available on every page. See [setup](docs/setup.md) for source builds
and [portal development](README.md#portal-development) for Vite.

Inside a container with `GENAIMAGE_CONTAINER=1` the server may bind `0.0.0.0`; publish ports on `127.0.0.1` only.
