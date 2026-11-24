import { expect, test } from 'vitest';
import { describeStatus, reconcileOutcome, type Run } from './run-model';

test('only completed is presented as completion, including for future status values', () => {
  expect(describeStatus('completed').label).toBe('Completed');
  for (const status of ['succeeded', 'toString', 'constructor']) {
    expect(describeStatus(status)).toEqual({
      label: 'Unknown status',
      icon: '?',
    });
  }
});

test('distinguishes adoption from a positive absence signal and an unresolved run', () => {
  const run = { status: 'unknown_submission' } as Run;
  expect(reconcileOutcome(run)).toBe('ambiguous');
  expect(
    reconcileOutcome({ ...run, reconciliation_outcome: 'not_found' }),
  ).toBe('not_found');
  expect(reconcileOutcome({ ...run, status: 'running' })).toBe('adopted');
  expect(reconcileOutcome({ ...run, status: 'submitting' })).toBe('ambiguous');
});
