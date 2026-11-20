---
name: character-image-revision
description: Make a minimal, scoped revision to a character concept image from explicit user direction, preserving approved design elements and recording each new version and review. Use for character-image corrections or refinement in the gated concept art workflow.
---

# Character Image Revision

Use this skill to revise an existing character concept image without turning a
correction into an unapproved redesign. Follow
`workflows/character-concept-art/WORKFLOW.md` and the records in
`templates/character-concept-art/`.

## Check phase, authority, and capability

- Identify the current phase and gate, source image and version, approved
  specification and brief versions, latest review, and any approved canonical
  reference. Confirm the relevant user decision in the production record.
- Do not generate or edit an image before explicit Gate 3 approval. If the Gate
  3 decision or source records are missing, pending, rejected, or ambiguous,
  stop and ask the user for direction or approval.
- Require the user's specific correction or refinement direction. A review
  note can describe a problem, but do not assume it authorises changes beyond
  the requested scope. Ask when the requested correction is unclear or
  conflicts with an approved element.
- Confirm that a compatible image-editing or image-generation capability is
  available. If unavailable, explain the blocker; do not claim an edit occurred.
- Never advance phases or gates autonomously. Ask the user to decide at the
  workflow's existing review points and approval gates.

## Make the smallest authorised change

1. Translate the user's direction into the smallest concrete change that
   addresses the specified issue.
2. List the elements that must remain unchanged, including all unaffected
   LOCKED and already-approved design elements. Compare the source image with
   the approved specification, brief, and canonical reference rather than
   relying on memory or a generic “same character” instruction.
3. Use a mask or region-specific edit only when the available tool explicitly
   supports it. Do not imply a local edit is possible when it is not. If only a
   broader edit is available, explain the likely collateral-change risk and ask
   whether the user wants to proceed or choose another direction.
4. Do not introduce extra pose, costume, prop, expression, composition, style,
   or detail changes unless the user has authorised them. If a necessary change
   would alter an approved feature, pause and request approval before editing.
5. Respect the approved brief and supported tool controls. Call out constraints
   or settings the tool cannot support or verify; do not silently substitute
   them or claim they were satisfied.

## Preserve versions and review evidence

- Never overwrite the source image, any prior revision, or an approved asset.
  Create each confirmed edit as a new revision linked to its parent version,
  using the workflow's versioning scheme.
- Do not claim an edit, saved file, or setting unless the tool confirms it. If
  the attempt fails or no output is returned, report that without recording a
  successful revision.
- For each resulting image, preserve its source references, specification and
  brief versions, requested changes, prompt or instructions, available
  model/provider and version, settings, and unsupported constraints where
  available. Mark unknown details as unknown.
- Create a revision review using
  `templates/character-concept-art/revision-review.md`. Record the image
  version and location, source specification and brief versions, review date,
  observed problem, requested correction, elements to preserve, acceptance
  conditions, and user decision. Update the production record's version history
  and status without replacing prior entries.
- Present the new revision and review for user evaluation. Do not treat a score,
  the edit result, or the assistant's judgement as acceptance or gate approval.

## Refinement and additional views

- Phase 05 corrections address review findings and remain subject to Gate 4
  design acceptance. Phase 06 refinement starts only after Gate 4 acceptance
  and is limited to the approved design. Do not treat one phase's approval as
  another gate's approval.
- Do not generate additional views before explicit Gate 5 visual quality
  approval. When Gate 5 is not explicitly approved and recorded, stop and ask
  the user; do not interpret a request for a revision as Gate 5 approval.
- After Gate 5, generate only the additional views the user requests. Do not
  assume front, side, and back views are consistent with one another or with
  the primary concept. Compare each view individually against the approved
  canonical reference and record discrepancies for user review.
- Ask the user to approve, reject, or direct further changes at each existing
  gate or review point. Keep the workflow at its current phase until they
  explicitly decide.

