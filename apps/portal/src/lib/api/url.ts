import type { paths } from './schema';

type Scalar = string | number | boolean | null | undefined;

export function apiUrl(
  template: keyof paths,
  path?: Record<string, string>,
  query?: Record<string, Scalar>,
): string {
  // Contract paths already include /api/v1.
  const url = template.replace(/\{(\w+)\}/g, (_, name: string) => {
    const value = path?.[name];
    if (!value || value === '.' || value === '..') {
      throw new TypeError(`Invalid API path parameter: ${name}.`);
    }
    return encodeURIComponent(value);
  });
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query ?? {})) {
    if (value != null) params.set(key, String(value));
  }
  return params.size ? `${url}?${params}` : url;
}
