import type { components } from '../../lib/api/schema';
import { showValue } from './sweep';

type Batch = components['schemas']['BatchResponse'];
type State = Batch['state'];
type Submission = components['schemas']['BatchItemSubmission'];

// These describe submission only. They never claim generation succeeded.
export const stateLabels: Record<State, { label: string; detail: string }> = {
  submitting: {
    label: 'Submitting runs',
    detail: 'GenAImage is sending the runs to ComfyUI one at a time.',
  },
  interrupted: {
    label: 'Submission interrupted',
    detail: 'Some runs have not been sent. Resume to send them.',
  },
  submitted: {
    label: 'All runs sent',
    detail:
      'Every run was handed to ComfyUI. This is not the generation result; check each run below.',
  },
  partial: {
    label: 'Some runs not sent',
    detail:
      'Some runs were sent and others were not or could not be confirmed.',
  },
  failed: {
    label: 'No runs sent',
    detail: 'No run was accepted for submission.',
  },
};

export const submissionLabels: Record<Submission, string> = {
  pending: 'Waiting to be sent',
  submitted: 'Sent',
  failed: 'Not sent: rejected',
  not_submitted: 'Not sent',
  outcome_unknown: 'Outcome unknown',
};

export function describeSweep(sweep: Batch['sweep']): string {
  if ('seeds' in sweep)
    return `Seeds sweep over ${sweep.seeds.parameter}: ${sweep.seeds.values.length} values`;
  return `Grid over ${Object.entries(sweep.grid)
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([name, values]) => `${name} (${values.length})`)
    .join(' × ')}`;
}

export const sweepLabel = (values: Record<string, unknown>) =>
  Object.entries(values)
    .map(([name, value]) => `${name}=${showValue(value)}`)
    .join(', ');

export function canResume(batch: Batch): boolean {
  if (batch.state === 'submitting') return false;
  return (
    batch.state === 'interrupted' ||
    batch.items.some(
      (item) =>
        item.submission === 'not_submitted' ||
        item.submission === 'pending' ||
        (item.submission === 'failed' && !item.run_id),
    )
  );
}

export const needsReconcile = (item: Batch['items'][number]) =>
  item.submission === 'outcome_unknown' && !!item.run_id;
