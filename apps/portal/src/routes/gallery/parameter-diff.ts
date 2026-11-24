import type { components } from '../../lib/api/schema';

type Run = components['schemas']['RunResponse'];
export type RecordedRow = {
  key: string;
  label: string;
  values: unknown[];
  different: boolean;
};

function entries(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

function imprecise(value: unknown): boolean {
  if (typeof value === 'number')
    return (
      !Number.isFinite(value) ||
      (Number.isInteger(value) && !Number.isSafeInteger(value))
    );
  if (Array.isArray(value)) return value.some(imprecise);
  return Object.values(entries(value) ?? {}).some(imprecise);
}

function equal(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (Array.isArray(a) && Array.isArray(b))
    return a.length === b.length && a.every((item, i) => equal(item, b[i]));
  const left = entries(a);
  const right = entries(b);
  return (
    !!left &&
    !!right &&
    Object.keys(left).length === Object.keys(right).length &&
    Object.keys(left).every(
      (key) => Object.hasOwn(right, key) && equal(left[key], right[key]),
    )
  );
}

/** Only compare data from persisted run records. Undefined is not JSON null. */
export function parameterDiff(
  runs: readonly (Run | undefined)[],
): RecordedRow[] {
  const rows: RecordedRow[] = [];
  const add = (key: string, label: string, select: (run: Run) => unknown) => {
    const values = runs.map((run) =>
      run === undefined ? undefined : select(run),
    );
    if (values.some((value) => value !== undefined))
      rows.push({
        key,
        label,
        values,
        different: values.some(
          (value) => imprecise(value) || !equal(value, values[0]),
        ),
      });
  };
  add('workflow', 'Workflow', (run) => run.workflow_id);
  add('version', 'Workflow version', (run) => run.workflow_version);
  add('checksum', 'Workflow checksum', (run) => run.workflow_sha256);
  const keys = new Set(
    runs.flatMap((run) => Object.keys(entries(run?.parameters) ?? {})),
  );
  for (const key of [...keys].sort((a, b) =>
    a === 'seed' ? -1 : b === 'seed' ? 1 : a.localeCompare(b, 'en-GB'),
  )) {
    add(`parameter:${key}`, key === 'seed' ? 'Seed' : key, (run) => {
      const parameters = entries(run.parameters);
      return parameters && Object.hasOwn(parameters, key)
        ? parameters[key]
        : undefined;
    });
  }
  if (
    runs.some(
      (run) => run && run.parameters !== undefined && !entries(run.parameters),
    )
  ) {
    add('parameters', 'Recorded parameters', (run) =>
      entries(run.parameters) ? undefined : run.parameters,
    );
  }
  return rows;
}

export function formatRecordedValue(value: unknown) {
  if (value === undefined) return 'Not recorded';
  if (imprecise(value)) return 'Cannot display exactly; see the run record';
  return typeof value === 'string' ? value : JSON.stringify(value);
}
