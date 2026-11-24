import type { SubmitRunRequest } from '../../lib/api/client';
import type { components } from '../../lib/api/schema';

export type Run = components['schemas']['RunResponse'];
export type RunStatus = Run['status'];

export const statuses = {
  created: { label: 'Created', icon: '○' },
  submitting: { label: 'Submitting', icon: '↑' },
  unknown_submission: { label: 'Submission uncertain', icon: '?' },
  queued: { label: 'Queued', icon: '◷' },
  running: { label: 'Running', icon: '▶' },
  completed: { label: 'Completed', icon: '✓' },
  failed: { label: 'Failed', icon: '!' },
  cancelling: { label: 'Cancelling', icon: '…' },
  cancelled: { label: 'Cancelled', icon: '×' },
} satisfies Record<RunStatus, { label: string; icon: string }>;

export function isRunStatus(status: string): status is RunStatus {
  return Object.hasOwn(statuses, status);
}

export function describeStatus(status: string) {
  return isRunStatus(status)
    ? statuses[status]
    : { label: 'Unknown status', icon: '?' };
}

export const canCancel = (status: string) =>
  status === 'queued' || status === 'running';

// Today's endpoint returns only the run. An unchanged unknown_submission is
// not proof of absence: both missing and unresolved history produce that state.
export type ReconciledRun = Run & {
  reconciliation_outcome?: 'not_found' | 'ambiguous' | 'adopted';
};

export function reconcileOutcome(run: ReconciledRun) {
  if (
    run.status === 'unknown_submission' &&
    run.reconciliation_outcome === 'not_found'
  )
    return 'not_found';
  if (
    ['queued', 'running', 'completed', 'failed', 'cancelled'].includes(
      run.status,
    )
  )
    return 'adopted';
  return 'ambiguous';
}

export function submissionFrom(run: Run): SubmitRunRequest | undefined {
  const { workflow_id, parameters } = run;
  if (
    !workflow_id ||
    !parameters ||
    typeof parameters !== 'object' ||
    Array.isArray(parameters)
  )
    return;
  const bindings = parameters as Record<string, unknown>;
  // Effective workflow bindings are scalar; file inputs use upload IDs.
  if (
    !Object.values(bindings).every(
      (value) =>
        typeof value === 'string' ||
        typeof value === 'boolean' ||
        (typeof value === 'number' &&
          Number.isFinite(value) &&
          (!Number.isInteger(value) || Number.isSafeInteger(value))),
    )
  )
    return;
  return { workflow: workflow_id, parameters: bindings };
}
