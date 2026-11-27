import { expect, test } from 'vitest';
import type { components } from '../api/schema';
import {
  filterTemplates,
  insertedText,
  missingVariables,
  parseListing,
  renderProblems,
  requestVariables,
  type Entries,
} from './model';

type Template = components['schemas']['Template'];
const hash = 'a'.repeat(64);
const template: Template = {
  ref: 'user:portrait',
  scope: 'user',
  pack_id: null,
  id: 'portrait',
  title: 'Portrait',
  description: null,
  variables: [
    { name: 'lighting', description: null, required: false, default: 'soft' },
    { name: 'subject', description: null, required: true, default: null },
  ],
  sha256: hash,
  text: 'A {{subject}}, {{lighting}}',
};

test('parses a listing and rejects a malformed one', () => {
  expect(
    parseListing({ templates: [template], diagnostics: [] }).templates,
  ).toEqual([template]);
  expect(() =>
    parseListing({
      templates: [{ ...template, sha256: 'not-a-hash' }],
      diagnostics: [],
    }),
  ).toThrow('expected format');
  expect(() => parseListing({ templates: [] })).toThrow('expected format');
});

test('requests only touched variables and reports missing required ones', () => {
  const entries: Entries = {
    lighting: { kind: 'text', text: '' },
    subject: { kind: 'absent' },
  };
  expect(requestVariables(template, entries)).toEqual({ lighting: '' });
  expect(missingVariables(template, entries)).toEqual(['subject']);
  expect(
    missingVariables(template, { subject: { kind: 'text', text: 'cat' } }),
  ).toEqual([]);
});

test('maps render errors to fields and keeps the rest general', () => {
  const mapped = renderProblems(
    [
      { key: 'variables.subject', reason: 'Too long.' },
      { key: 'variables.unknown', reason: 'Not declared.' },
      { key: '', reason: '2 more problems were omitted' },
    ],
    template.variables,
  );
  expect(mapped.fields).toEqual({ subject: 'Too long.' });
  expect(mapped.general).toEqual([
    { key: 'variables.unknown', reason: 'Not declared.' },
    { key: 'Template', reason: '2 more problems were omitted' },
  ]);
});

test('inserts replace or append without adding a separator', () => {
  expect(insertedText('old', 'new', 'replace')).toBe('new');
  expect(insertedText('old ', 'new', 'append')).toBe('old new');
});

test('filters by title, id, reference and description', () => {
  const other = {
    ...template,
    id: 'street',
    title: 'Street',
    ref: 'pack:x:s.toml',
    description: 'Night',
  };
  expect(filterTemplates([template, other], ' night ')).toEqual([other]);
  expect(filterTemplates([template, other], '')).toHaveLength(2);
});
