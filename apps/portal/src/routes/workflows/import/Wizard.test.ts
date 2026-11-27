import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { navigateHash } from '../../../navigation';
import Wizard from './Wizard.svelte';

const graphText =
  '{"3":{"class_type":"KSampler","inputs":{"seed":18446744073709551615,"cfg":2.0,"shift":-0.0,"steps":20}},' +
  '"4":{"class_type":"VAEDecode","inputs":{}}}';
const proposalText = (extra = '') => `{
  "graph_sha256": "sha-1",
  "manifest": {
    "schema_version": 2, "id": "my-flow", "version": "1", "title": "My flow",
    "workflow": "api-workflow.json",
    "parameters": {
      "seed": {"node": "3", "input": "seed", "kind": "seed", "required": true,
        "default": 18446744073709551615, "description": "Seed"},
      "cfg": {"node": "3", "input": "cfg", "kind": "number", "required": true,
        "default": 2.0, "minimum": -0.0, "maximum": 30.0, "description": "Guidance"},
      "prompt": {"node": "6", "input": "text", "kind": "string", "required": true,
        "description": "Prompt text"}
    },
    "presets": {},
    "outputs": [{"node": "9", "kind": "image"}]
  },
  "recognised": [{"node": "3", "class_type": "KSampler",
    "bound_inputs": [{"input": "seed", "parameter": "seed"}]}],
  "unknown_nodes": [{"node": "4", "class_type": "VAEDecode"},
    {"node": "5", "class_type": "VAEDecode"}],
  "unbound_inputs": [
    {"node": "3", "class_type": "KSampler", "input": "model", "linked": true, "reason": "connection"},
    {"node": "3", "class_type": "KSampler", "input": "sampler_name", "linked": false, "reason": "failed validation"}
  ],
  "warnings": ["Prompt node 6 is not fed by a sampler."]${extra}
}`;

type Call = { url: string; body: string };
type Reply = Response | Error | Promise<Response>;
let calls: Call[];
let replies: Record<string, Reply[]>;

const json = (body: string, status = 200, type = 'application/json') =>
  new Response(body, { status, headers: { 'content-type': type } });
const problem = (status: number, type: string, extra: object = {}) =>
  json(
    JSON.stringify({
      type: `urn:genaimage:problem:${type}`,
      title: `Problem ${type}`,
      status,
      detail: `Detail of ${type}.`,
      recovery: `Recovery for ${type}.`,
      ...extra,
    }),
    status,
    'application/problem+json',
  );

beforeEach(() => {
  calls = [];
  replies = {};
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: string, init?: RequestInit) => {
      calls.push({ url: String(input), body: String(init?.body ?? '') });
      const next = replies[String(input)]?.shift();
      if (!next) throw new Error(`unexpected request ${String(input)}`);
      if (next instanceof Error) throw next;
      return next;
    }),
  );
  vi.spyOn(window, 'confirm').mockReturnValue(true);
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  window.location.hash = '';
});

const importUrl = '/api/v1/workflows/import';
const confirmUrl = '/api/v1/workflows/import/confirm';
// jsdom's File has no text().
const file = (text = graphText, name = 'My Flow.json') =>
  Object.assign(new File([text], name, { type: 'application/json' }), {
    text: async () => text,
  });
const confirmCalls = () => calls.filter((call) => call.url === confirmUrl);
const heading = () => document.activeElement?.textContent ?? '';

async function start(reply: Reply = json(proposalText()), input = file()) {
  replies[importUrl] = [reply];
  const user = userEvent.setup();
  render(Wizard);
  await user.upload(screen.getByLabelText('Workflow JSON file'), input);
  return user;
}
async function toSave(user: ReturnType<typeof userEvent.setup>) {
  for (let step = 0; step < 3; step++)
    await user.click(await screen.findByRole('button', { name: 'Continue' }));
  await screen.findByRole('button', { name: 'Save workflow' });
}

it('sends the graph with exact numbers and a name taken from the file', async () => {
  await start();
  await screen.findByRole('heading', { name: '2. Review parameters' });
  expect(calls).toEqual([
    {
      url: importUrl,
      body: '{"graph":{"3":{"class_type":"KSampler","inputs":{"seed":18446744073709551615,"cfg":2.0,"shift":-0.0,"steps":20}},"4":{"class_type":"VAEDecode","inputs":{}}},"name":"My Flow"}',
    },
  ]);
  expect(heading()).toBe('2. Review parameters');
});

it('shows the Export (API) guidance for a UI-format file', async () => {
  await start(
    problem(422, 'workflow-ui-format', {
      recovery: 'In ComfyUI choose Workflow, then Export (API).',
    }),
  );
  const alert = await screen.findByRole('alert');
  expect(alert.textContent).toContain('Export (API)');
  expect(
    screen.queryByRole('heading', { name: '2. Review parameters' }),
  ).toBeNull();
});

it('lists invalid graph errors with their keys', async () => {
  await start(
    problem(422, 'workflow-import-invalid', {
      errors: [{ key: 'graph.7', reason: 'node has no class_type' }],
    }),
  );
  expect((await screen.findByRole('alert')).textContent).toContain(
    'graph.7: node has no class_type',
  );
});

it('refuses a file over the limit and reports invalid JSON without calling the server', async () => {
  await start(
    json('unused'),
    new File([new Uint8Array(2 * 1024 * 1024 + 1)], 'big.json'),
  );
  expect((await screen.findByRole('alert')).textContent).toContain('too large');
  const user = userEvent.setup();
  await user.upload(screen.getByLabelText('Workflow JSON file'), file('{nope'));
  await waitFor(() =>
    expect(screen.getByRole('alert').textContent).toContain('not valid JSON'),
  );
  await user.upload(screen.getByLabelText('Workflow JSON file'), file('[]'));
  await waitFor(() =>
    expect(screen.getByRole('alert').textContent).toContain(
      'not a workflow graph',
    ),
  );
  expect(calls).toEqual([]);
});

it('shows a server 413', async () => {
  await start(problem(413, 'payload-too-large'));
  expect((await screen.findByRole('alert')).textContent).toContain(
    'payload-too-large',
  );
});

it('ignores an import response for a replaced file', async () => {
  let release!: (response: Response) => void;
  const slow = new Promise<Response>((done) => (release = done));
  replies[importUrl] = [
    slow,
    json(proposalText().replace('my-flow', 'second')),
  ];
  const user = userEvent.setup();
  render(Wizard);
  const input = screen.getByLabelText('Workflow JSON file');
  await user.upload(input, file(graphText, 'first.json'));
  await waitFor(() => expect(calls).toHaveLength(1));
  await user.upload(input, file(graphText, 'second.json'));
  await screen.findByRole('heading', { name: '2. Review parameters' });
  release(json(proposalText().replace('my-flow', 'first')));
  await new Promise((done) => setTimeout(done, 20));
  expect((screen.getByLabelText('Workflow ID') as HTMLInputElement).value).toBe(
    'second',
  );
});

it('summarises what the importer found without listing every node', async () => {
  await start();
  await screen.findByRole('heading', { name: '2. Review parameters' });
  expect(screen.getByText(/Prompt node 6 is not fed/)).toBeTruthy();
  expect(screen.getByText('Nodes without import rules (2)')).toBeTruthy();
  expect(screen.getByText(/VAEDecode/, { selector: 'code' })).toBeTruthy();
  expect(screen.getByText(/1 of them are connections/)).toBeTruthy();
  expect(screen.getByText(/sampler_name/, { selector: 'code' })).toBeTruthy();
});

it('caps long unbound lists', async () => {
  const many = Array.from(
    { length: 400 },
    (_, at) =>
      `{"node":"${at}","class_type":"KSampler","input":"in${at}","linked":false,"reason":"r"}`,
  ).join(',');
  await start(
    json(
      proposalText().replace(
        /"unbound_inputs": \[[\s\S]*?\],\n/,
        `"unbound_inputs": [${many}],\n`,
      ),
    ),
  );
  await screen.findByRole('heading', { name: '2. Review parameters' });
  const details = screen.getByText('Inputs without a parameter (400)')
    .parentElement as HTMLElement;
  expect(details.querySelectorAll('li')).toHaveLength(50);
  expect(within(details).getByText(/350 more not shown/)).toBeTruthy();
});

it('saves exactly what was edited, then links to Generate and the detail page', async () => {
  const user = await start();
  await screen.findByRole('heading', { name: '2. Review parameters' });
  await user.clear(screen.getByLabelText('Title'));
  await user.type(screen.getByLabelText('Title'), 'Renamed flow');
  const seed = document.querySelector('[data-parameter="seed"]') as HTMLElement;
  await user.clear(within(seed).getByLabelText('Name'));
  await user.type(within(seed).getByLabelText('Name'), 'noise_seed');
  await user.click(screen.getByLabelText('Expose prompt in the form'));
  const cfg = document.querySelector('[data-parameter="cfg"]') as HTMLElement;
  await user.clear(within(cfg).getByLabelText('Maximum'));
  await user.type(within(cfg).getByLabelText('Maximum'), '30.50');
  await user.click(screen.getByRole('button', { name: 'Continue' }));
  await user.click(await screen.findByRole('button', { name: 'Add a preset' }));
  await user.type(screen.getByLabelText('Preset name'), 'fast');
  await user.click(screen.getByRole('button', { name: 'Add a value' }));
  await user.clear(screen.getByLabelText('Value 1'));
  await user.type(screen.getByLabelText('Value 1'), '18446744073709551614');
  await user.click(screen.getByRole('button', { name: 'Continue' }));
  await user.click(await screen.findByRole('button', { name: 'Continue' }));
  await screen.findByRole('heading', { name: '5. Review and save' });
  expect(screen.getByText('Renamed flow')).toBeTruthy();
  replies[confirmUrl] = [
    json(
      '{"id":"my-flow","revision":"r1","manifest_path":"/w/my-flow/workflow.genaimage.json","source":"user"}',
    ),
  ];
  await user.click(screen.getByRole('button', { name: 'Save workflow' }));
  const link = await screen.findByRole('link', {
    name: 'Generate with this workflow',
  });
  expect(link.getAttribute('href')).toBe('#/generate?workflow=my-flow');
  expect(
    screen
      .getByRole('link', { name: 'View workflow details' })
      .getAttribute('href'),
  ).toBe('#/workflows/my-flow');
  expect(confirmCalls()).toHaveLength(1);
  expect(confirmCalls()[0].body).toBe(
    '{"graph":{"3":{"class_type":"KSampler","inputs":{"seed":18446744073709551615,"cfg":2.0,"shift":-0.0,"steps":20}},"4":{"class_type":"VAEDecode","inputs":{}}},' +
      '"graph_sha256":"sha-1","manifest":{"schema_version":2,"id":"my-flow","version":"1","title":"Renamed flow","workflow":"api-workflow.json",' +
      '"parameters":{"noise_seed":{"node":"3","input":"seed","kind":"seed","required":true,"description":"Seed","default":18446744073709551615},' +
      '"cfg":{"node":"3","input":"cfg","kind":"number","required":true,"description":"Guidance","default":2.0,"minimum":-0.0,"maximum":30.50}},' +
      '"presets":{"fast":{"noise_seed":18446744073709551614}},"outputs":[{"node":"9","kind":"image"}]}}',
  );
});

it('previews the form from the edited manifest without assistant or upload calls', async () => {
  const user = await start();
  await screen.findByRole('heading', { name: '2. Review parameters' });
  const seed = document.querySelector('[data-parameter="seed"]') as HTMLElement;
  await user.clear(within(seed).getByLabelText('Name'));
  await user.type(within(seed).getByLabelText('Name'), 'noise_seed');
  await user.click(screen.getByLabelText('Expose cfg in the form'));
  await user.click(screen.getByRole('button', { name: 'Continue' }));
  await user.click(await screen.findByRole('button', { name: 'Continue' }));
  await screen.findByRole('heading', { name: '4. Preview the form' });
  expect(screen.getAllByText('noise_seed').length).toBeGreaterThan(0);
  expect(screen.queryByText('cfg')).toBeNull();
  expect(screen.getByText('prompt', { selector: 'label' })).toBeTruthy();
  expect(screen.queryByRole('button', { name: /assist|suggest/i })).toBeNull();
  expect(calls).toHaveLength(1);
});

it('blocks continuing while a field is invalid and says why', async () => {
  const user = await start();
  await screen.findByRole('heading', { name: '2. Review parameters' });
  await user.clear(screen.getByLabelText('Workflow ID'));
  await user.type(screen.getByLabelText('Workflow ID'), 'Bad Id');
  await user.click(screen.getByRole('button', { name: 'Continue' }));
  expect((await screen.findAllByRole('alert')).length).toBeGreaterThan(0);
  expect(
    screen.getByRole('heading', { name: '2. Review parameters' }),
  ).toBeTruthy();
});

it('maps manifest errors to the fields that own them', async () => {
  const user = await start();
  await toSave(user);
  replies[confirmUrl] = [
    problem(422, 'workflow-manifest-invalid', {
      errors: [
        { key: 'parameters.seed', reason: 'minimum is above maximum' },
        { key: 'id', reason: 'reserved' },
        { key: 'and 3 more', reason: '' },
      ],
    }),
  ];
  await user.click(screen.getByRole('button', { name: 'Save workflow' }));
  const item = (await screen.findByText('Parameter seed:')).closest(
    'li',
  ) as HTMLElement;
  expect(item.textContent).toContain('minimum is above maximum');
  await user.click(within(item).getByRole('button', { name: 'Fix in step 2' }));
  await screen.findByRole('heading', { name: '2. Review parameters' });
  expect(screen.getByText('minimum is above maximum')).toBeTruthy();
  expect(screen.getByText('reserved')).toBeTruthy();
});

it('offers the suggested id for an existing workflow and saves again with it', async () => {
  const user = await start();
  await toSave(user);
  replies[confirmUrl] = [
    problem(409, 'workflow-exists', { suggested_id: 'my-flow-2' }),
    json(
      '{"id":"my-flow-2","revision":"r","manifest_path":"/p","source":"user"}',
    ),
  ];
  await user.click(screen.getByRole('button', { name: 'Save workflow' }));
  await user.click(
    await screen.findByRole('button', { name: 'Use my-flow-2 and save' }),
  );
  const link = await screen.findByRole('link', {
    name: 'Generate with this workflow',
  });
  expect(link.getAttribute('href')).toBe('#/generate?workflow=my-flow-2');
  const [first, second] = confirmCalls().map((call) => JSON.parse(call.body));
  expect(first.manifest.id).toBe('my-flow');
  expect(second.manifest.id).toBe('my-flow-2');
});

it('reports a written but unavailable workflow without success or a repeat offer', async () => {
  const user = await start();
  await toSave(user);
  replies[confirmUrl] = [
    problem(409, 'workflow-ambiguous', {
      id: 'my-flow',
      manifest_path: '/w/my-flow/workflow.genaimage.json',
    }),
  ];
  await user.click(screen.getByRole('button', { name: 'Save workflow' }));
  const alert = await screen.findByText(
    'The workflow was written, but it is not available yet.',
  );
  expect(alert.closest('[role="alert"]')?.textContent).toContain(
    '/w/my-flow/workflow.genaimage.json',
  );
  expect(screen.queryByRole('button', { name: /save/i })).toBeNull();
  expect(screen.queryByRole('link', { name: /Generate with/ })).toBeNull();
  expect(heading()).toBe('Workflow written but unavailable');
});

it('explains a graph mismatch and offers a fresh import instead of a retry', async () => {
  const user = await start();
  await toSave(user);
  replies[confirmUrl] = [
    problem(409, 'workflow-graph-mismatch', { expected_graph_sha256: 'other' }),
  ];
  await user.click(screen.getByRole('button', { name: 'Save workflow' }));
  await screen.findByText(/nothing was written/);
  expect(screen.queryByRole('button', { name: 'Save workflow' })).toBeNull();
  expect(
    screen.getByRole('button', { name: 'Choose the file again' }),
  ).toBeTruthy();
});

it('re-sends a byte-identical body after a network error', async () => {
  const user = await start();
  await toSave(user);
  replies[confirmUrl] = [
    new TypeError('network down'),
    json(
      '{"id":"my-flow","revision":"r","manifest_path":"/p","source":"user"}',
    ),
  ];
  await user.click(screen.getByRole('button', { name: 'Save workflow' }));
  const retry = await screen.findByRole('button', {
    name: 'Send the same save again',
  });
  expect(screen.queryByText(/written and is ready/)).toBeNull();
  await user.click(retry);
  await screen.findByRole('link', { name: 'Generate with this workflow' });
  const [first, second] = confirmCalls();
  expect(second.body).toBe(first.body);
  expect(first.body).toContain('"seed":18446744073709551615');
});

it('refuses to save when the browser cannot write exact numbers', async () => {
  const user = await start();
  await toSave(user);
  const raw = (JSON as { rawJSON?: unknown }).rawJSON;
  Object.defineProperty(JSON, 'rawJSON', {
    value: undefined,
    configurable: true,
  });
  try {
    await user.click(screen.getByRole('button', { name: 'Save workflow' }));
    expect((await screen.findByRole('alert')).textContent).toContain(
      'keep numbers exact',
    );
    expect(confirmCalls()).toHaveLength(0);
  } finally {
    Object.defineProperty(JSON, 'rawJSON', { value: raw, configurable: true });
  }
});

it('guards navigation between import and save', async () => {
  const user = await start();
  await screen.findByRole('heading', { name: '2. Review parameters' });
  const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
  navigateHash('#/jobs');
  expect(confirm).toHaveBeenCalledOnce();
  expect(window.location.hash).not.toBe('#/jobs');
  confirm.mockReturnValue(true);
  navigateHash('#/jobs');
  expect(window.location.hash).toBe('#/jobs');
  void user;
});

it('does not guard navigation once the workflow is saved', async () => {
  const user = await start();
  await toSave(user);
  replies[confirmUrl] = [
    json(
      '{"id":"my-flow","revision":"r","manifest_path":"/p","source":"user"}',
    ),
  ];
  await user.click(screen.getByRole('button', { name: 'Save workflow' }));
  await screen.findByRole('link', { name: 'Generate with this workflow' });
  const confirm = vi.spyOn(window, 'confirm');
  navigateHash('#/jobs');
  expect(confirm).not.toHaveBeenCalled();
  expect(window.location.hash).toBe('#/jobs');
});
