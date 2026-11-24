import { expect, test, vi } from 'vitest';
import { bindingError, randomSeed, type Parameter } from './model';

const seed = (bound: bigint): Parameter => ({
  node: '1',
  input: 'seed',
  kind: 'seed',
  required: true,
  description: '',
  minimum: Number(bound),
  maximum: Number(bound),
});

const cases = [
  [2n ** 53n, 2n ** 53n + 1n],
  [-(2n ** 63n), -(2n ** 63n) + 512n],
  [2n ** 64n - 1024n, 2n ** 64n - 1n],
] as const;

test.each(cases)(
  'samples the first and last f64-compatible seed (%s … %s)',
  (first, last) => {
    const parameter = seed(first);
    const fillOffset = (bytes: ArrayBufferView | null, offset: bigint) => {
      if (!(bytes instanceof Uint8Array))
        throw new Error('Expected random bytes');
      for (let i = bytes.length - 1; i >= 0; i--) {
        bytes[i] = Number(offset & 255n);
        offset >>= 8n;
      }
      return bytes;
    };
    const random = vi.spyOn(crypto, 'getRandomValues');
    try {
      random.mockImplementation(
        (bytes) => fillOffset(bytes, 0n) as typeof bytes,
      );
      expect(randomSeed(parameter)).toBe(first);
      random.mockImplementation(
        (bytes) => fillOffset(bytes, last - first) as typeof bytes,
      );
      expect(randomSeed(parameter)).toBe(last);
      expect(bindingError(parameter, { value: first })).toBeUndefined();
      expect(bindingError(parameter, { value: last })).toBeUndefined();
      expect(bindingError(parameter, { value: first - 1n })).toBeDefined();
      expect(bindingError(parameter, { value: last + 1n })).toBeDefined();
      expect(
        randomSeed({ ...parameter, allowed_values: [last + 1n, last] }),
      ).toBe(last);
    } finally {
      random.mockRestore();
    }
  },
);
