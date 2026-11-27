import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import {
  ApiHttpError,
  ApiProblemError,
  BindingFloat,
  stringifyBindingJson,
  type api,
} from '../../lib/api/client';
import type { components } from '../../lib/api/schema';
import type { Workflow, Parameter } from '../generate/model';
import BatchMode from './BatchMode.svelte';
import { loadPendingBatch } from './pending';
import { mockNativeDialog } from '../../test/dialog';

mockNativeDialog();

const fake = vi.hoisted(() => ({
  previewBatch: vi.fn<typeof api.previewBatch>(),
  createBatch: vi.fn<typeof api.createBatch>(),
  health: vi.fn<typeof api.health>(),
  listTemplates: vi.fn<typeof api.listTemplates>(),
  renderTemplate: vi.fn<typeof api.renderTemplate>(),
}));
vi.mock('../../lib/api/client', async (original) => ({
  ...(await original<typeof import('../../lib/api/client')>()),
  api: fake,
}));

const parameter = (
  kind: string,
  extra: Partial<Parameter> = {},
): Parameter => ({
  kind,
  node: '1',
  input: 'value',
  description: '',
  required: false,
  ...extra,
});
const workflow: Workflow = {
  revision: 'rev-1',
  id: 'portrait',
  version: '3',
  schema_version: 2,
  title: 'Portrait workflow',
  workflow: 'portrait.json',
  outputs: [],
  parameters: {
    prompt: parameter('string', { required: true, default: 'a cat' }),
    seed: parameter('seed', { default: 1 }),
    steps: parameter('integer', { default: 20 }),
    cfg: parameter('number', { default: new BindingFloat(2) as never }),
  },
  presets: {},
};
const problem = (status: number, type: string, extra = {}) =>
  new ApiProblemError({
    type,
    title: 'Problem',
    status,
    detail: 'Detail.',
    recovery: 'Recover.',
    ...extra,
  } as never);

type Preview = components['schemas']['BatchPreviewResponse'];
function previewOf(body: Parameters<typeof api.previewBatch>[0]): Preview {
  const sweep = body.sweep;
  const combos: Record<string, unknown>[] =
    'seeds' in sweep
      ? sweep.seeds.values.map((value) => ({ [sweep.seeds.parameter]: value }))
      : Object.entries(sweep.grid)
          .sort(([a], [b]) => (a < b ? -1 : 1))
          .reduce<Record<string, unknown>[]>(
            (rows, [name, values]) =>
              rows.flatMap((row) =>
                values.map((value) => ({ ...row, [name]: value })),
              ),
            [{}],
          );
  return {
    item_count: combos.length,
    max_items: body.max_items,
    max_items_cap: 256,
    workflow_revision: body.workflow_revision,
    items: combos.map((sweep_values, index) => ({
      index,
      sweep_values,
      parameters: { ...body.parameters, ...sweep_values },
      effective_parameters: { ...body.parameters, ...sweep_values },
    })),
  };
}

const reloadWorkflow = vi.fn();
const view = () =>
  render(BatchMode, { workflow, query: new URLSearchParams(), reloadWorkflow });
const set = (label: RegExp, value: string) =>
  fireEvent.input(screen.getByLabelText(label), { target: { value } });

beforeEach(() => {
  fake.health.mockResolvedValue({
    self: 'ok',
    image_backend: 'ok',
    llm: 'ok',
    llm_text: { status: 'ok', model: 'local-text' },
  } as never);
  sessionStorage.clear();
  location.hash = '';
  fake.previewBatch.mockImplementation(async (body) => previewOf(body));
  fake.createBatch.mockResolvedValue({ id: 'batch-1' } as never);
});
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

async function reviewSeeds(text = '18446744073709551615\n7') {
  view();
  await set(/Seeds for seed/, text);
  await userEvent.click(screen.getByRole('button', { name: 'Preview batch' }));
  return screen.findByRole('heading', { name: 'Review batch' });
}

test('seeds are sent exactly and the review states the run count before anything is submitted', async () => {
  const heading = await reviewSeeds();
  const body = fake.previewBatch.mock.calls[0][0];
  expect(stringifyBindingJson(body)).toBe(
    '{"workflow":"portrait","workflow_revision":"rev-1","parameters":{"prompt":"a cat","steps":20,"cfg":2.0},"sweep":{"seeds":{"parameter":"seed","values":[18446744073709551615,7]}},"max_items":2}',
  );
  expect(screen.getByText(/This will submit/).textContent).toContain('2 runs');
  expect(
    within(heading.parentElement!).getAllByText('18446744073709551615').length,
  ).toBeGreaterThan(0);
  expect(fake.createBatch).not.toHaveBeenCalled();
});

test('random seeds are written into the visible list, never added silently', async () => {
  view();
  await set(/Seeds for seed/, '5');
  await set(/Random seeds to add/, '3');
  await userEvent.click(
    screen.getByRole('button', { name: 'Add random seeds' }),
  );
  const list = screen.getByLabelText(/Seeds for seed/) as HTMLTextAreaElement;
  const lines = list.value.split('\n');
  expect(lines).toHaveLength(4);
  expect(lines[0]).toBe('5');
  expect(new Set(lines).size).toBe(4);
  expect(screen.getByText(/Added 3 random seeds to the list/)).toBeTruthy();
  expect(screen.getByText('The sweep makes 4 runs.')).toBeTruthy();
});

test('a grid keeps 2.0 as a decimal and sorts parameter names', async () => {
  view();
  await userEvent.click(screen.getByLabelText('Grid of parameter values'));
  await userEvent.click(screen.getByRole('checkbox', { name: 'steps' }));
  await userEvent.click(screen.getByRole('checkbox', { name: 'cfg' }));
  await set(/Values for cfg/, '2.0\n3.5');
  await set(/Values for steps/, '10\n20');
  expect(screen.getByText('The sweep makes 4 runs.')).toBeTruthy();
  await userEvent.click(screen.getByRole('button', { name: 'Preview batch' }));
  await screen.findByRole('heading', { name: 'Review batch' });
  expect(stringifyBindingJson(fake.previewBatch.mock.calls[0][0])).toBe(
    '{"workflow":"portrait","workflow_revision":"rev-1","parameters":{"prompt":"a cat","seed":1},"sweep":{"grid":{"cfg":[2.0,3.5],"steps":[10,20]}},"max_items":4}',
  );
  expect(screen.getAllByRole('row').length).toBe(5);
});

test('the review table is paginated so 256 items never render at once', async () => {
  const values = Array.from({ length: 256 }, (_, i) => String(i)).join('\n');
  await reviewSeeds(values);
  expect(screen.getAllByRole('row')).toHaveLength(21);
  await userEvent.click(screen.getByRole('button', { name: 'Next runs' }));
  expect(screen.getByText(/Runs 21 to 40 of 256/)).toBeTruthy();
});

test('an explicit confirmation is required and double clicks send one request', async () => {
  await reviewSeeds();
  expect(fake.createBatch).not.toHaveBeenCalled();
  let release!: (value: never) => void;
  fake.createBatch.mockReturnValue(
    new Promise((resolve) => (release = resolve)),
  );
  const submit = screen.getByRole('button', { name: 'Submit 2 runs' });
  await userEvent.dblClick(submit);
  expect(fake.createBatch).toHaveBeenCalledTimes(1);
  release({ id: 'batch-9' } as never);
  await waitFor(() => expect(location.hash).toBe('#/batches/batch-9'));
  expect(loadPendingBatch('portrait')).toBeUndefined();
});

test('an uncertain outcome keeps one key and retries the identical request', async () => {
  await reviewSeeds();
  fake.createBatch.mockRejectedValueOnce(new TypeError('offline'));
  await userEvent.click(screen.getByRole('button', { name: 'Submit 2 runs' }));
  await screen.findByText('Pending batch submission');
  const [firstBody, firstKey] = fake.createBatch.mock.calls[0];
  await userEvent.click(
    screen.getByRole('button', { name: 'Retry with the same key' }),
  );
  await waitFor(() => expect(fake.createBatch).toHaveBeenCalledTimes(2));
  const [secondBody, secondKey] = fake.createBatch.mock.calls[1];
  expect(secondKey).toBe(firstKey);
  expect(stringifyBindingJson(secondBody)).toBe(
    stringifyBindingJson(firstBody),
  );
  expect(location.hash).toBe('#/batches/batch-1');
});

test('a pending batch is restored after a reload and the form stays locked', async () => {
  await reviewSeeds();
  fake.createBatch.mockRejectedValueOnce(new ApiHttpError(503, 'Try later.'));
  await userEvent.click(screen.getByRole('button', { name: 'Submit 2 runs' }));
  await screen.findByText('Pending batch submission');
  const key = fake.createBatch.mock.calls[0][1];
  cleanup();
  view();
  expect(screen.getByText('Pending batch submission')).toBeTruthy();
  expect(screen.queryByRole('button', { name: 'Preview batch' })).toBeNull();
  expect(screen.queryByLabelText(/Seeds for seed/)).toBeNull();
  expect(screen.getByText('18446744073709551615, 7')).toBeTruthy();
  await userEvent.click(
    screen.getByRole('button', { name: 'Retry with the same key' }),
  );
  await waitFor(() => expect(fake.createBatch).toHaveBeenCalledTimes(2));
  expect(fake.createBatch.mock.calls[1][1]).toBe(key);
  expect(stringifyBindingJson(fake.createBatch.mock.calls[1][0])).toContain(
    '[18446744073709551615,7]',
  );
});

test('a workflow-changed conflict clears the pending request and asks for a new review', async () => {
  await reviewSeeds();
  fake.createBatch.mockRejectedValueOnce(
    problem(409, 'urn:genaimage:problem:workflow-changed'),
  );
  await userEvent.click(screen.getByRole('button', { name: 'Submit 2 runs' }));
  await waitFor(() => expect(reloadWorkflow).toHaveBeenCalled());
  expect(loadPendingBatch('portrait')).toBeUndefined();
  expect(screen.queryByRole('button', { name: /Submit 2 runs/ })).toBeNull();
});

test('invalid items are listed with their parameters and nothing stays pending', async () => {
  await reviewSeeds();
  fake.createBatch.mockRejectedValueOnce(
    problem(422, 'urn:genaimage:problem:batch-items-invalid', {
      invalid_item_count: 25,
      invalid_items: [
        { index: 1, errors: [{ parameter: 'seed', reason: 'Out of range.' }] },
      ],
    }),
  );
  await userEvent.click(screen.getByRole('button', { name: 'Submit 2 runs' }));
  const alert = await screen.findByRole('alert');
  expect(alert.textContent).toContain('Item 2 · seed');
  expect(alert.textContent).toContain('Out of range.');
  expect(alert.textContent).toContain(
    '25 items are invalid; the first 1 are listed',
  );
  expect(loadPendingBatch('portrait')).toBeUndefined();
  expect(screen.queryByText('Pending batch submission')).toBeNull();
});

test('a server expansion that differs from the form is refused', async () => {
  fake.previewBatch.mockImplementation(async (body) => ({
    ...previewOf(body),
    item_count: 3,
  }));
  view();
  await set(/Seeds for seed/, '1\n2');
  await userEvent.click(screen.getByRole('button', { name: 'Preview batch' }));
  expect((await screen.findByRole('alert')).textContent).toContain(
    'expected 2 runs',
  );
  expect(screen.queryByRole('heading', { name: 'Review batch' })).toBeNull();
});

test('swept parameters are left out of the shared values', async () => {
  await reviewSeeds('1\n2');
  const body = fake.previewBatch.mock.calls[0][0];
  expect(Object.keys(body.parameters ?? {})).not.toContain('seed');
});

test('an empty random seed range is reported and the list is not changed', async () => {
  const impossible = {
    ...workflow,
    parameters: {
      ...workflow.parameters,
      seed: parameter('seed', { minimum: 5, maximum: 4 }),
    },
  };
  render(BatchMode, {
    workflow: impossible,
    query: new URLSearchParams(),
    reloadWorkflow,
  });
  await set(/Seeds for seed/, '5');
  await userEvent.click(
    screen.getByRole('button', { name: 'Add random seeds' }),
  );
  expect(screen.getByText(/leave no seed between 0 and/)).toBeTruthy();
  expect(
    (screen.getByLabelText(/Seeds for seed/) as HTMLTextAreaElement).value,
  ).toBe('5');
});

test('a corrupt stored request is discarded only after confirmation and unlocks the form', async () => {
  const corrupt = 'genaimage.batch.pending.v1:portrait:corrupt-key';
  const other = 'genaimage.batch.pending.v1:other:kept-key';
  sessionStorage.setItem(
    corrupt,
    JSON.stringify({ body: '{"workflow":"portrait"', itemCount: 2 }),
  );
  sessionStorage.setItem(
    other,
    JSON.stringify({ body: '{"workflow":"other"}', itemCount: 1 }),
  );
  view();
  const discard = await screen.findByRole('button', {
    name: 'Discard stored request',
  });
  expect(
    screen.getByRole('link', { name: 'Batches in Jobs' }).getAttribute('href'),
  ).toBe('#/jobs?view=batches');
  const formFieldset = () =>
    screen
      .getByRole('button', { name: 'Preview batch' })
      .closest('fieldset') as HTMLFieldSetElement;
  expect(formFieldset().disabled).toBe(true);

  const confirm = vi.spyOn(window, 'confirm').mockReturnValueOnce(false);
  await userEvent.click(discard);
  expect(sessionStorage.getItem(corrupt)).not.toBeNull();
  expect(formFieldset().disabled).toBe(true);

  confirm.mockReturnValueOnce(true);
  await userEvent.click(discard);
  expect(confirm).toHaveBeenCalledTimes(2);
  expect(sessionStorage.getItem(corrupt)).toBeNull();
  expect(sessionStorage.getItem(other)).not.toBeNull();
  expect(formFieldset().disabled).toBe(false);
  expect(
    screen.queryByRole('button', { name: 'Discard stored request' }),
  ).toBeNull();
  confirm.mockRestore();
});

test('inserting a template clears the stale review and the next preview sends the inserted base value', async () => {
  const sha = 'c'.repeat(64);
  fake.listTemplates.mockResolvedValue({
    templates: [
      {
        ref: 'user:studio',
        scope: 'user',
        pack_id: null,
        id: 'studio',
        title: 'Studio',
        description: null,
        variables: [],
        sha256: sha,
        text: 'a dog in a studio',
      },
    ],
    diagnostics: [],
  });
  fake.renderTemplate.mockImplementation(async (body) => ({
    ref: body.ref,
    sha256: body.sha256,
    text: 'a dog in a studio',
  }));
  await reviewSeeds();
  await userEvent.click(
    screen.getByRole('button', { name: 'Use a template for prompt' }),
  );
  await userEvent.click(
    await screen.findByRole('button', { name: 'Use “Studio”' }),
  );
  await userEvent.click(screen.getByRole('button', { name: 'Preview' }));
  await screen.findByText('Preview ready. Insert adds exactly this text.');
  await userEvent.click(screen.getByLabelText('Replace the current text'));
  await userEvent.click(screen.getByRole('button', { name: 'Insert' }));
  expect(screen.queryByRole('heading', { name: 'Review batch' })).toBeNull();
  await userEvent.click(screen.getByRole('button', { name: 'Preview batch' }));
  await screen.findByRole('heading', { name: 'Review batch' });
  expect(fake.previewBatch.mock.lastCall?.[0].parameters?.prompt).toBe(
    'a dog in a studio',
  );
});
