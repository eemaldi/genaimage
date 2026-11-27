import { ApiProblemError, BindingFloat } from '../../../lib/api/client';
import type { Problem } from '../../../lib/ui/ProblemDetails.svelte';

export type Exact = number | bigint | BindingFloat;
export type Json = Record<string, unknown>;

export const maxGraphBytes = 2 * 1024 * 1024;
export const maxListed = 50;
export const reservedIds = ['refresh', 'import'];
const idPattern = /^[a-z0-9][a-z0-9_-]{0,63}$/;

export type Recognised = {
  node: string;
  class_type: string;
  bound_inputs: { input: string; parameter: string }[];
};
export type Unbound = {
  node: string;
  class_type: string;
  input: string;
  linked: boolean;
  reason: string;
};
export type Proposal = {
  graph_sha256: string;
  manifest: Json;
  recognised: Recognised[];
  unknown_nodes: { node: string; class_type: string }[];
  unbound_inputs: Unbound[];
  warnings: string[];
};
export type Imported = { graph: Json; proposal: Proposal; fileName: string };

export type ParameterDraft = {
  uid: number;
  original: string;
  key: string;
  exposed: boolean;
  node: string;
  input: string;
  kind: string;
  required: boolean;
  defaultText: string;
  description: string;
  minimum: string;
  maximum: string;
  step: string;
  widget: string;
  group: string;
  placeholder: string;
  advanced: boolean;
  options: string;
};
export type PresetDraft = {
  uid: number;
  name: string;
  values: { uid: number; parameter: number; text: string }[];
};
export type ManifestDraft = {
  id: string;
  title: string;
  description: string;
  version: string;
  parameters: ParameterDraft[];
  presets: PresetDraft[];
};
/** Server values that are never edited. Kept outside reactive state so exact numbers stay untouched. */
export type Origin = { base: Json; sources: Map<number, Json> };

const isRecord = (value: unknown): value is Json =>
  !!value && typeof value === 'object' && !Array.isArray(value);
const text = (value: unknown) => (typeof value === 'string' ? value : '');
const list = <T>(value: unknown): T[] => (Array.isArray(value) ? value : []);

export function exactText(value: unknown): string {
  if (value instanceof BindingFloat || typeof value === 'bigint')
    return value.toString();
  if (typeof value === 'number')
    return Object.is(value, -0) ? '-0.0' : String(value);
  return '';
}

const integerText = /^[+-]?\d+$/;
const decimalText = /^([+-]?)(\d*)(?:\.(\d*))?(?:[eE]([+-]?\d+))?$/;

/** Parses typed text as an exact JSON number: whole values stay integers, any other spelling stays a float. */
export function parseExactNumber(input: string): Exact | undefined {
  const source = input.trim();
  if (!source) return;
  if (integerText.test(source)) {
    const negative = source.startsWith('-');
    const digits = source.replace(/^[+-]/, '').replace(/^0+(?=\d)/, '');
    if (negative && /^0+$/.test(digits)) return new BindingFloat(-0, '-0');
    const value = BigInt(`${negative ? '-' : ''}${digits}`);
    const number = Number(value);
    return Number.isSafeInteger(number) ? number : value;
  }
  const match = decimalText.exec(source);
  if (!match) return;
  const [, sign, whole, fraction, exponent] = match;
  if (!whole && !fraction) return;
  const spelled = `${sign === '-' ? '-' : ''}${whole.replace(/^0+(?=\d)/, '') || '0'}${
    fraction !== undefined ? `.${fraction || '0'}` : ''
  }${exponent ? `e${exponent}` : ''}`;
  const value = Number(spelled);
  if (!Number.isFinite(value)) return;
  return new BindingFloat(value, spelled);
}

function numberOrUndefined(source: string): Exact | undefined {
  return source.trim() === '' ? undefined : parseExactNumber(source);
}

export function draftFromProposal(proposal: Proposal): {
  draft: ManifestDraft;
  origin: Origin;
} {
  const manifest = proposal.manifest;
  let uid = 0;
  const sources = new Map<number, Json>();
  const parameters = Object.entries(
    isRecord(manifest.parameters) ? manifest.parameters : {},
  ).flatMap(([key, value]): ParameterDraft[] => {
    if (!isRecord(value)) return [];
    const kind = text(value.kind);
    sources.set(uid, value);
    return [
      {
        uid: uid++,
        original: key,
        key,
        exposed: true,
        node: text(value.node),
        input: text(value.input),
        kind,
        required: value.required === true,
        defaultText: valueText(value.default),
        description: text(value.description),
        minimum: exactText(value.minimum),
        maximum: exactText(value.maximum),
        step: exactText(value.step),
        widget: text(value.widget),
        group: text(value.group),
        placeholder: text(value.placeholder),
        advanced: value.advanced === true,
        options: list<unknown>(value.options).map(optionText).join('\n'),
      },
    ];
  });
  const byOriginal = new Map(parameters.map((item) => [item.original, item]));
  let presetUid = 0;
  const presets = Object.entries(
    isRecord(manifest.presets) ? manifest.presets : {},
  ).map(([name, values]): PresetDraft => {
    return {
      uid: presetUid++,
      name,
      values: Object.entries(isRecord(values) ? values : {}).flatMap(
        ([parameter, value]) => {
          const target = byOriginal.get(parameter);
          return target
            ? [
                {
                  uid: presetUid++,
                  parameter: target.uid,
                  text: valueText(value),
                },
              ]
            : [];
        },
      ),
    };
  });
  return {
    draft: {
      id: text(manifest.id),
      title: text(manifest.title),
      description: text(manifest.description),
      version: text(manifest.version) || '1',
      parameters,
      presets,
    },
    origin: { base: manifest, sources },
  };
}

function optionText(value: unknown): string {
  return typeof value === 'string' ? value : valueText(value);
}
export function valueText(value: unknown): string {
  if (typeof value === 'string') return value;
  if (typeof value === 'boolean') return String(value);
  return exactText(value);
}

export type ValueResult = { value?: string | boolean | Exact; error?: string };
export function parseValue(kind: string, source: string): ValueResult {
  switch (kind) {
    case 'boolean':
      return source === 'true' || source === 'false'
        ? { value: source === 'true' }
        : { error: 'Choose Yes or No.' };
    case 'integer':
    case 'seed': {
      const value = parseExactNumber(source);
      return value !== undefined && integerText.test(source.trim())
        ? { value }
        : { error: 'Enter a whole number.' };
    }
    case 'number': {
      const value = parseExactNumber(source);
      return value !== undefined
        ? { value }
        : { error: 'Enter a number, for example 7 or 7.5.' };
    }
    case 'image_upload':
      return { error: 'Image inputs cannot be set in a preset.' };
    default:
      return { value: source };
  }
}

export const numericKinds = ['integer', 'number', 'seed'];
export function widgetChoices(kind: string): string[] {
  if (kind === 'boolean') return ['checkbox', 'select'];
  if (numericKinds.includes(kind)) return ['number', 'slider', 'select'];
  if (kind === 'string') return ['text', 'textarea', 'select'];
  return [];
}

export type DraftErrors = {
  id?: string;
  title?: string;
  version?: string;
  parameters: Map<number, Partial<Record<ParameterField, string>>>;
  presets: Map<number, { name?: string; values: Map<number, string> }>;
};
export type ParameterField =
  'key' | 'minimum' | 'maximum' | 'step' | 'options' | 'widget';

export function idError(id: string): string | undefined {
  if (!idPattern.test(id))
    return 'Use 1 to 64 characters: lower-case letters, digits, hyphens and underscores, starting with a letter or digit.';
  if (reservedIds.includes(id))
    return `"${id}" is reserved. Choose another identifier.`;
}

export function validateDraft(draft: ManifestDraft): DraftErrors {
  const errors: DraftErrors = {
    parameters: new Map(),
    presets: new Map(),
  };
  const id = idError(draft.id);
  if (id) errors.id = id;
  if (!draft.title.trim()) errors.title = 'Enter a title.';
  if (!draft.version.trim()) errors.version = 'Enter a version.';
  const keys = new Map<string, number>();
  for (const item of draft.parameters) {
    if (!item.exposed) continue;
    const found: Partial<Record<ParameterField, string>> = {};
    if (!item.key.trim()) found.key = 'Enter a parameter name.';
    else if (item.key !== item.key.trim())
      found.key = 'Remove the spaces around the name.';
    else if (keys.has(item.key)) found.key = 'This name is already used.';
    keys.set(item.key, item.uid);
    for (const field of ['minimum', 'maximum', 'step'] as const) {
      if (!numericKinds.includes(item.kind) && item[field].trim()) {
        found[field] = 'Only numeric parameters have this setting.';
      } else if (item[field].trim()) {
        const value = parseExactNumber(item[field]);
        if (value === undefined) found[field] = 'Enter a number.';
        else if (field === 'step' && approximate(value) <= 0)
          found.step = 'The step must be greater than zero.';
      }
    }
    if (item.options.trim()) {
      const bad = optionValues(item).find((entry) => entry.error);
      if (bad) found.options = `${bad.error} Check each option line.`;
    }
    if (
      found.key ||
      found.minimum ||
      found.maximum ||
      found.step ||
      found.options
    )
      errors.parameters.set(item.uid, found);
  }
  const exposed = new Map(
    draft.parameters.filter((item) => item.exposed).map((i) => [i.uid, i]),
  );
  const names = new Set<string>();
  for (const preset of draft.presets) {
    const found: { name?: string; values: Map<number, string> } = {
      values: new Map(),
    };
    if (!preset.name.trim()) found.name = 'Enter a preset name.';
    else if (preset.name !== preset.name.trim())
      found.name = 'Remove the spaces around the name.';
    else if (names.has(preset.name)) found.name = 'This name is already used.';
    names.add(preset.name);
    const used = new Set<number>();
    for (const entry of preset.values) {
      const target = exposed.get(entry.parameter);
      if (!target) found.values.set(entry.uid, 'Choose an exposed parameter.');
      else if (used.has(entry.parameter))
        found.values.set(entry.uid, 'This parameter is set twice.');
      else {
        const { error } = parseValue(target.kind, entry.text);
        if (error) found.values.set(entry.uid, error);
      }
      used.add(entry.parameter);
    }
    if (found.name || found.values.size) errors.presets.set(preset.uid, found);
  }
  return errors;
}

export function hasErrors(errors: DraftErrors): boolean {
  return !!(
    errors.id ||
    errors.title ||
    errors.version ||
    errors.parameters.size ||
    errors.presets.size
  );
}

function approximate(value: Exact): number {
  return value instanceof BindingFloat ? value.value : Number(value);
}

function optionValues(item: ParameterDraft): ValueResult[] {
  return item.options
    .split('\n')
    .filter((line) => line.trim() !== '')
    .map((line) => parseValue(item.kind, line));
}

function numberField(current: string, original: unknown): Exact | undefined {
  if (current === exactText(original)) return original as Exact | undefined;
  return numberOrUndefined(current);
}

/** Builds the manifest to confirm. Call only for a draft that passes validateDraft. */
export function buildManifest(draft: ManifestDraft, origin: Origin): Json {
  const parameters: Json = {};
  for (const item of draft.parameters) {
    if (!item.exposed) continue;
    const source = origin.sources.get(item.uid) ?? {};
    const next: Json = {
      node: item.node,
      input: item.input,
      kind: item.kind,
      required: item.required,
      description: item.description,
    };
    if (source.default !== undefined && source.default !== null)
      next.default = source.default;
    for (const field of ['minimum', 'maximum', 'step'] as const) {
      const value = numberField(item[field], source[field]);
      if (value !== undefined) next[field] = value;
    }
    if (item.widget) next.widget = item.widget;
    if (item.options.trim()) {
      next.options =
        item.options ===
        list<unknown>(source.options).map(optionText).join('\n')
          ? source.options
          : optionValues(item).map((entry) => entry.value);
    }
    if (source.allowed_values) next.allowed_values = source.allowed_values;
    if (item.group.trim()) next.group = item.group.trim();
    if (item.advanced || typeof source.advanced === 'boolean')
      next.advanced = item.advanced;
    if (item.placeholder) next.placeholder = item.placeholder;
    parameters[item.key] = next;
  }
  const presets: Json = {};
  const exposed = new Map(
    draft.parameters.filter((item) => item.exposed).map((i) => [i.uid, i]),
  );
  for (const preset of draft.presets) {
    const values: Json = {};
    for (const entry of preset.values) {
      const target = exposed.get(entry.parameter);
      if (target)
        values[target.key] = parseValue(target.kind, entry.text).value;
    }
    presets[preset.name] = values;
  }
  const manifest: Json = {
    ...origin.base,
    id: draft.id,
    title: draft.title.trim(),
    version: draft.version.trim(),
    parameters,
    presets,
  };
  if (draft.description.trim()) manifest.description = draft.description;
  else delete manifest.description;
  for (const [key, value] of Object.entries(manifest))
    if (value === null) delete manifest[key];
  return manifest;
}

export type Grouped = { class_type: string; count: number; nodes: string[] };
export function groupByClass(
  items: { node: string; class_type: string }[],
): Grouped[] {
  const groups = new Map<string, Grouped>();
  for (const { node, class_type } of items) {
    const group = groups.get(class_type) ?? { class_type, count: 0, nodes: [] };
    group.count += 1;
    group.nodes.push(node);
    groups.set(class_type, group);
  }
  return [...groups.values()].sort(
    (a, b) => b.count - a.count || a.class_type.localeCompare(b.class_type),
  );
}

export function parseProposal(value: unknown): Proposal {
  const body = isRecord(value) ? value : {};
  return {
    graph_sha256: text(body.graph_sha256),
    manifest: isRecord(body.manifest) ? body.manifest : {},
    recognised: list<Recognised>(body.recognised).map((item) => ({
      ...item,
      bound_inputs: list(item?.bound_inputs),
    })),
    unknown_nodes: list(body.unknown_nodes),
    unbound_inputs: list(body.unbound_inputs),
    warnings: list<string>(body.warnings).filter((w) => typeof w === 'string'),
  };
}

export type Confirmed = { id: string; revision: string; manifest_path: string };

export type ProblemWithExtras = Problem & Record<string, unknown>;
export function problemOf(error: unknown, title: string): ProblemWithExtras {
  if (error instanceof ApiProblemError) return error.problem;
  return {
    title,
    detail:
      error instanceof Error ? error.message : 'An unexpected error occurred.',
    recovery: 'Check that the local GenAImage server is running, then retry.',
  };
}

export const problemTypes = {
  uiFormat: 'urn:genaimage:problem:workflow-ui-format',
  importInvalid: 'workflow-import-invalid',
  graphMismatch: 'workflow-graph-mismatch',
  exists: 'workflow-exists',
  manifestInvalid: 'workflow-manifest-invalid',
  ambiguous: 'workflow-ambiguous',
};
export function isProblemType(problem: Problem | undefined, name: string) {
  return !!problem?.type && problem.type.endsWith(name);
}

export type StepId = 'file' | 'review' | 'presets' | 'preview' | 'save';
export const steps: { id: StepId; label: string }[] = [
  { id: 'file', label: 'Choose a file' },
  { id: 'review', label: 'Review parameters' },
  { id: 'presets', label: 'Presets' },
  { id: 'preview', label: 'Preview the form' },
  { id: 'save', label: 'Review and save' },
];

/** Maps a manifest-invalid error key to the step and field that owns it. */
export function locateError(
  key: string,
  draft: ManifestDraft,
): { step: StepId; label: string } {
  if (key === 'id') return { step: 'review', label: 'Workflow ID' };
  if (key === 'version') return { step: 'review', label: 'Version' };
  if (key.startsWith('parameters.')) {
    const rest = key.slice('parameters.'.length);
    const match = draft.parameters.find(
      (item) =>
        item.exposed && (rest === item.key || rest.startsWith(`${item.key}.`)),
    );
    return {
      step: 'review',
      label: match ? `Parameter ${match.key}` : 'Parameters',
    };
  }
  if (key.startsWith('presets.'))
    return { step: 'presets', label: `Preset ${key.slice(8)}` };
  if (key === 'workflow' || key === 'thumbnail' || key === 'schema_version')
    return { step: 'save', label: `Manifest ${key}` };
  return { step: 'save', label: key };
}

export function fileStem(name: string): string {
  return name
    .replace(/\.json$/i, '')
    .replace(/[-_.]+/g, ' ')
    .trim();
}

export function nextUid(draft: ManifestDraft): number {
  return (
    Math.max(
      -1,
      ...draft.parameters.map((item) => item.uid),
      ...draft.presets.flatMap((item) => [
        item.uid,
        ...item.values.map((value) => value.uid),
      ]),
    ) + 1
  );
}
