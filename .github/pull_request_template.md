## Summary

<!-- Summarise the changes and their purpose. -->

## Linked issue

Closes #

## Validation

<!-- List the commands run and their results. -->

- `python3 scripts/ci_check.py`

## Checklist

- [ ] `python3 scripts/ci_check.py` passes, or any unrelated failure is explained.
- [ ] Documentation and user-facing text use English (UK).
- [ ] Copilot and Claude skill copies are identical if skills were added or changed.
- [ ] Project guardrails are respected: generation remains explicit, ambiguous submissions are reconciled, existing run records are preserved, services stay local and loopback-bound, and generation logic remains in `genaimage-core`.
