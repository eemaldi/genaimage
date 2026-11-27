import {
  formatExactJson,
  supportsExactNumberWrites,
  BindingFloat,
} from '../../lib/api/client';
import type { Scalar, Workflow } from '../generate/model';

export const starterWorkflow = 'text-to-image';
export const checkpointParameter = 'checkpoint';
export const promptParameter = 'positive_prompt';
export const defaultTestPrompt =
  'A small wooden boat on a calm lake at sunrise, soft mist, detailed';

export type TestRow = {
  name: string;
  value: Scalar;
  origin: 'your checkpoint choice' | 'test prompt' | 'workflow default';
};

function scalar(value: unknown): Scalar | undefined {
  return typeof value === 'string' ||
    typeof value === 'boolean' ||
    typeof value === 'number' ||
    typeof value === 'bigint' ||
    value instanceof BindingFloat
    ? value
    : undefined;
}

/** Every value the test run will send, derived from the fetched manifest. */
export function testRows(
  workflow: Workflow,
  checkpoint: string,
  prompt: string,
): { rows: TestRow[]; blockers: string[] } {
  const rows: TestRow[] = [];
  const blockers: string[] = [];
  for (const name of [checkpointParameter, promptParameter])
    if (!workflow.parameters[name])
      blockers.push(`The workflow has no "${name}" parameter.`);
  for (const [name, parameter] of Object.entries(workflow.parameters)) {
    if (name === checkpointParameter)
      rows.push({ name, value: checkpoint, origin: 'your checkpoint choice' });
    else if (name === promptParameter)
      rows.push({ name, value: prompt, origin: 'test prompt' });
    else {
      const value = scalar(parameter.default);
      if (value !== undefined)
        rows.push({ name, value, origin: 'workflow default' });
      else if (parameter.required)
        blockers.push(
          `The required parameter "${name}" has no default for the test.`,
        );
    }
  }
  if (
    rows.some(
      ({ value }) => typeof value === 'bigint' || value instanceof BindingFloat,
    ) &&
    !supportsExactNumberWrites()
  )
    blockers.push(
      'This browser cannot send exact JSON numbers. Update your browser.',
    );
  return { rows, blockers };
}

export function parametersOf(rows: TestRow[]): Record<string, Scalar> {
  return Object.freeze(
    Object.fromEntries(rows.map(({ name, value }) => [name, value])),
  );
}

export function signatureOf(rows: TestRow[]): string {
  return formatExactJson(parametersOf(rows), 0);
}

export function showValue(value: Scalar): string {
  return value === '' ? '(empty text)' : String(value);
}
