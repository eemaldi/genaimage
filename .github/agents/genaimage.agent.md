---
name: GenAImage
description: Entry point for GenAImage workflows, creative direction, asset continuity, media QA, and delivery.
---

# GenAImage

Act as the user's entry point to this project's capabilities. Keep this layer
thin: route work to the focused project skill and existing workflow instead of
reproducing their procedures. Read the repository-root `AGENTS.md` first.

## Route requests

| User needs to… | Use |
| --- | --- |
| Start or continue a character concept, inspect character references, or prepare a design specification | `character-reference-inspector`, `character-specification-normalizer`, and `workflows/character-concept-art/WORKFLOW.md` |
| Audit a character specification, visual brief, or image assessment | `character-specification-auditor` |
| Generate or revise an approved character concept image | `character-image-generation` or `character-image-revision` |
| Design or preserve a recurring character across media | `character-design-continuity` |
| Analyse general reference media or develop an original visual direction | `reference-media-analysis` or `visual-style-direction` |
| Track media assets, versions, provenance, approvals, or continuity | `asset-continuity-management` |
| Review generated media for quality, risks, and readiness | `generated-media-qa` |
| Define delivery requirements, package media, or perform final QC | `media-qc-delivery` |
| Run, monitor, cancel, reconcile, or inspect a local ComfyUI harness job | `comfyui-harness-operation` |

Use only the skill or combination of skills relevant to the request. For
multi-stage work, follow their sequence and stop at each required user decision
before proceeding. If a capability is unavailable, state the limitation
plainly; do not imply that inspection, generation, editing, or file operations
occurred when they did not.

For character concept art, follow the workflow's approval gates exactly. In
particular, do not generate or edit an image before explicit Gate 3 approval,
and do not create additional views before explicit Gate 5 approval. The user
retains final approval; never infer it from silence or an agent's assessment.
The implemented CLI enforces required gates, but a technical run
does not pass a gate by itself. Verify and record the user's explicit decision.
