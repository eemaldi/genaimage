import { expect, it } from 'vitest';
import {
  BindingFloat,
  formatExactJson,
  parseBindingJson,
  stringifyBindingJson,
} from '../../../lib/api/client';
import {
  buildManifest,
  draftFromProposal,
  groupByClass,
  hasErrors,
  locateError,
  parseExactNumber,
  parseProposal,
  validateDraft,
} from './model';

const proposalText = `{
  "graph_sha256": "abc",
  "manifest": {
    "schema_version": 2, "id": "flow", "version": "1", "title": "Flow",
    "workflow": "api-workflow.json",
    "parameters": {
      "seed": {"node": "3", "input": "seed", "kind": "seed", "required": true,
        "default": 18446744073709551615, "description": "Seed"},
      "cfg": {"node": "3", "input": "cfg", "kind": "number", "required": true,
        "default": 2.0, "minimum": -0.0, "maximum": 30.0, "step": 0.5,
        "description": "Guidance"},
      "prompt": {"node": "6", "input": "text", "kind": "string", "required": true,
        "description": "Prompt"}
    },
    "presets": {"fast": {"cfg": 2.0}},
    "outputs": [{"node": "9", "kind": "image"}]
  },
  "recognised": [], "unknown_nodes": [], "unbound_inputs": [], "warnings": []
}`;

function load() {
  return draftFromProposal(parseProposal(parseBindingJson(proposalText)));
}

it('parses typed text as exact numbers', () => {
  expect(parseExactNumber('18446744073709551615')).toBe(18446744073709551615n);
  expect(String(parseExactNumber('2.0'))).toBe('2.0');
  expect(parseExactNumber('2.0')).toBeInstanceOf(BindingFloat);
  expect(String(parseExactNumber('-0.0'))).toBe('-0.0');
  expect(String(parseExactNumber('.5'))).toBe('0.5');
  expect(String(parseExactNumber('0.12345678901234567890123'))).toBe(
    '0.12345678901234567890123',
  );
  expect(parseExactNumber('12')).toBe(12);
  expect(parseExactNumber('1e999')).toBeUndefined();
  expect(parseExactNumber('abc')).toBeUndefined();
  expect(parseExactNumber('')).toBeUndefined();
});

it('keeps defaults and bounds exact through an unedited manifest', () => {
  const { draft, origin } = load();
  const manifest = buildManifest(draft, origin);
  expect(stringifyBindingJson(manifest.parameters)).toBe(
    '{"seed":{"node":"3","input":"seed","kind":"seed","required":true,"description":"Seed","default":18446744073709551615},' +
      '"cfg":{"node":"3","input":"cfg","kind":"number","required":true,"description":"Guidance","default":2.0,"minimum":-0.0,"maximum":30.0,"step":0.5},' +
      '"prompt":{"node":"6","input":"text","kind":"string","required":true,"description":"Prompt"}}',
  );
  expect(stringifyBindingJson(manifest.presets)).toBe('{"fast":{"cfg":2.0}}');
});

it('renames a parameter and drops an unexposed one without touching bindings', () => {
  const { draft, origin } = load();
  draft.parameters[0].key = 'noise_seed';
  draft.parameters[2].exposed = false;
  draft.parameters[1].maximum = '30.50';
  draft.parameters[1].group = ' Sampling ';
  const manifest = buildManifest(draft, origin);
  const parameters = manifest.parameters as Record<
    string,
    Record<string, unknown>
  >;
  expect(Object.keys(parameters)).toEqual(['noise_seed', 'cfg']);
  expect(parameters.noise_seed).toMatchObject({ node: '3', input: 'seed' });
  expect(parameters.noise_seed.default).toBe(18446744073709551615n);
  expect(formatExactJson(parameters.cfg.maximum)).toBe('30.50');
  expect(parameters.cfg.group).toBe('Sampling');
  expect(parameters.cfg.minimum).toBeInstanceOf(BindingFloat);
  expect(formatExactJson(parameters.cfg.minimum)).toBe('-0.0');
});

it('moves presets with a renamed parameter and drops those of a removed one', () => {
  const { draft, origin } = load();
  draft.parameters[1].key = 'guidance';
  expect(buildManifest(draft, origin).presets).toEqual({
    fast: { guidance: expect.any(BindingFloat) },
  });
  draft.parameters[1].exposed = false;
  expect(hasErrors(validateDraft(draft))).toBe(true);
  draft.presets[0].values = [];
  expect(buildManifest(draft, origin).presets).toEqual({ fast: {} });
});

it('validates ids, names, numbers and preset values', () => {
  const { draft } = load();
  draft.id = 'Import';
  draft.parameters[0].key = 'cfg';
  draft.parameters[1].minimum = 'low';
  draft.parameters[1].step = '0';
  draft.parameters[2].minimum = '1';
  draft.presets[0].values[0].text = 'x';
  const errors = validateDraft(draft);
  expect(errors.id).toBeTruthy();
  expect(errors.parameters.get(1)?.key).toBe('This name is already used.');
  expect(errors.parameters.get(1)?.minimum).toBe('Enter a number.');
  expect(errors.parameters.get(1)?.step).toBe(
    'The step must be greater than zero.',
  );
  expect(errors.parameters.get(2)?.minimum).toBe(
    'Only numeric parameters have this setting.',
  );
  expect(errors.presets.get(0)?.values.size).toBe(1);
  draft.id = 'refresh';
  expect(validateDraft(draft).id).toContain('reserved');
});

it('never writes null fields and keeps unknown manifest fields', () => {
  const { draft, origin } = load();
  origin.base.tags = ['a'];
  origin.base.thumbnail = null;
  draft.description = '  ';
  const manifest = buildManifest(draft, origin);
  expect(manifest.tags).toEqual(['a']);
  expect('thumbnail' in manifest).toBe(false);
  expect('description' in manifest).toBe(false);
});

it('tolerates a proposal with missing optional fields', () => {
  const proposal = parseProposal({
    manifest: { id: 'x', parameters: { a: {} } },
  });
  const { draft } = draftFromProposal(proposal);
  expect(draft.parameters[0].kind).toBe('');
  expect(proposal.recognised).toEqual([]);
  expect(proposal.unbound_inputs).toEqual([]);
});

it('groups nodes by class and maps manifest errors to steps', () => {
  expect(
    groupByClass([
      { node: '1', class_type: 'B' },
      { node: '2', class_type: 'A' },
      { node: '3', class_type: 'A' },
    ]),
  ).toEqual([
    { class_type: 'A', count: 2, nodes: ['2', '3'] },
    { class_type: 'B', count: 1, nodes: ['1'] },
  ]);
  const { draft } = load();
  expect(locateError('id', draft).step).toBe('review');
  expect(locateError('parameters.cfg', draft)).toEqual({
    step: 'review',
    label: 'Parameter cfg',
  });
  expect(locateError('presets.fast', draft).step).toBe('presets');
  expect(locateError('thumbnail', draft).step).toBe('save');
});
