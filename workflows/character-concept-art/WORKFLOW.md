# Character Concept Art Production Workflow

**Version:** 1.0  
**Mode:** Human-in-the-loop  
**Pilot scope:** One character at a time

## Purpose and operating rule

This workflow produces a traceable character concept package suitable for
project development. It is not a prompt-only image generation recipe.

**Hard stop:** do not begin the next phase until the user has explicitly
validated the current phase's output. At numbered gates, this means explicit
approval; at phases without a numbered gate, record the user's review or
validation before continuing. Silence, a high quality score, or an assistant's
judgement is not approval. Record the decision, approver, and date in the
character production record. If a gate or phase review is rejected, address the
stated issues in the current or an earlier phase and submit it again.

Do not generate or edit an image before Gate 3 is approved. Do not create
additional views before Gate 5 is approved. A user may request a return to an
earlier phase at any time; record the reason and preserve previous versions.

## Roles

| Activity | User | Assistant |
| --- | --- | --- |
| Character description and source references | Responsible | Supports and checks |
| Design analysis and normalisation | Validates | Responsible |
| Visual generation brief | Approves | Prepares |
| Image generation and corrections | Directs | Executes when a generation capability is available |
| Evaluation and final acceptance | Responsible | Supports and records |
| Archive and delivery | Validates | Organises |

The user's artistic decision is final. A generation capability is not assumed
to exist: if unavailable, stop at the approved brief and report that image
production is not configured.

## Character workspace

Keep all working material under `characters/<character-id>/`:

```text
characters/<character-id>/
  production-record.md
  input-package.md
  references/
  specifications/
  briefs/
  generations/
  reviews/
  delivery/
```

Use the templates in `templates/character-concept-art/`. Store source material
in its original form where practical and record its origin and approval status.
Do not overwrite a previously approved brief, specification, image, or
delivery. Add a new version and link it to the previous one.

## Phase sequence and gates

### 01 — Input acquisition

**Owner:** User, with assistant support  
**Output:** Character Input Package (`input-package.md`)

Collect the character's existing description, sketches, visual references,
non-negotiable elements, and details open to interpretation. The assistant
organises the material, notes missing information, and identifies conflicts
between text and references. Do not resolve a conflict by silently choosing
one source.

**Gate 1 — Input completeness:** proceed only when the description is sufficient
to analyse, mandatory elements are identified, references are understandable,
and material contradictions are resolved or explicitly accepted as open
questions. If not, remain in Phase 01 and list the missing information.

### 02 — Design analysis and normalisation

**Owner:** Assistant; user validates  
**Output:** Character Design Specification (`specifications/`)

Structure the character across anatomy, silhouette, head design, clothing,
equipment, materials, colour palette, and distinguishing details. Classify
every material design decision:

- **LOCKED:** required and not to be changed.
- **FLEXIBLE:** may be interpreted only within stated bounds.
- **OPEN:** intentionally left for exploration.

Record source and rationale for decisions where useful. Do not generate images
in this phase.

**Gate 2 — Design approval:** the user checks that the specification describes
the intended character and approves it explicitly. Rejected or unclear items
return to analysis or input acquisition; do not prepare a production brief yet.

### 03 — Visual specification

**Owner:** Assistant; user approves  
**Output:** Visual Generation Brief (`briefs/`)

Translate the approved design specification into a visual brief covering
composition and framing, pose and proportions, colour distribution, materials,
lighting, detail level, rendering technique, background, and exclusions. State
how LOCKED elements must be preserved and where FLEXIBLE or OPEN elements may
vary. Target readable development concept art rather than promotional
illustration unless the user approves a different direction.

**Gate 3 — Brief approval:** the user approves the complete brief. No image
generation or editing is allowed before this approval.

### 04 — First generation

**Owner:** Assistant and image-generation capability; user directs  
**Output:** Initial Concept, `v0.1`

Generate from the approved specification and brief. Prioritise silhouette,
proportions, volumes, clothing, palette, and character identity; defer
fine-detail polish. Save the exact approved inputs and generation settings
alongside the output when available. If an image-generation capability is not
configured, stop and report the blocker rather than claiming an image was
created.

The user inspects the image and identifies discrepancies. This is not a
gate-passing phase by itself. Record the user's review in the production record
before entering the correction loop.

### 05 — Evaluation and correction loop

**Owner:** Shared; the user decides acceptance  
**Output:** Numbered revisions and a completed review for each evaluated image

Evaluate description fidelity, silhouette and proportions, clothing and
equipment, materials and palette, and style. For each issue, record the
observed problem, the requested correction, elements to preserve, and priority.
Create a new revision for each iteration; retain the earlier image and its
review.

**Gate 4 — Design acceptance:** proceed only when every LOCKED item is respected,
the principal anatomy and silhouette are correct, there are no critical
errors, and the user explicitly accepts the design. The quality score is a
supporting measure, not a substitute for these conditions.

### 06 — Refinement

**Owner:** Assistant; user approves  
**Output:** Approved Primary Concept

Refine only the approved design: textures, material readability, clothing
details, face and hair, equipment, shadows, and colour contrast. Avoid
unrequested redesign and overworking the image.

**Gate 5 — Visual quality approval:** the user confirms that the primary
concept's finish is sufficient. Do not create additional views before this
approval.

### 07 — Multi-view generation

**Owner:** Assistant and image-generation capability; user validates  
**Output:** Character Multi-view Sheet

Create only the views requested, in this suggested order: front, back, side,
equipment details, then facial expressions. Compare each view with the approved
primary concept and record discrepancies; do not assume independently
generated images preserve geometry or details automatically. Any design change
must return to the relevant earlier gate for approval. Record the user's
multi-view validation before beginning consolidation.

### 08 — Consolidation and delivery

**Owner:** Assistant organises; user validates  
**Output:** Character Concept Package (`delivery/`)

Include the original description, approved specification, approved references,
approved visual brief and prompt, primary concept, revisions and change notes,
additional views, and final notes on definitive features. Link all delivered
assets from the production record. Record final user approval and mark the
character complete only after delivery has been checked.

## Quality review

Score each category from 0 to 5. Calculate the weighted score as
`sum(category score / 5 × category weight)`:

| Category | Weight |
| --- | ---: |
| Fidelity to description | 30% |
| Silhouette and proportions | 25% |
| Clothing and equipment | 15% |
| Materials and palette | 15% |
| Quality and stylistic consistency | 15% |

The indicative passing score is **80/100**. A score of 80 or above never
overrides a failed acceptance condition: any mismatch with a LOCKED element or
any critical error fails Gate 4. The user may reject an image regardless of
score.

## Revision and approval rules

- `v0.1` is the first generation; `v0.2`, `v0.3`, etc. are subsequent
  corrections; `v0.9` is a substantially approved design; `v1.0` is the
  definitive approved design; `v1.1` and later are approved post-release
  changes.
- Keep each version and its associated brief, review, and change list. Never
  replace an approved version in place.
- Record gate status as `Pending`, `Approved`, or `Rejected`, with the decision,
  approver, date, and any conditions.
- Record user reviews after the first generation, multi-view validation, and
  delivery acceptance in the production record as well; these phase reviews
  also block progression until completed.
- A rejected gate keeps the workflow in that phase (or an explicitly selected
  earlier phase). Later phases remain blocked.
- Track the current phase, current gate, version, pending issues, and approved
  references in `production-record.md`.
