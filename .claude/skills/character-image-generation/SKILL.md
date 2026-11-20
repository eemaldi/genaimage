---
name: character-image-generation
description: Generate a character concept image from an explicitly approved Gate 3 visual brief when a compatible image-generation capability is available. Use for first concepts or other approved single-image generation in the character concept art workflow.
---

# Character Image Generation

Use this skill to carry out an approved character-image generation step in the
human-in-the-loop character concept art workflow. It does not configure or
guarantee access to an image-generation capability.

## Non-negotiable workflow controls

- Follow `workflows/character-concept-art/WORKFLOW.md` and use the
  `templates/character-concept-art/` records.
- Never generate or edit an image before Gate 3 approval.
- Before generating, verify that the current complete visual brief is explicitly
  approved at Gate 3. Check its Gate 3 status, decision, approver, and date in
  the brief and production record. A pending, rejected, missing, or ambiguous
  approval is a hard stop. Ask the user for the missing approval or direction;
  do not infer approval from silence, quality, or earlier approvals.
- Do not move to a later workflow phase or gate on your own. Ask the user for
  direction or approval at the workflow's existing gates and review points.
- Confirm that an image-generation capability is available for this task before
  attempting generation. If none is available, stop at the approved brief and
  report that image production is not configured. Do not imply that an image
  was generated.

## Before generation

1. Identify the character, current phase and gate, approved design
   specification, Gate 3-approved visual brief, and requested output.
2. Check the brief's composition, pose and proportions, silhouette, colours,
   materials, lighting, detail, rendering style, background, LOCKED elements,
   permitted FLEXIBLE interpretation, OPEN elements, and exclusions.
3. Confirm the user's requested scope. Do not fill unresolved contradictions or
   make new design decisions silently. Ask the user when instructions conflict,
   the scope is unclear, or a required constraint cannot be supported.
4. Preserve source inputs and approved records. Do not modify the source
   references, specification, or approved brief as a side effect of generation.
   Record the exact source versions used.
5. Check the available capability's actual controls. Do not assume it supports
   a requested model, reference input, negative prompt, aspect ratio, image
   size, seed, or other setting.

## Generate and record

- Follow the approved brief and prioritise its LOCKED elements. Treat FLEXIBLE
  and OPEN elements only within the approved bounds.
- Use only controls the available capability actually exposes. Before acting,
  call out any requested constraint or setting that is unsupported or cannot be
  verified; ask whether to proceed with that limitation, revise the brief, or
  stop. Do not silently substitute settings or claim unsupported constraints
  were met.
- Do not claim a tool operation, saved file, setting, or result unless the tool
  confirms it. If generation fails, is unavailable, or returns no usable
  output, report that accurately and do not record a successful generation.
- For a confirmed output, preserve it as a new version; never overwrite a
  previous or approved image. Use the workflow's versioning scheme and link the
  output to its parent brief and specification.
- Record the source inputs and their versions, prompt or instructions, available
  model/provider and version, settings, and relevant limitations in the
  production record or associated generation notes when available. Mark
  unavailable details as unknown or unsupported rather than guessing.
- Ask the user to review the output. Record their review before continuing;
  generation itself does not pass a gate or approve the design.

## Approval boundaries

Gate 3 approval authorises only the generation described by the approved brief;
it does not approve the resulting image or authorise later workflow phases.
For each numbered gate and required phase review, present the relevant result
and ask the user to approve, reject, or direct changes. Keep the workflow at its
current phase until the user explicitly decides.

