import { BindingFloat } from '../../lib/api/client';
import type { components } from '../../lib/api/schema';

export type Workflow = components['schemas']['WorkflowDetails'];
export type Parameter = components['schemas']['WorkflowParameter'];
export type Scalar = string | number | bigint | boolean | BindingFloat;
export type FieldState = {
  value?: Scalar;
  draft?: string;
  source?: 'default' | 'preset' | 'user';
  highlighted?: boolean;
  seedMode?: 'locked' | 'random';
};
export type Review = Record<string, Scalar>;

const minInteger = -(1n << 63n);
const maxInteger = (1n << 64n) - 1n;
const integerPattern = /^[+-]?\d+$/;
const numberPattern = /^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:e[+-]?\d+)?$/i;
const uploadId = /^sha256:[0-9a-f]{64}$/;

function scalar(value: unknown): Scalar | undefined {
  return typeof value === 'string' ||
    typeof value === 'boolean' ||
    typeof value === 'number' ||
    typeof value === 'bigint' ||
    value instanceof BindingFloat
    ? value
    : undefined;
}

export function sameValue(left: unknown, right: unknown): boolean {
  return (
    left === right ||
    (left instanceof BindingFloat &&
      right instanceof BindingFloat &&
      left.value === right.value)
  );
}

export function initialField(parameter: Parameter): FieldState {
  const value = scalar(parameter.default);
  return {
    value,
    source: value === undefined ? undefined : 'default',
    seedMode: parameter.kind === 'seed' ? 'locked' : undefined,
  };
}

export function presetField(
  parameter: Parameter,
  previous: FieldState,
  raw: unknown,
): FieldState {
  const value = scalar(raw);
  return {
    value,
    source: 'preset',
    highlighted:
      !sameValue(value, previous.value) ||
      previous.draft !== undefined ||
      previous.seedMode === 'random',
    seedMode: parameter.kind === 'seed' ? 'locked' : undefined,
  };
}

export function numericDraft(
  parameter: Parameter,
  text: string,
): Pick<FieldState, 'value' | 'draft'> {
  if (text === '') return {};
  if (parameter.kind === 'number' && numberPattern.test(text)) {
    if (integerPattern.test(text)) {
      const integer = BigInt(text);
      if (integer >= minInteger && integer <= maxInteger)
        return { value: compactInteger(integer), draft: text };
    }
    const parsed = Number(text);
    return {
      value: Number.isInteger(parsed) ? new BindingFloat(parsed) : parsed,
      draft: text,
    };
  }
  if (parameter.kind !== 'number' && integerPattern.test(text)) {
    const integer = BigInt(text);
    return { value: compactInteger(integer), draft: text };
  }
  return { draft: text };
}

function compactInteger(integer: bigint): number | bigint {
  const value = Number(integer);
  return Number.isSafeInteger(value) ? value : integer;
}

export function presenceLabel(
  declared: boolean | string | null | undefined,
): string {
  if (declared === null) return ' (default)';
  if (declared === undefined) return ' (declaration unavailable)';
  return ' (declared)';
}

export function requirementLabel(parameter: Parameter): string {
  return `${parameter.required ? 'Required' : 'Optional'}${presenceLabel(parameter.declared_required)}`;
}

export function bindingError(
  parameter: Parameter,
  field: FieldState,
): string | undefined {
  const value = field.value;
  if (value === undefined && field.draft === undefined) {
    return parameter.required ? 'Enter a value for this parameter.' : undefined;
  }
  switch (parameter.kind) {
    case 'seed':
    case 'integer':
      if (
        typeof value !== 'bigint' &&
        !(typeof value === 'number' && Number.isSafeInteger(value))
      )
        return 'Enter a whole number.';
      if (BigInt(value) < minInteger || BigInt(value) > maxInteger)
        return `Enter a whole number from ${minInteger} to ${maxInteger}.`;
      break;
    case 'number':
      if (
        (typeof value !== 'number' || !Number.isFinite(value)) &&
        typeof value !== 'bigint' &&
        !(value instanceof BindingFloat)
      )
        return 'Enter a finite number.';
      break;
    case 'boolean':
      if (typeof value !== 'boolean') return 'Choose Yes or No.';
      break;
    case 'image_upload':
      if (typeof value !== 'string' || !uploadId.test(value))
        return 'Choose an image uploaded to GenAImage.';
      break;
    case 'string':
      if (typeof value !== 'string') return 'Enter text.';
      break;
    default:
      return 'This parameter type is not supported.';
  }
  if (
    parameter.allowed_values &&
    !parameter.allowed_values.some((choice) => sameValue(choice, value))
  ) {
    return `Choose an allowed value: ${parameter.allowed_values.map(String).join(', ')}.`;
  }
  // The core compares numeric values to f64 bounds. Step and options are UI hints,
  // and the binding contract has no string-length constraint.
  if (
    typeof value === 'number' ||
    typeof value === 'bigint' ||
    value instanceof BindingFloat
  ) {
    const numeric = value instanceof BindingFloat ? value.value : Number(value);
    if (parameter.minimum != null && numeric < parameter.minimum)
      return `Enter a value of at least ${parameter.minimum}.`;
    if (parameter.maximum != null && numeric > parameter.maximum)
      return `Enter a value of at most ${parameter.maximum}.`;
  }
}

function randomBelow(count: bigint): bigint {
  if (count === 1n) return 0n;
  const bits = (count - 1n).toString(2).length;
  const bytes = new Uint8Array(Math.ceil(bits / 8));
  let value: bigint;
  do {
    crypto.getRandomValues(bytes);
    value = BigInt(bytes[0] & (255 >> (bytes.length * 8 - bits)));
    for (const byte of bytes.subarray(1)) value = (value << 8n) | BigInt(byte);
  } while (value >= count);
  return value;
}

// Find the first integer satisfying a monotone predicate on the supported range.
function firstInteger(test: (value: number) => boolean): bigint {
  let low = minInteger;
  let high = maxInteger + 1n;
  while (low < high) {
    const middle = low + (high - low) / 2n;
    if (test(Number(middle))) high = middle;
    else low = middle + 1n;
  }
  return low;
}

export function randomSeed(parameter: Parameter): number | bigint | undefined {
  if (parameter.allowed_values) {
    const options = [...new Set(parameter.allowed_values.map(scalar))].filter(
      (value): value is number | bigint =>
        (typeof value === 'bigint' || typeof value === 'number') &&
        !bindingError(parameter, { value }),
    );
    return options.length
      ? options[Number(randomBelow(BigInt(options.length)))]
      : undefined;
  }
  // Rust compares as_f64() against the bounds, including integers that round to an endpoint.
  const { minimum, maximum } = parameter;
  const first =
    minimum == null
      ? minInteger
      : firstInteger((rounded) => rounded >= minimum);
  const pastLast =
    maximum == null
      ? maxInteger + 1n
      : firstInteger((rounded) => rounded > maximum);
  if (first < pastLast)
    return compactInteger(first + randomBelow(pastLast - first));
}

export function workflowHref(id: string): string {
  return id
    ? `#/generate?${new URLSearchParams({ workflow: id })}`
    : '#/generate';
}

export function parameterGroups(workflow: Workflow, advanced: boolean) {
  const groups = new Map<string, [string, Parameter][]>();
  for (const [name, parameter] of Object.entries(workflow.parameters)) {
    if ((parameter.advanced ?? false) !== advanced) continue;
    const group = parameter.group ?? '';
    const members = groups.get(group) ?? [];
    members.push([name, parameter]);
    groups.set(group, members);
  }
  return [...groups];
}
