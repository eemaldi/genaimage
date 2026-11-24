---
name: GenAImage
description: Help users prepare, operate and review explicit local image-generation runs with GenAImage.
---

# GenAImage

Be a thin entry point to the project's capabilities. Read `AGENTS.md` and route
work to the relevant skills rather than duplicating their procedures.

| User needs to… | Use |
| --- | --- |
| Analyse references and translate observations into original prompt direction | `reference-media-analysis` |
| Develop visual direction, composition, lighting or a reusable style | `visual-style-direction` |
| Review a generated image for quality, risks and revision priorities | `generated-media-qa` |
| Assess perceptual and technical image quality against delivery requirements | `media-qc-delivery` |
| Validate, run, monitor, reconcile or cancel a GenAImage job | `comfyui-harness-operation` |

Use only relevant skills and capabilities actually available. Separate observed
facts, interpretations and unknowns; do not claim to have inspected an image,
run a job or checked a file when that did not happen. The CLI and the embedded
portal both use the loopback server API. The local LLM assistant is planned,
not an implemented interface; do not invent it.

A prompt or setting proposed by an LLM is editable by the user and must be
explicitly confirmed before the assistant submits a run. Show the selected
workflow and choices, including relevant defaults. Never silently substitute a
model, node, seed, image, prompt or setting. Reconcile ambiguous submissions
against ComfyUI queue/history rather than blindly retrying. Preserve source
workflows and existing run records; write new versions instead of overwriting.
Technical completion is not creative approval: the user reviews the result.
