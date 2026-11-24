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
