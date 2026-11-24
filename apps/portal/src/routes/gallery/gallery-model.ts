import { ApiProblemError, type OutputQuery } from '../../lib/api/client';
import type { components } from '../../lib/api/schema';

export type Output = components['schemas']['OutputResponse'];
export const pageSize = 100;
export const filterKeys = ['workflow', 'from', 'to', 'tag', 'status'] as const;
export const statuses = [
  { value: 'completed', label: 'Completed' },
  { value: 'failed', label: 'Failed' },
  { value: 'cancelled', label: 'Cancelled' },
] as const;

export function filterHash(query: URLSearchParams) {
  const filters = new URLSearchParams();
  for (const key of filterKeys) {
    const value = query.get(key)?.trim();
    if (value) filters.set(key, value);
  }
  return filters.toString();
}

function date(value: string | null, end: boolean) {
  if (!value) return undefined;
  const time = `${value}T${end ? '23:59:59.999999999' : '00:00:00'}Z`;
  if (
    !/^\d{4}-\d{2}-\d{2}$/.test(value) ||
    !Number.isFinite(Date.parse(time)) ||
    new Date(time).toISOString().slice(0, 10) !== value
  ) {
    throw new Error('Use a valid calendar date.');
  }
  return time;
}

export function outputFilters(key: string): NonNullable<OutputQuery> {
  const query = new URLSearchParams(key);
  const from = query.get('from');
  const to = query.get('to');
  if (from && to && from > to)
    throw new Error('The start date must not be after the end date.');
  const status = query.get('status');
  if (status && !statuses.some((item) => item.value === status))
    throw new Error('Choose a supported run status.');
  return {
    workflow: query.get('workflow') ?? undefined,
    tag: query.get('tag') ?? undefined,
    created_from: date(from, false),
    created_to: date(to, true),
    status: (status ?? undefined) as
      components['schemas']['RunStatus'] | undefined,
  };
}

// The index returns UTC RFC 3339 and an ID ending in the output row number.
// Preserve sub-millisecond ordering and break timestamp ties like SQLite.
const timeKey = (time: string) =>
  time.replace(
    /(?:\.(\d+))?(?:Z|\+00:00)$/,
    (_, fraction: string | undefined) => (fraction ?? '').padEnd(9, '0'),
  );
export function newestFirst(a: Output, b: Output) {
  const timeA = timeKey(a.created_at);
  const timeB = timeKey(b.created_at);
  if (timeA !== timeB) return timeA > timeB ? -1 : 1;
  const rowA = a.id.slice(a.id.lastIndexOf(':') + 1);
  const rowB = b.id.slice(b.id.lastIndexOf(':') + 1);
  return (
    rowB.length - rowA.length || (rowA === rowB ? 0 : rowA > rowB ? -1 : 1)
  );
}

export function galleryProblem(error: unknown, title: string) {
  return error instanceof ApiProblemError
    ? error.problem
    : {
        title,
        detail: error instanceof Error ? error.message : undefined,
      };
}
