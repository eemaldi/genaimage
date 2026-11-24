import { expect, test } from 'vitest';
import { BindingFloat } from '../../lib/api/client';
import { buildSuggestionGenerateUrl, restoreFormState } from './share';
import type { Workflow } from './model';

const workflow: Workflow = {
  revision: 'current-revision',
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

test('a linked revision is informational and does not invalidate exact values', () => {
  const query = new URLSearchParams({
    v: '2',
    workflow: workflow.id,
    wr: 'old-revision',
    p: JSON.stringify([['seed', 'u', ['int', '9007199254740993']]]),
  });
  const state = restoreFormState(workflow, query);
  expect(state.revisionNotice).toMatch(/revision differs/);
  expect(state.warnings).toEqual([]);
  expect(state.fields.get('seed')?.value).toBe(9007199254740993n);
});

test('mismatched suggestion links stage exact scalars without changing active fields', () => {
  const exactWorkflow: Workflow = {
    ...workflow,
    presets: { Studio: { strength: 0.25 } },
    parameters: {
      seed: workflow.parameters.seed,
      strength: {
        node: '2',
        input: 'value',
        kind: 'number',
        required: true,
        description: '',
        default: 0,
      },
      offset: {
        node: '3',
        input: 'value',
        kind: 'number',
        required: true,
        description: '',
        default: 0,
      },
      image: {
        node: '4',
        input: 'image',
        kind: 'image_upload',
        required: false,
        description: '',
      },
    },
  };
  const hash = buildSuggestionGenerateUrl({
    workflow_id: exactWorkflow.id,
    workflow_revision: 'suggested-revision',
    rank: 1,
    preset: 'Studio',
    parameters: {
      seed: 9007199254740993n,
      offset: new BindingFloat(-0),
    },
    rationale: 'Exact values',
  });
  const query = new URLSearchParams(hash.split('?')[1]);
  const restored = restoreFormState(exactWorkflow, query);
  expect(query.get('assistant')).toBe('suggest');
  expect(query.get('wr')).toBe('suggested-revision');
  expect(restored.suggested).toBe(true);
  expect(restored.warnings).toEqual([]);
  expect(restored.revisionNotice).toMatch(/revision differs/);
  expect(restored.suggestionNotice).toMatch(/explicit approval/);
  expect(restored.preset).toBe('');
  expect(restored.fields.get('seed')?.value).toBeUndefined();
  expect(restored.fields.get('strength')?.value).toBe(0);
  expect(restored.fields.get('strength')?.source).toBe('default');
  expect(restored.fields.get('offset')?.value).toBe(0);
  expect(restored.pendingSuggestion).toMatchObject({
    suggestedRevision: 'suggested-revision',
    loadedRevision: 'current-revision',
    preset: 'Studio',
  });
  expect(restored.pendingSuggestion?.fields.get('seed')?.value).toBe(
    9007199254740993n,
  );
  expect(restored.pendingSuggestion?.fields.get('offset')?.value).toEqual(
    new BindingFloat(-0),
  );
  expect(restored.fields.get('image')?.value).toBeUndefined();
});

test('a suggestion pinned to the loaded revision prefill applies immediately for editing', () => {
  const hash = buildSuggestionGenerateUrl({
    workflow_id: workflow.id,
    workflow_revision: workflow.revision,
    rank: 1,
    parameters: { seed: 9007199254740993n },
    rationale: 'Exact value',
  });
  const restored = restoreFormState(
    workflow,
    new URLSearchParams(hash.split('?')[1]),
  );
  expect(restored.pendingSuggestion).toBeUndefined();
  expect(restored.fields.get('seed')?.value).toBe(9007199254740993n);
  expect(restored.warnings).toEqual([]);
});
