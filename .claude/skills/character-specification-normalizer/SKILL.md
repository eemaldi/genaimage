---
name: character-specification-normalizer
description: Normalise user-supplied character design material into a traceable specification, without inventing details; use during character concept-art Gate 1 and Gate 2.
---

# Character Specification Normalizer

Turn the user's existing character description and references into a clear, source-traceable Character Design Specification. Work only from material supplied or explicitly approved by the user. Do not add plausible details, resolve contradictions by choosing a source, or treat silence as consent.

## Procedure

1. **Check the input package (Gate 1).** Use `templates/character-concept-art/input-package.md`. Identify the original description, mandatory and flexible elements, open questions, references and their relevant features, conflicts, and missing information. Do not begin normalisation until the user explicitly approves Gate 1. If information is insufficient or contradictory, list the exact issue and ask the user to clarify or explicitly accept it as open. Keep the workflow at Phase 01 meanwhile.
2. **Trace each supplied design choice.** Organise it under the design-specification template's sections: anatomy; silhouette and posture; head design; clothing; equipment; materials; colour palette; distinguishing details. For each choice, identify its source (description, reference ID and relevant feature, or explicit user direction). Quote or paraphrase faithfully; do not infer beyond the evidence.
3. **Classify every supplied choice:**
   - **LOCKED** — explicitly required and not to change.
   - **FLEXIBLE** — interpretation is allowed only within stated, source-supported bounds. State those bounds concretely; if the user has not supplied them, ask rather than invent them.
   - **OPEN** — the user intentionally leaves the choice for exploration. State what is open and any stated limits. Missing information is not automatically OPEN.
   
   A field with no supported choice remains **unspecified** and is listed as an unknown, not filled with a guess. Record each source conflict with the alternatives and their origins; neither choose nor reconcile it without the user's direction.
4. **Fill the specification template.** Copy `templates/character-concept-art/design-specification.md` to the character's `specifications/` area. Preserve the character ID, version, input-package link, all applicable sections, and the template's contradictions, assumptions, exclusions, and Gate 2 record. Use “Do not invent” to state unresolved items and boundaries. Keep the production record's classification lists, pending issues, current phase/gate, and approval status consistent with `templates/character-concept-art/production-record.md`.
5. **Stop at Gate 2.** Present the complete specification and request explicit user approval or requested changes. Do not prepare a Visual Generation Brief until Gate 2 is explicitly approved. On rejection or ambiguity, revise the current specification or return to input acquisition as needed; record the decision, approver, date, and conditions. An assistant's assessment or silence is not approval.

## Output checks

- Every stated design choice has exactly one classification and traceable source.
- Every FLEXIBLE choice has explicit bounds; every OPEN choice is intentionally open.
- Unknowns and conflicts are visible, not converted into assumptions.
- No unsupported details have been introduced.
- Gate 1 and Gate 2 status and decisions follow the workflow; no later phase is implied to be approved.

Follow `workflows/character-concept-art/WORKFLOW.md` and the referenced templates. Do not generate or edit images; image operations require Gate 3 approval.
