import { expect, test } from 'vitest';
import { restoreFormState } from './share';
import type { Workflow } from './model';

const workflow: Workflow = {
  id: 'workflow',
  workflow: 'workflow.json',
  version: '1',
  title: 'Workflow',
  schema_version: 2,
  outputs: [],
  presets: {},
  parameters: {
    seed: {
      node: '1',
      input: 'seed',
      kind: 'seed',
      required: true,
      description: '',
    },
  },
};

const restore = (p: string) =>
  restoreFormState(
    workflow,
    new URLSearchParams({ v: '2', workflow: workflow.id, p }),
  );

test.each([
  '9007199254740991.1',
  '["float","9007199254740991.1"]',
  '["float","42.0"]',
  '["int","01"]',
])('never coerces an ambiguous or floating-point seed: %s', (token) => {
  const state = restore(`[["seed","u",${token}]]`);
  expect(state.warnings).toHaveLength(1);
  expect(state.chosenSeeds.has('seed')).toBe(false);
});

test('rejects oversized fragments and entry counts before restoring any values', () => {
  const long = restore(JSON.stringify([['seed', 'u', 'x'.repeat(33_000)]]));
  expect(long.warnings[0].message).toMatch(/too long/);
  const many = restore(
    JSON.stringify(
      Array.from({ length: 257 }, () => ['seed', 'u', ['int', '42']]),
    ),
  );
  expect(many.warnings[0].message).toMatch(/too many parameters/);
  expect(many.fields.get('seed')?.value).toBeUndefined();
});
