import { cleanup, render, screen } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import App from '../../../App.svelte';
import { mockNativeDialog } from '../../../test/dialog';

mockNativeDialog();

const graph =
  '{"3":{"class_type":"KSampler","inputs":{"seed":18446744073709551615,"cfg":2.0}}}';
const proposal = `{"graph_sha256":"sha","manifest":{"schema_version":2,"id":"imported","version":"1","title":"Imported",
  "workflow":"api-workflow.json","parameters":{
  "seed":{"node":"3","input":"seed","kind":"seed","required":true,"default":18446744073709551615,"description":"Seed"},
  "cfg":{"node":"3","input":"cfg","kind":"number","required":true,"default":2.0,"minimum":0.0,"maximum":30.0,"description":"Guidance"}},
  "presets":{},"outputs":[{"node":"9","kind":"image"}]},
  "recognised":[],"unknown_nodes":[],"unbound_inputs":[],"warnings":[]}`;

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  window.location.hash = '';
});

it('generates with an imported workflow straight after saving', async () => {
  let saved: { manifest: Record<string, unknown> } | undefined;
  const requested: string[] = [];
  const ok = (body: string) =>
    new Response(body, { headers: { 'content-type': 'application/json' } });
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: string, init?: RequestInit) => {
      const url = String(input);
      requested.push(`${init?.method ?? 'GET'} ${url}`);
      if (url === '/api/v1/workflows/import') return ok(proposal);
      if (url === '/api/v1/workflows/import/confirm') {
        saved = JSON.parse(String(init?.body));
        return ok(
          '{"id":"imported","revision":"r1","manifest_path":"/w/imported/workflow.genaimage.json","source":"user"}',
        );
      }
      if (url === '/api/v1/workflows/imported' && saved)
        return ok(
          JSON.stringify({
            ...saved.manifest,
            presets: {},
            revision: 'r1',
          }),
        );
      if (url === '/api/v1/workflows' && saved)
        return ok(
          JSON.stringify({
            workflows: [
              {
                id: 'imported',
                title: 'Imported',
                version: '1',
                source: 'user',
              },
            ],
            diagnostics: [],
          }),
        );
      return new Response(
        '{"type":"urn:x","title":"Not mocked","status":404,"detail":"d","recovery":"r"}',
        {
          status: 404,
          headers: { 'content-type': 'application/problem+json' },
        },
      );
    }),
  );
  window.location.hash = '#/workflows/import';
  const user = userEvent.setup();
  render(App);
  await user.upload(
    await screen.findByLabelText('Workflow JSON file'),
    Object.assign(new File([graph], 'imported.json'), {
      text: async () => graph,
    }),
  );
  for (let step = 0; step < 3; step++)
    await user.click(await screen.findByRole('button', { name: 'Continue' }));
  await user.click(
    await screen.findByRole('button', { name: 'Save workflow' }),
  );
  await user.click(
    await screen.findByRole('link', { name: 'Generate with this workflow' }),
  );
  expect(window.location.hash).toBe('#/generate?workflow=imported');
  expect(
    await screen.findByRole('heading', { name: 'Generate', level: 1 }),
  ).toBeTruthy();
  expect(
    await screen.findByLabelText('seed', {}, { timeout: 3000 }),
  ).toBeTruthy();
  expect(screen.getByLabelText('cfg')).toBeTruthy();
  expect(requested).toContain('GET /api/v1/workflows/imported');
});
