---
name: character-specification-auditor
description: Audit a character specification, visual brief, or supplied image assessment against its source package, reporting evidence, uncertainty, missing information, and blockers without replacing human approval.
---

# Character Specification Auditor

Check whether an existing Character Design Specification, Visual Generation Brief, or user-provided image assessment is faithful to its character source package. Audit evidence and workflow readiness; do not rewrite the design, invent evidence, or approve a gate on the user's behalf.

## Procedure

1. **Identify the audit scope and sources.** Record the artifact and version under review, the input package, relevant approved references or their documented descriptions, and applicable approval entries in the production record. If the source package or required approvals are absent, state what is missing and limit conclusions accordingly.
2. **Compare field by field.** Cover every applicable input-package requirement and relevant design-specification field: anatomy, silhouette/posture, head, clothing, equipment, materials, palette, and distinguishing details. For a brief, also check composition, pose/proportions, colour distribution, materials, lighting, detail, rendering, background, preservation instructions, and exclusions. For each field report:
   - source requirement and source location/reference ID;
   - corresponding artifact statement or observation;
   - finding: **matches**, **conflicts**, **unsupported**, **missing**, or **not assessable**;
   - evidence, uncertainty, and any required clarification.
3. **Check classifications and traceability.** Verify that supplied choices are classified LOCKED, FLEXIBLE, or OPEN consistently with the input and that FLEXIBLE bounds are explicit and supported. Flag invented or untraceable statements, unstated bounds, unresolved source conflicts, unknowns presented as facts, and missing required fields. Do not treat absence of a detail as permission to invent it or as user-approved openness.
4. **Handle image evidence honestly.** If the user supplies an image assessment, identify it as reported assessment and audit it against the source package. Claim direct visual findings only when image-inspection capability is actually available and the image was inspected; otherwise mark visual fields **not assessable** and say what image evidence is needed. Never imply that an image was inspected when it was not.
5. **Report blockers and approval status.** Mark as hard failures any contradiction of a LOCKED requirement, unresolved blocking source conflict, material unsupported invention, missing required approval, or other condition that the workflow makes a stop. A score may be included only as supporting information; it cannot override a hard failure or substitute for explicit user approval. Report approval as Pending, Approved, or Rejected only when supported by a recorded user decision; otherwise say “not recorded”.
6. **Respect phase gates.** Use `workflows/character-concept-art/WORKFLOW.md` and `templates/character-concept-art/production-record.md`. No image generation or editing before explicit Gate 3 approval of the complete brief. No additional views before explicit Gate 5 approval. If the relevant approval is absent, pending, or rejected, state that the operation is blocked. Do not infer approval from audit results, silence, or a score.

## Report format

1. **Scope and evidence examined** — artifact/version, source package, references, approval records, and any inspection limits.
2. **Field-by-field findings** — use the fields above; cite section names, reference IDs, or supplied assessment details so each finding is traceable.
3. **Hard failures and missing information** — separate blockers from non-blocking uncertainty; state the evidence needed to resolve each.
4. **Workflow status** — gate status as documented, operations currently blocked or allowed by recorded approvals, and the user's next decision.

Use the existing templates as the checklist: `input-package.md`, `design-specification.md`, `visual-brief.md`, `production-record.md`, and, for image revisions, `revision-review.md`. The user's artistic judgement and explicit gate decisions remain authoritative.
