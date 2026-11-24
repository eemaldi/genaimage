import { BindingFloat } from '../../lib/api/client';
import {
  bindingError,
  initialField,
  numericDraft,
  type FieldState,
  type Scalar,
  type Workflow,
} from './model';

type Encoded =
  | string
  | number
  | boolean
  | null
  | [kind: 'int' | 'float' | 'draft' | 'invalid', text: string];
type Entry = [
  name: string,
  source: 'd' | 'p' | 'u',
  value: Encoded,
  mode?: 'r' | 'l',
  chosen?: Encoded,
];
type FormState = {
  workflow: string;
  version?: string;
  preset?: string;
  modified?: boolean;
  fields: Iterable<[string, FieldState]>;
  chosenSeeds?: Readonly<Record<string, Scalar>>;
};

function encodeValue(value: unknown): Encoded {
  if (value === undefined) return null;
  if (typeof value === 'bigint') return ['int', String(value)];
  if (value instanceof BindingFloat) return ['float', String(value)];
  if (
    typeof value === 'string' ||
    typeof value === 'boolean' ||
    (typeof value === 'number' &&
      Number.isFinite(value) &&
      (!Number.isInteger(value) || Number.isSafeInteger(value)))
  )
    return value;
  return ['invalid', String(value)];
}

export function encodeFormState(state: FormState): string {
  const query = new URLSearchParams({ workflow: state.workflow, v: '1' });
  if (state.version !== undefined) query.set('wv', state.version);
  if (state.preset) query.set('preset', state.preset);
  if (state.modified) query.set('modified', '1');
  const entries: Entry[] = [];
  for (const [name, field] of state.fields) {
    if (
      (!field.source || field.source === 'default') &&
      field.seedMode !== 'random'
    )
      continue;
    const source =
      field.source === 'user' ? 'u' : field.source === 'preset' ? 'p' : 'd';
    const value: Encoded =
      field.draft !== undefined
        ? ['draft', field.draft]
        : encodeValue(field.value);
    const entry: Entry = [name, source, value];
    if (field.seedMode) entry.push(field.seedMode === 'random' ? 'r' : 'l');
    if (
      field.seedMode === 'random' &&
      state.chosenSeeds &&
      Object.hasOwn(state.chosenSeeds, name)
    )
      entry.push(encodeValue(state.chosenSeeds[name]));
    entries.push(entry);
  }
  if (entries.length) query.set('p', JSON.stringify(entries));
  return `#/generate?${query}`;
}

/** Pass a recorded run with exact integers from api.getRecordedRun(id, { exactBindings: true }). */
export function buildGenerateUrl(run: {
  workflow_id: string;
  workflow_version: string;
  parameters: unknown;
}): string {
  const query = new URLSearchParams({
    workflow: run.workflow_id,
    v: '1',
    wv: run.workflow_version,
  });
  if (
    run.parameters &&
    typeof run.parameters === 'object' &&
    !Array.isArray(run.parameters)
  ) {
    const entries = Object.entries(run.parameters).map(
      ([name, value]): Entry => [name, 'u', encodeValue(value)],
    );
    if (entries.length) query.set('p', JSON.stringify(entries));
  }
  return `#/generate?${query}`;
}

function decodeValue(value: unknown): Pick<FieldState, 'draft' | 'value'> {
  if (value === null) return {};
  if (
    typeof value === 'string' ||
    typeof value === 'boolean' ||
    (typeof value === 'number' &&
      Number.isFinite(value) &&
      (!Number.isInteger(value) || Number.isSafeInteger(value)))
  )
    return { value };
  if (
    Array.isArray(value) &&
    value.length === 2 &&
    typeof value[1] === 'string'
  ) {
    const [tag, text] = value;
    if (tag === 'int' && /^-?\d{1,20}$/.test(text)) {
      const exact = BigInt(text);
      return {
        value: Number.isSafeInteger(Number(exact)) ? Number(exact) : exact,
      };
    }
    if (
      tag === 'float' &&
      /^-?\d+(?:\.\d+)?(?:e[+-]?\d+)?$/i.test(text) &&
      Number.isInteger(Number(text))
    )
      return { value: new BindingFloat(Number(text)) };
    if (tag === 'draft') return { draft: text };
  }
  throw new Error('Unrecognised value');
}

export function restoreFormState(workflow: Workflow, query: URLSearchParams) {
  const fields = new Map(
    Object.entries(workflow.parameters).map(([name, parameter]) => [
      name,
      initialField(parameter),
    ]),
  );
  const warnings: { name?: string; message: string }[] = [];
  const chosenSeeds = new Map<string, Scalar>();
  const oldVersion = query.get('wv');
  const versionNotice =
    oldVersion !== null && oldVersion !== workflow.version
      ? `This link used workflow version ${oldVersion}; the current version is ${workflow.version}. Check the parameters before reviewing.`
      : undefined;
  let preset = query.get('preset') ?? '';
  const modified = query.get('modified') === '1';
  if (
    !query.has('v') &&
    !query.has('p') &&
    !query.has('preset') &&
    !query.has('wv')
  )
    return { fields, warnings, chosenSeeds, preset, modified, versionNotice };
  if (query.get('v') !== '1') {
    warnings.push({
      message: 'This link uses an unsupported form state version.',
    });
    preset = '';
    return { fields, warnings, chosenSeeds, preset, modified, versionNotice };
  }
  if (preset && !Object.hasOwn(workflow.presets, preset)) {
    warnings.push({
      message: `Preset “${preset}” is not available in the current workflow.`,
    });
  }
  if (query.has('p')) {
    let entries: unknown;
    try {
      entries = JSON.parse(query.get('p') ?? '');
    } catch {
      /* reported below */
    }
    if (!Array.isArray(entries))
      warnings.push({
        message: 'This link contains unreadable parameter data.',
      });
    else {
      const seen = new Set<string>();
      for (const entry of entries) {
        if (!Array.isArray(entry) || typeof entry[0] !== 'string') {
          warnings.push({
            message: 'This link contains an unreadable parameter entry.',
          });
          continue;
        }
        const [name, source, encoded, mode, chosen]: [
          string,
          unknown,
          unknown,
          unknown?,
          unknown?,
        ] = entry as [string, unknown, unknown, unknown?, unknown?];
        if (seen.has(name)) {
          warnings.push({ message: `The link repeats parameter “${name}”.` });
          continue;
        }
        seen.add(name);
        if (!Object.hasOwn(workflow.parameters, name)) {
          warnings.push({
            message: `Unknown parameter “${name}”: ${JSON.stringify(encoded) ?? '(missing)'}.`,
          });
          continue;
        }
        const parameter = workflow.parameters[name];
        const seedMode =
          parameter.kind === 'seed'
            ? mode === 'r'
              ? 'random'
              : 'locked'
            : undefined;
        const field: FieldState = {
          seedMode,
          source:
            source === 'p' ? 'preset' : source === 'd' ? 'default' : 'user',
        };
        let error: string | undefined;
        try {
          if (
            entry.length < 3 ||
            entry.length > 5 ||
            !['d', 'p', 'u'].includes(String(source)) ||
            (mode !== undefined &&
              (parameter.kind !== 'seed' || (mode !== 'r' && mode !== 'l'))) ||
            (chosen !== undefined && mode !== 'r')
          )
            throw new Error('Unrecognised entry');
          Object.assign(field, decodeValue(encoded));
          if (
            field.draft !== undefined &&
            ['integer', 'number', 'seed'].includes(parameter.kind)
          )
            Object.assign(field, numericDraft(parameter, field.draft));
          if (chosen !== undefined) {
            const decoded = decodeValue(chosen);
            error =
              decoded.value === undefined
                ? 'The shared seed is missing.'
                : bindingError(parameter, decoded);
            if (!error && decoded.value !== undefined)
              chosenSeeds.set(name, decoded.value);
          }
          if (
            field.value !== undefined ||
            field.draft !== undefined ||
            seedMode !== 'random'
          )
            error ??= bindingError(parameter, field);
          if (
            seedMode !== 'random' &&
            field.value === undefined &&
            initialField(parameter).source
          )
            error ??= 'An omitted value would fall back to a manifest default.';
        } catch {
          error = 'The encoded value is not supported.';
        }
        fields.set(name, field);
        if (error)
          warnings.push({
            name,
            message: `Parameter “${name}”: ${error} Shared value: ${JSON.stringify(encoded) ?? '(missing)'}.`,
          });
      }
    }
  }
  return { fields, warnings, chosenSeeds, preset, modified, versionNotice };
}
