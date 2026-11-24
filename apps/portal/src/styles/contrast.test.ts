// @vitest-environment node
import { readFileSync } from 'node:fs';
import { expect, it } from 'vitest';

const css = readFileSync(new URL('./tokens.css', import.meta.url), 'utf8');

const colours = (theme: string) =>
  Object.fromEntries(
    [...theme.matchAll(/--([\w-]+): (#\w{6});/g)].map(([, name, hex]) => [
      name,
      hex,
    ]),
  );

function luminance(hex: string) {
  const channels = [1, 3, 5].map((i) => {
    const srgb = parseInt(hex.slice(i, i + 2), 16) / 255;
    return srgb <= 0.04045 ? srgb / 12.92 : ((srgb + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2];
}

const [light, dark] = css.split('@media (prefers-color-scheme: dark)');
it.each([
  ['light', colours(light)],
  ['dark', { ...colours(light), ...colours(dark) }],
] as const)(
  '%s theme semantic pairs meet WCAG AA contrast thresholds',
  (_theme, palette) => {
    function check(foreground: string, background: string, threshold: number) {
      const pair = [
        luminance(palette[foreground]),
        luminance(palette[background]),
      ];
      expect(
        (Math.max(...pair) + 0.05) / (Math.min(...pair) + 0.05),
        `${foreground} on ${background}`,
      ).toBeGreaterThanOrEqual(threshold);
    }
    for (const surface of ['canvas', 'surface', 'subtle']) {
      for (const text of ['ink', 'muted', 'accent']) check(text, surface, 4.5);
      for (const detail of ['border', 'focus']) check(detail, surface, 3);
    }
    check('on-accent', 'accent', 4.5);
    check('danger', 'surface', 4.5);
    check('danger', 'danger-surface', 4.5);
    check('focus', 'danger-surface', 3);
    check('border', 'danger-surface', 3);
  },
);
