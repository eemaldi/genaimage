import { afterEach, expect, test, vi } from 'vitest';
import { randomSeed, type Parameter } from './model';

const seed = (extra: Partial<Parameter> = {}): Parameter => ({
  node: '1',
  input: 'seed',
  kind: 'seed',
  required: true,
  description: '',
  ...extra,
});

function fill(bytes: ArrayBufferView | null, value: bigint) {
  if (!(bytes instanceof Uint8Array)) throw new Error('Expected random bytes');
  let rest = value;
  for (let i = bytes.length - 1; i >= 0; i--) {
    bytes[i] = Number(rest & 255n);
    rest >>= 8n;
  }
  return bytes;
}

function drawing(value: bigint) {
  return vi
    .spyOn(crypto, 'getRandomValues')
    .mockImplementation((bytes) => fill(bytes, value) as typeof bytes);
}

afterEach(() => vi.restoreAllMocks());

test('draws from the full unsigned 64-bit range with exact edge values', () => {
  drawing(0n);
  expect(randomSeed(seed())).toBe(0);
  drawing(18446744073709551615n);
  expect(randomSeed(seed())).toBe(18446744073709551615n);
});

test('keeps draws inside declared bounds at exact endpoints above 2^53', () => {
  const bounded = seed({ minimum: 2 ** 53, maximum: 2 ** 60 });
  drawing(0n);
  expect(randomSeed(bounded)).toBe(9007199254740992n);
  drawing(2n ** 60n - 2n ** 53n);
  expect(randomSeed(bounded)).toBe(1152921504606846976n);
});

test('intersects declared bounds with the unsigned 64-bit range', () => {
  drawing(18446744073709551615n);
  expect(randomSeed(seed({ minimum: -(2 ** 63), maximum: 2 ** 64 }))).toBe(
    18446744073709551615n,
  );
});

test('uses only the integers that satisfy negative and fractional bounds', () => {
  drawing(0n);
  expect(randomSeed(seed({ minimum: -10, maximum: 3 }))).toBe(0);
  drawing(3n);
  expect(randomSeed(seed({ minimum: -10, maximum: 3 }))).toBe(3);
  drawing(0n);
  expect(randomSeed(seed({ minimum: 0.5, maximum: 2.9 }))).toBe(1);
  drawing(1n);
  expect(randomSeed(seed({ minimum: 0.5, maximum: 2.9 }))).toBe(2);
});

test.each([0n, 1n, 2n ** 62n])(
  'never returns a negative seed (draw %s)',
  (draw) => {
    drawing(draw);
    expect(BigInt(randomSeed(seed({ minimum: -(2 ** 63) }))!)).toBe(draw);
  },
);

test('returns seeds above 2^53 as exact bigints', () => {
  drawing(2n ** 53n + 1n);
  const result = randomSeed(seed());
  expect(typeof result).toBe('bigint');
  expect(result).toBe(9007199254740993n);
});

test.each([
  ['an inverted range', { minimum: 5, maximum: 4 }],
  ['a minimum above 2^64-1', { minimum: 2 ** 64 }],
  ['no integer between fractional bounds', { minimum: 0.2, maximum: 0.8 }],
])('fails clearly for %s instead of clamping', (_label, bounds) => {
  const random = vi.spyOn(crypto, 'getRandomValues');
  expect(() => randomSeed(seed(bounds))).toThrowError(
    /leave no seed between 0 and 18446744073709551615/,
  );
  expect(random).not.toHaveBeenCalled();
});

test('chooses only allowed values that are valid seeds', () => {
  expect(
    randomSeed(seed({ allowed_values: [2n ** 64n, 18446744073709551615n] })),
  ).toBe(18446744073709551615n);
});
