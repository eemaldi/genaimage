import type { TemplateRenderRequest } from '../api/client';
import type { components } from '../api/schema';

export type Template = components['schemas']['Template'];
export type TemplateVariable = components['schemas']['TemplateVariable'];
export type TemplateDiagnostic = components['schemas']['TemplateDiagnostic'];
export type InsertMode = 'replace' | 'append';
export type Entry = { kind: 'absent' } | { kind: 'text'; text: string };
export type Entries = Readonly<Record<string, Entry>>;
export type Listing = {
  templates: Template[];
  diagnostics: TemplateDiagnostic[];
};

export const absent: Entry = { kind: 'absent' };
const sha256 = /^[0-9a-f]{64}$/;
const variablePrefix = 'variables.';
const malformed = 'The template list was not in the expected format.';

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function nullableText(value: unknown): string | null | undefined {
  if (value === null || value === undefined) return null;
  return typeof value === 'string' ? value : undefined;
}

function parseVariable(raw: unknown): TemplateVariable {
  if (!isRecord(raw)) throw new Error(malformed);
  const description = nullableText(raw.description);
  const fallback = nullableText(raw.default);
  if (
    typeof raw.name !== 'string' ||
    typeof raw.required !== 'boolean' ||
    description === undefined ||
    fallback === undefined
  )
    throw new Error(malformed);
  return {
    name: raw.name,
    required: raw.required,
    description,
    default: fallback,
  };
}

function parseTemplate(raw: unknown): Template {
  if (!isRecord(raw) || !Array.isArray(raw.variables))
    throw new Error(malformed);
  const description = nullableText(raw.description);
  const packId = nullableText(raw.pack_id);
  if (
    typeof raw.ref !== 'string' ||
    typeof raw.id !== 'string' ||
    typeof raw.title !== 'string' ||
    typeof raw.text !== 'string' ||
    typeof raw.sha256 !== 'string' ||
    !sha256.test(raw.sha256) ||
    (raw.scope !== 'user' && raw.scope !== 'pack') ||
    description === undefined ||
    packId === undefined
  )
    throw new Error(malformed);
  return {
    ref: raw.ref,
    id: raw.id,
    title: raw.title,
    text: raw.text,
    sha256: raw.sha256,
    scope: raw.scope,
    pack_id: packId,
    description,
    variables: raw.variables.map(parseVariable),
  };
}

export function parseListing(raw: unknown): Listing {
  if (
    !isRecord(raw) ||
    !Array.isArray(raw.templates) ||
    !Array.isArray(raw.diagnostics)
  )
    throw new Error(malformed);
  return {
    templates: raw.templates.map(parseTemplate),
    diagnostics: raw.diagnostics.map((item): TemplateDiagnostic => {
      const ref = nullableText(isRecord(item) ? item.ref : undefined);
      if (
        !isRecord(item) ||
        typeof item.path !== 'string' ||
        typeof item.error !== 'string' ||
        ref === undefined
      )
        throw new Error(malformed);
      return { ref, path: item.path, error: item.error };
    }),
  };
}

export function filterTemplates(
  templates: readonly Template[],
  query: string,
): Template[] {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return [...templates];
  return templates.filter((template) =>
    [
      template.title,
      template.id,
      template.ref,
      template.description ?? '',
    ].some((field) => field.toLocaleLowerCase().includes(needle)),
  );
}

export function isRequired(variable: TemplateVariable): boolean {
  return variable.required || variable.default == null;
}

export function missingVariables(
  template: Template,
  entries: Entries,
): string[] {
  return template.variables
    .filter(
      (variable) =>
        isRequired(variable) && entries[variable.name]?.kind !== 'text',
    )
    .map((variable) => variable.name);
}

/** An absent variable is left out so the server applies its default; a touched one is always sent, even as "". */
export function requestVariables(
  template: Template,
  entries: Entries,
): Record<string, string> {
  const variables: Record<string, string> = {};
  for (const { name } of template.variables) {
    const entry = entries[name];
    if (entry?.kind === 'text') variables[name] = entry.text;
  }
  return variables;
}

export function pruneEntries(template: Template, entries: Entries): Entries {
  const declared = new Set(template.variables.map(({ name }) => name));
  return Object.fromEntries(
    Object.entries(entries).filter(([name]) => declared.has(name)),
  );
}

export function entryStatus(variable: TemplateVariable, entry: Entry): string {
  if (entry.kind === 'absent') {
    if (isRequired(variable))
      return 'Required: enter a value or use an empty value.';
    const fallback = variable.default
      ? `“${variable.default}”`
      : '(empty text)';
    return `Not set: the default ${fallback} is used.`;
  }
  if (entry.text === '')
    return 'Empty value: it renders as nothing, not as the default.';
  return 'Your text is sent exactly as typed.';
}

export function renderedText(
  raw: unknown,
  request: TemplateRenderRequest,
): string {
  if (
    !isRecord(raw) ||
    raw.ref !== request.ref ||
    raw.sha256 !== request.sha256 ||
    typeof raw.text !== 'string'
  )
    throw new Error(
      'The preview did not match the template you reviewed. Preview again.',
    );
  return raw.text;
}

export function renderProblems(
  errors: unknown,
  variables: readonly TemplateVariable[],
) {
  const declared = new Set(variables.map(({ name }) => name));
  const fields: Record<string, string> = {};
  const general: { key: string; reason: string }[] = [];
  for (const item of Array.isArray(errors) ? errors : []) {
    if (
      !isRecord(item) ||
      typeof item.key !== 'string' ||
      typeof item.reason !== 'string'
    )
      continue;
    const name = item.key.startsWith(variablePrefix)
      ? item.key.slice(variablePrefix.length)
      : '';
    if (declared.has(name) && !Object.hasOwn(fields, name))
      fields[name] = item.reason;
    else general.push({ key: item.key || 'Template', reason: item.reason });
  }
  return { fields, general };
}

export function insertedText(
  current: string,
  rendered: string,
  mode: InsertMode,
): string {
  return mode === 'replace' ? rendered : current + rendered;
}
