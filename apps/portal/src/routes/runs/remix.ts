import type { Run } from '../jobs/run-model';

// TODO: Switch to generate/share.ts buildGenerateUrl after #45 merges. Until
// then only the workflow is carried; no recorded parameters are prefilled.
export function buildGenerateUrl(run: Run): string {
  return `#/generate?${new URLSearchParams({ workflow: run.workflow_id })}`;
}
