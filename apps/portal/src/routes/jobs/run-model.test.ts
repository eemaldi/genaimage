import { expect, test } from 'vitest';
import { describeStatus } from './run-model';

test('only completed is presented as completion, including for future status values', () => {
  expect(describeStatus('completed').label).toBe('Completed');
  for (const status of ['succeeded', 'toString', 'constructor']) {
    expect(describeStatus(status)).toEqual({
      label: 'Unknown status',
      icon: '?',
    });
  }
});
