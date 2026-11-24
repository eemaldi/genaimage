import { BindingFloat } from '../../lib/api/client';
import {
  bindingError,
  initialField,
  presetField,
  sameValue,
  type FieldState,
  type Scalar,
  type Workflow,
} from './model';

const format = '2';
const maxFragment = 32_768;
const maxEntries = 256;
type Encoded =
  | string
  | boolean
  | null
  | [kind: 'int' | 'float' | 'draft' | 'invalid', text: string];
type Entry = [
  name: string,
  source: 'd' | 'p' | 'u' | 'n',
  value: Encoded,
  mode?: 'r' | 'l',
  chosen?: Encoded,
];
// These descriptions are provenance only, never editor values or bindings.
export type SharedInherited = [
  name: string,
  node: string,
  input: string,
  description: string,
];

type FormState = {
  workflow: string;
  version?: string;
  revision?: string;
  preset?: string;
  modified?: boolean;
  fields: Iterable<[string, FieldState]>;
  inherited?: readonly SharedInherited[];
  unverifiedSource?: boolean;
  suggested?: boolean;
  chosenSeeds?: Readonly<Record<string, Scalar>>;
};

function encodeValue(value: unknown): Encoded {
  if (value === undefined) return null;
  if (Object.is(value, -0)) return ['float', '-0.0'];
  if (
    typeof value === 'bigint' ||
    (typeof value === 'number' && Number.isSafeInteger(value))
  )
    return ['int', String(value)];
  if (value instanceof BindingFloat) return ['float', String(value)];
  if (
    typeof value === 'number' &&
    Number.isFinite(value) &&
    !Number.isInteger(value)
  )
    return ['float', String(value)];
  if (typeof value === 'string' || typeof value === 'boolean') return value;
  return ['invalid', String(value)];
}

function makeHash(query: URLSearchParams, entries: Entry[]): string {
  if (entries.length > maxEntries)
    throw new Error('There are too many parameters for a share link.');
  query.set('p', JSON.stringify(entries));
  const hash = `#/generate?${query}`;
  if (hash.length > maxFragment) throw new Error('The share link is too long.');
  return hash;
}

export function encodeFormState(state: FormState): string {
  const query = new URLSearchParams({ workflow: state.workflow, v: format });
  if (state.version !== undefined) query.set('wv', state.version);
  if (state.revision !== undefined) query.set('wr', state.revision);
  if (state.unverifiedSource) query.set('unverified_source', '1');
  if (state.suggested) query.set('assistant', 'suggest');
  if (state.preset) query.set('preset', state.preset);
  if (state.modified) query.set('modified', '1');
  if (state.inherited?.length) query.set('i', JSON.stringify(state.inherited));
  const entries: Entry[] = [];
  for (const [name, field] of state.fields) {
    const source =
      field.source === 'user'
        ? 'u'
        : field.source === 'preset'
          ? 'p'
          : field.source === 'default'
            ? 'd'
            : 'n';
    const value =
      field.value === undefined && field.draft !== undefined
        ? (['draft', field.draft] as Encoded)
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
  return makeHash(query, entries);
}

/** Use api.getRecordedRun(id, { exactBindings: true }) to preserve exact numbers. */
export function buildGenerateUrl(run: {
  workflow_id: string;
  workflow_version: string;
  parameters: unknown;
  workflow_revision?: string | null;
}): string {
  const query = new URLSearchParams({
    workflow: run.workflow_id,
    v: format,
    wv: run.workflow_version,
  });
  if (run.workflow_revision) query.set('wr', run.workflow_revision);
  else query.set('unverified_source', '1');
  const entries =
    run.parameters &&
    typeof run.parameters === 'object' &&
    !Array.isArray(run.parameters)
      ? Object.entries(run.parameters).map(([name, value]): Entry => [
          name,
          'u',
          encodeValue(value),
        ])
      : [];
  return makeHash(query, entries);
}

function decodeValue(value: unknown): Pick<FieldState, 'draft' | 'value'> {
  if (value === null) return {};
  if (typeof value === 'string' || typeof value === 'boolean') return { value };
  if (
    Array.isArray(value) &&
    value.length === 2 &&
    typeof value[1] === 'string'
  ) {
    const [tag, text] = value;
    if (tag === 'int' && /^(?:0|-?[1-9]\d{0,19})$/.test(text)) {
      const exact = BigInt(text);
      if (exact >= -(1n << 63n) && exact < 1n << 64n)
        return {
          value: Number.isSafeInteger(Number(exact)) ? Number(exact) : exact,
        };
    }
    if (
      tag === 'float' &&
      /^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:e[+-]?\d+)?$/.test(text) &&
      /[.e]/.test(text)
    ) {
      const number = Number(text);
      if (Number.isFinite(number)) {
        const exact = Number.isInteger(number)
          ? new BindingFloat(number)
          : number;
        if (String(exact) === text) return { value: exact };
      }
    }
    if (tag === 'draft') return { draft: text };
  }
  throw new Error('The encoded value is not supported.');
}

type Difference = { name: string; message: string };

export type PendingSuggestion = {
  suggestedRevision: string;
  loadedRevision: string;
  preset: string;
  modified: boolean;
  fields: Map<string, FieldState>;
  issues: string[];
};

export function restoreFormState(workflow: Workflow, query: URLSearchParams) {
  const fields = new Map(
    Object.entries(workflow.parameters).map(([name, parameter]) => [
      name,
      initialField(parameter),
    ]),
  );
  const warnings: { name?: string; message: string }[] = [];
  const differences: Difference[] = [];
  const chosenSeeds = new Map<string, Scalar>();
  const state = {
    fields,
    warnings,
    differences,
    chosenSeeds,
    inherited: [] as SharedInherited[],
    preset: '',
    modified: false,
    versionNotice: undefined as string | undefined,
    revisionNotice: undefined as string | undefined,
    suggested: query.get('assistant') === 'suggest',
    suggestionNotice: undefined as string | undefined,
    pendingSuggestion: undefined as PendingSuggestion | undefined,
    unverifiedSource: query.get('unverified_source') === '1',
  };
  if (
    ![
      'v',
      'p',
      'wv',
      'wr',
      'preset',
      'modified',
      'i',
      'unverified_source',
      'assistant',
    ].some((key) => query.has(key))
  )
    return state;
  if (query.toString().length + '#/generate?'.length > maxFragment) {
    warnings.push({ message: 'This share link is too long.' });
    return state;
  }
  if (query.get('v') !== format) {
    warnings.push({
      message: 'This link uses an unsupported form state version.',
    });
    return state;
  }
  try {
    const inherited: unknown = JSON.parse(query.get('i') ?? '[]');
    if (Array.isArray(inherited) && inherited.length <= maxEntries) {
      state.inherited = inherited.filter(
        (entry): entry is SharedInherited =>
          Array.isArray(entry) &&
          entry.length === 4 &&
          entry.every((part) => typeof part === 'string'),
      );
    }
  } catch {
    // Informational metadata cannot invalidate the shared bindings.
  }
  const oldVersion = query.get('wv');
  if (oldVersion !== null && oldVersion !== workflow.version)
    state.versionNotice = `This link used workflow version ${oldVersion}; the current version is ${workflow.version}. Check the parameters before reviewing.`;
  const oldRevision = query.get('wr');
  if (oldRevision !== null && oldRevision !== workflow.revision)
    state.revisionNotice =
      'The workflow revision differs from the one in this link. Check the current workflow before reviewing.';
  const suggestedPreset = query.get('preset') ?? '';
  const suggestionMismatch =
    state.suggested && oldRevision !== workflow.revision;
  if (suggestionMismatch) {
    state.pendingSuggestion = {
      suggestedRevision: oldRevision ?? '(missing)',
      loadedRevision: workflow.revision,
      preset: suggestedPreset,
      modified: query.get('modified') === '1',
      fields: new Map(),
      issues: [],
    };
  } else {
    state.preset = suggestedPreset;
    state.modified = query.get('modified') === '1';
  }
  const preset = state.preset;
  if (state.suggested) {
    state.suggestionNotice = suggestionMismatch
      ? `Assistant-suggested values from revision ${oldRevision ?? '(missing)'} need explicit approval before they can be applied to loaded revision ${workflow.revision}.`
      : 'These assistant-suggested values are prefilled for editing and have not been submitted. Check every value before review.';
  }
  if (!suggestionMismatch && preset && !Object.hasOwn(workflow.presets, preset))
    warnings.push({
      message: `Preset “${preset}” is not available in the current workflow.`,
    });
  let entries: unknown;
  try {
    entries = JSON.parse(query.get('p') ?? '');
  } catch {
    /* reported below */
  }
  if (!Array.isArray(entries)) {
    warnings.push({ message: 'This link contains unreadable parameter data.' });
    return state;
  }
  if (entries.length > maxEntries) {
    warnings.push({ message: 'This link contains too many parameters.' });
    return state;
  }
  const seen = new Set<string>();
  for (const entry of entries) {
    if (!Array.isArray(entry) || typeof entry[0] !== 'string') {
      warnings.push({
        message: 'This link contains an unreadable parameter entry.',
      });
      continue;
    }
    const [name, source, encoded, mode, chosen] = entry as [
      string,
      unknown,
      unknown,
      unknown?,
      unknown?,
    ];
    if (seen.has(name)) {
      warnings.push({ message: `The link repeats parameter “${name}”.` });
      continue;
    }
    seen.add(name);
    if (!Object.hasOwn(workflow.parameters, name)) {
      const message = `Unknown parameter “${name}”: ${JSON.stringify(encoded) ?? '(missing)'}.`;
      if (state.pendingSuggestion) state.pendingSuggestion.issues.push(message);
      else warnings.push({ message });
      continue;
    }
    const parameter = workflow.parameters[name];
    const currentDefault = fields.get(name)?.value;
    const seedMode =
      parameter.kind === 'seed'
        ? mode === 'r'
          ? 'random'
          : 'locked'
        : undefined;
    const field: FieldState = {
      seedMode,
      source:
        source === 'p'
          ? 'preset'
          : source === 'd'
            ? 'default'
            : source === 'u'
              ? 'user'
              : undefined,
    };
    let error: string | undefined;
    try {
      if (
        entry.length < 3 ||
        entry.length > 5 ||
        (source !== 'd' &&
          source !== 'p' &&
          source !== 'u' &&
          source !== 'n') ||
        (source === 'n' && encoded !== null) ||
        (mode !== undefined &&
          (parameter.kind !== 'seed' || (mode !== 'r' && mode !== 'l'))) ||
        (chosen !== undefined && mode !== 'r')
      )
        throw new Error('Unrecognised entry');
      Object.assign(field, decodeValue(encoded));
      if (chosen !== undefined) {
        const decoded = decodeValue(chosen);
        error =
          decoded.value === undefined
            ? 'The shared seed is missing.'
            : bindingError(parameter, decoded);
        if (!error && decoded.value !== undefined)
          chosenSeeds.set(name, decoded.value);
      }
      if (encoded === null && currentDefault !== undefined)
        error ??= `The link explicitly left this unset, but the current manifest defaults to ${displayScalar(currentDefault)}. Set a value or accept the current default.`;
      else if (
        !state.pendingSuggestion &&
        source === 'd' &&
        !sameValue(field.value, currentDefault)
      )
        differences.push({
          name,
          message: `Shared default: ${displayScalar(field.value)}. Current manifest default: ${displayScalar(currentDefault)}. The shared value was kept.`,
        });
      else if (
        !state.pendingSuggestion &&
        currentDefault !== undefined &&
        field.value !== undefined &&
        !sameValue(field.value, currentDefault)
      )
        differences.push({
          name,
          message: `Shared value: ${displayScalar(field.value)}. Current manifest default: ${displayScalar(currentDefault)}. The shared value was kept.`,
        });
      if (
        !state.pendingSuggestion &&
        (field.value !== undefined ||
          field.draft !== undefined ||
          seedMode !== 'random')
      )
        error ??= bindingError(parameter, field);
    } catch {
      error = 'The encoded value is not supported.';
    }
    if (state.pendingSuggestion)
      state.pendingSuggestion.fields.set(name, field);
    else fields.set(name, field);
    if (error && state.pendingSuggestion)
      state.pendingSuggestion.issues.push(
        `Parameter “${name}”: ${error} Shared value: ${JSON.stringify(encoded) ?? '(missing)'}.`,
      );
    else if (error)
      warnings.push({
        name,
        message: `Parameter “${name}”: ${error} Shared value: ${JSON.stringify(encoded) ?? '(missing)'}.`,
      });
  }
  for (const [name, parameter] of Object.entries(workflow.parameters)) {
    if (seen.has(name)) continue;
    if (state.suggested) {
      if (state.pendingSuggestion) continue;
      if (
        preset &&
        Object.hasOwn(workflow.presets, preset) &&
        Object.hasOwn(workflow.presets[preset], name)
      ) {
        fields.set(
          name,
          presetField(
            parameter,
            initialField(parameter),
            workflow.presets[preset][name],
          ),
        );
      } else fields.set(name, initialField(parameter));
      continue;
    }
    const current = initialField(parameter);
    fields.set(name, { seedMode: current.seedMode });
    warnings.push({
      name,
      message: `Parameter “${name}” is missing from the link.${current.value === undefined ? '' : ` The current manifest defaults to ${displayScalar(current.value)}.`} Set a value or accept the current form setting.`,
    });
  }
  return state;
}

export function displayScalar(value: Scalar | undefined) {
  return value === undefined
    ? '(not set)'
    : typeof value === 'string'
      ? JSON.stringify(value)
      : String(value);
}

export type WorkflowSuggestion =
  import('../../lib/api/schema').components['schemas']['WorkflowSuggestion'];

export function suggestionFields(suggestion: WorkflowSuggestion) {
  const fields = new Map<string, FieldState>();
  for (const [name, value] of Object.entries(suggestion.parameters)) {
    if (
      typeof value !== 'string' &&
      typeof value !== 'boolean' &&
      typeof value !== 'number' &&
      typeof value !== 'bigint' &&
      !(value instanceof BindingFloat)
    )
      throw new Error(
        `The suggestion for parameter “${name}” is not a scalar value.`,
      );
    fields.set(name, { value, source: 'user' });
  }
  return fields;
}

/** Prefill Generate without submitting; the server omits image inputs from suggestions. */
export function buildSuggestionGenerateUrl(suggestion: WorkflowSuggestion) {
  return encodeFormState({
    workflow: suggestion.workflow_id,
    revision: suggestion.workflow_revision,
    preset: suggestion.preset ?? undefined,
    modified:
      !!suggestion.preset && Object.keys(suggestion.parameters).length > 0,
    suggested: true,
    fields: suggestionFields(suggestion),
  });
}
