// @vitest-environment node
import { expect, it } from 'vitest';
import config from './vite.config';

it.each([
  ['/api', true],
  ['/api/', true],
  ['/api?x', true],
  ['/apix', false],
])('matches the Vite API proxy for %s: %s', (url, expected) => {
  // Vite tests regex contexts and also accepts literal prefix matches.
  const matches = Object.keys(config.server?.proxy ?? {}).some(
    (context) =>
      (context.startsWith('^') && new RegExp(context).test(url)) ||
      url.startsWith(context),
  );
  expect(matches).toBe(expected);
});
