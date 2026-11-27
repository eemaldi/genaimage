import { BindingFloat, formatExactJson } from '../../lib/api/client';
import {
  bindingError,
  numericDraft,
  sameValue,
  type Parameter,
  type Scalar,
} from '../generate/model';

export const maxItemsCap = 256;
export const maxRandomSeeds = 64;

export type Sweep =
  | { kind: 'seeds'; parameter: string; text: string }
  | { kind: 'grid'; texts: Readonly<Record<string, string>> };

export type ParsedValues = { values: Scalar[]; errors: string[] };

const lines = (text: string) =>
  text
    .split(/\r?\n/)
    .map((line, index) => ({ line: line.trim(), number: index + 1 }))
    .filter(({ line }) => line !== '');

type Line = { value: Scalar } | { error: string };

function parseLine(parameter: Parameter, text: string): Line {
  switch (parameter.kind) {
    case 'boolean':
      if (text === 'true') return { value: true };
      if (text === 'false') return { value: false };
      return { error: 'Enter true or false.' };
    case 'string':
    case 'seed':
    case 'integer':
    case 'number': {
      const field =
        parameter.kind === 'string'
          ? { value: text }
          : numericDraft(parameter, text);
      const error = bindingError(parameter, field);
      if (error) return { error };
      return field.value === undefined
        ? { error: 'Enter a value.' }
        : { value: field.value };
    }
    default:
      return { error: 'This parameter type cannot be swept.' };
  }
}

/** One value per line, parsed exactly. Blank lines are skipped. */
export function parseValues(parameter: Parameter, text: string): ParsedValues {
  const values: Scalar[] = [];
  const errors: string[] = [];
  for (const { line, number } of lines(text)) {
    const parsed = parseLine(parameter, line);
    if ('error' in parsed) {
      errors.push(`Line ${number}: ${parsed.error}`);
    } else if (values.some((previous) => sameValue(previous, parsed.value))) {
      errors.push(`Line ${number}: ${line} is repeated.`);
    } else values.push(parsed.value);
  }
  if (!values.length && !errors.length)
    errors.push('Enter at least one value.');
  return { values, errors };
}

export function valuesText(values: readonly Scalar[]): string {
  return values.map(String).join('\n');
}

export function expectedCount(parsed: readonly ParsedValues[]): number {
  return parsed.reduce((total, { values }) => total * values.length, 1);
}

/** Visible, client-side random seeds. The caller shows them in the list. */
export function randomSeeds(
  parameter: Parameter,
  count: number,
  draw: (parameter: Parameter) => Scalar | undefined,
  existing: readonly Scalar[],
): Scalar[] {
  const added: Scalar[] = [];
  for (
    let attempts = 0;
    added.length < count && attempts < count * 20;
    attempts++
  ) {
    const value = draw(parameter);
    if (value === undefined) break;
    if ([...existing, ...added].some((other) => sameValue(other, value)))
      continue;
    added.push(value);
  }
  return added;
}

export function sweepNames(sweep: Sweep): string[] {
  return sweep.kind === 'seeds' ? [sweep.parameter] : Object.keys(sweep.texts);
}

export function requiresExactWrites(values: Iterable<unknown>): boolean {
  for (const value of values) {
    if (typeof value === 'bigint' || value instanceof BindingFloat) return true;
  }
  return false;
}

/** Display a scalar or JSON value without losing its spelling. */
export function showValue(value: unknown): string {
  if (value === '') return '(empty text)';
  if (typeof value === 'string') return value;
  try {
    return formatExactJson(value, 0);
  } catch {
    return String(value);
  }
}
