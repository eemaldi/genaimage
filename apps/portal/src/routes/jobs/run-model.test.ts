import { expect, test } from 'vitest';
import { describeStatus, submissionFrom, type Run } from './run-model';

test('only completed is presented as completion, including for future status values', () => {
  expect(describeStatus('completed').label).toBe('Completed');
  for (const status of ['succeeded', 'toString', 'constructor']) {
    expect(describeStatus(status)).toEqual({
      label: 'Unknown status',
      icon: '?',
    });
  }
});

test('does not offer to replay a binding with lost integer precision', () => {
  const run = { workflow_id: 'example', parameters: { seed: 2 ** 53 } } as Run;
  expect(submissionFrom(run)).toBeUndefined();
});
