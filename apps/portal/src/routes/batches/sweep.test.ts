import { expect, test } from 'vitest';
import { BindingFloat, stringifyBindingJson } from '../../lib/api/client';
import type { Parameter } from '../generate/model';
import { parseValues, randomSeeds, requiresExactWrites } from './sweep';

const parameter = (
  kind: string,
  extra: Partial<Parameter> = {},
): Parameter => ({
  kind,
  node: '1',
  input: 'x',
  description: '',
  required: false,
  ...extra,
});

test('a seed list is parsed exactly, including the largest seed', () => {
  const { values, errors } = parseValues(
    parameter('seed'),
    '18446744073709551615\n\n7\r\n9007199254740993',
  );
  expect(errors).toEqual([]);
  expect(values).toEqual([
    18_446_744_073_709_551_615n,
    7,
    9_007_199_254_740_993n,
  ]);
  expect(stringifyBindingJson(values)).toBe(
    '[18446744073709551615,7,9007199254740993]',
  );
});

test('seed lines that are not whole numbers or are repeated are reported by line', () => {
  const { errors } = parseValues(
    parameter('seed'),
    '1\nabc\n1\n18446744073709551616',
  );
  expect(errors).toEqual([
    'Line 2: Enter a whole number.',
    'Line 3: 1 is repeated.',
    'Line 4: Enter a whole number from -9223372036854775808 to 18446744073709551615.',
  ]);
});

test('decimal spelling survives parsing so 2.0 stays a float and 2 stays an integer', () => {
  const floats = parseValues(parameter('number'), '2.0\n3\n-0.0\n0.5');
  expect(floats.errors).toEqual([]);
  expect(stringifyBindingJson(floats.values)).toBe('[2.0,3,-0.0,0.5]');
  expect(floats.values[0]).toBeInstanceOf(BindingFloat);
});

test('values are validated against the parameter bounds and kind', () => {
  expect(
    parseValues(parameter('integer', { minimum: 1, maximum: 10 }), '5\n11\n2.5')
      .errors,
  ).toEqual([
    'Line 2: Enter a value of at most 10.',
    'Line 3: Enter a whole number.',
  ]);
  expect(parseValues(parameter('boolean'), 'true\nmaybe').errors).toEqual([
    'Line 2: Enter true or false.',
  ]);
  expect(parseValues(parameter('integer'), '  \n').errors).toEqual([
    'Enter at least one value.',
  ]);
});

test('random seeds are drawn by the caller, distinct and never hidden', () => {
  let next = 1;
  const added = randomSeeds(parameter('seed'), 3, () => next++, [2]);
  expect(added).toEqual([1, 3, 4]);
});

test('only exact numbers need exact writes', () => {
  expect(requiresExactWrites([1, 'a', true])).toBe(false);
  expect(requiresExactWrites([1n])).toBe(true);
  expect(requiresExactWrites([new BindingFloat(2)])).toBe(true);
});
