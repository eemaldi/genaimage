import {
  cleanup,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import App from '../../App.svelte';
import { api, ApiProblemError, BindingFloat } from '../api/client';
import type { components } from '../api/schema';
import { mockNativeDialog } from '../../test/dialog';
import WorkflowSuggest from './WorkflowSuggest.svelte';

mockNativeDialog();
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  window.location.hash = '';
});

type SuggestionResponse = components['schemas']['SuggestWorkflowResponse'];
const response: SuggestionResponse = {
  applied: false,
  model: 'local-text',
  template: { name: 'suggest', sha256: 'hash' },
  truncated: true,
  dropped: [
    {
      workflow_id: 'old-workflow',
      field: 'seed',
      reason: 'The value was not exact.',
    },
  ],
  suggestions: [
    {
      workflow_id: 'portrait',
      workflow_revision: 'suggested-revision',
      rank: 1,
      preset: 'Studio',
      parameters: {
        Prompt: 'A lighthouse',
        Seed: 9007199254740993n,
        Strength: new BindingFloat(2),
        Offset: new BindingFloat(-0),
      },
      rationale: '<strong>Best match</strong>',
      parameter_notes: { Strength: 'Keep this precise value.' },
    },
  ],
};

const currentWorkflow: components['schemas']['WorkflowDetails'] = {
  id: 'portrait',
  title: 'Portrait',
  version: '2',
  revision: 'current-revision',
  schema_version: 2,
  workflow: 'portrait.json',
  outputs: [],
  presets: { Studio: {} },
  parameters: {
    Prompt: {
      node: '1',
      input: 'text',
      kind: 'string',
      required: true,
      description: 'Positive prompt',
    },
    Seed: {
      node: '2',
      input: 'seed',
      kind: 'seed',
      required: true,
      description: '',
    },
    Strength: {
      node: '3',
      input: 'strength',
      kind: 'number',
      required: true,
      description: '',
    },
    Offset: {
      node: '4',
      input: 'offset',
      kind: 'number',
      required: true,
      description: '',
    },
    Reference: {
      node: '5',
      input: 'image',
      kind: 'image_upload',
      required: false,
      description: 'User-selected image',
    },
  },
};

function mockGenerateApis() {
  vi.spyOn(api, 'listWorkflows').mockResolvedValue({
    workflows: [
      {
        id: 'portrait',
        name: 'Portrait',
        version: '2',
        revision: 'current-revision',
      },
    ],
    diagnostics: [],
  });
  vi.spyOn(api, 'getWorkflow').mockResolvedValue(currentWorkflow);
  vi.spyOn(api, 'getSettings').mockResolvedValue({
    config_file: '',
    config_file_exists: true,
    api_key_set: false,
    api_key_read_only: false,
    restart_required: [],
    settings: {},
  });
}

function available() {
  vi.spyOn(api, 'health').mockResolvedValue({
    self: 'ok',
    image_backend: 'ok',
    llm: 'ok',
    llm_text: { status: 'ok', model: 'local-text' },
  });
}

async function openSuggestions(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole('button', { name: 'Suggest workflow' }));
  await user.type(
    screen.getByLabelText('What do you want to create?'),
    'A lighthouse',
  );
  await user.click(screen.getByRole('button', { name: 'Suggest' }));
}

it('shows ranked suggestions, notes and dropped or truncated results', async () => {
  available();
  vi.spyOn(api, 'suggestWorkflow').mockResolvedValue(response);
  const user = userEvent.setup();
  const { container } = render(WorkflowSuggest);
  await openSuggestions(user);
  await screen.findByRole('heading', { name: '#1 portrait' });
  expect(screen.getByText('<strong>Best match</strong>')).toBeTruthy();
  expect(container.querySelector('strong')?.textContent).not.toBe('Best match');
  expect(screen.getByText('Keep this precise value.')).toBeTruthy();
  expect(screen.getByText(/old-workflow \/ seed/)).toBeTruthy();
  expect(screen.getByText(/More workflows matched/)).toBeTruthy();
});

it('Use this suggestion opens Generate prefilled, pinned and unsubmitted with exact numbers', async () => {
  available();
  vi.spyOn(api, 'suggestWorkflow').mockResolvedValue(response);
  mockGenerateApis();
  const createRun = vi.spyOn(api, 'createRun');
  window.location.hash = '#/workflows';
  const user = userEvent.setup();
  render(App);
  await screen.findByRole('link', { name: 'Portrait' });
  await openSuggestions(user);
  const use = await screen.findByRole('link', { name: 'Use this suggestion' });
  const href = use.getAttribute('href') ?? '';
  expect(new URLSearchParams(href.split('?')[1]).get('wr')).toBe(
    'suggested-revision',
  );
  expect(new URLSearchParams(href.split('?')[1]).get('assistant')).toBe(
    'suggest',
  );
  await user.click(use);
  const warningHeading = await screen.findByRole('heading', {
    name: 'Assistant suggestion needs review',
  });
  const warning = warningHeading.closest('section')!;
  expect(within(warning).getByText('suggested-revision')).toBeTruthy();
  expect(within(warning).getByText('current-revision')).toBeTruthy();
  expect(within(warning).getByText('9007199254740993')).toBeTruthy();
  expect(within(warning).getByText('2.0')).toBeTruthy();
  expect(within(warning).getByText('-0.0')).toBeTruthy();
  expect(screen.getByLabelText<HTMLInputElement>('Prompt').value).toBe('');
  expect(screen.getByLabelText<HTMLInputElement>('Seed').value).toBe('');
  expect(screen.getByLabelText<HTMLSelectElement>('Preset').value).toBe('');
  expect(
    (
      screen.getByRole('button', {
        name: 'Review parameters',
      }) as HTMLButtonElement
    ).disabled,
  ).toBe(true);
  expect(screen.queryByRole('button', { name: 'Submit run' })).toBeNull();
  await user.click(
    screen.getByRole('button', { name: 'Apply to current revision' }),
  );
  expect(screen.getByLabelText<HTMLInputElement>('Prompt').value).toBe(
    'A lighthouse',
  );
  expect(screen.getByLabelText<HTMLInputElement>('Seed').value).toBe(
    '9007199254740993',
  );
  expect(screen.getByLabelText<HTMLInputElement>('Strength').value).toBe('2.0');
  expect(screen.getByLabelText<HTMLInputElement>('Offset').value).toBe('-0.0');
  expect(screen.getByLabelText<HTMLSelectElement>('Preset').value).toBe(
    'Studio',
  );
  expect(
    screen.queryByRole('button', { name: 'Apply to current revision' }),
  ).toBeNull();
  expect(
    (screen.getByLabelText('Reference') as HTMLInputElement).files,
  ).toHaveLength(0);
  expect(createRun).not.toHaveBeenCalled();
});

it('discards a mismatched suggestion without changing current form values', async () => {
  available();
  vi.spyOn(api, 'suggestWorkflow').mockResolvedValue(response);
  mockGenerateApis();
  window.location.hash = '#/workflows';
  const user = userEvent.setup();
  render(App);
  await screen.findByRole('link', { name: 'Portrait' });
  await openSuggestions(user);
  await user.click(
    await screen.findByRole('link', { name: 'Use this suggestion' }),
  );
  expect((await screen.findByLabelText<HTMLInputElement>('Prompt')).value).toBe(
    '',
  );
  const discard = screen.getByRole('button', { name: 'Discard' });
  await user.click(discard);
  expect(screen.getByText(/suggestion was discarded/)).toBeTruthy();
  expect(screen.getByLabelText<HTMLInputElement>('Prompt').value).toBe('');
  expect(screen.getByLabelText<HTMLInputElement>('Seed').value).toBe('');
  expect(screen.getByLabelText<HTMLSelectElement>('Preset').value).toBe('');
  expect(
    (
      screen.getByRole('button', {
        name: 'Review parameters',
      }) as HTMLButtonElement
    ).disabled,
  ).toBe(false);
});

it('reports invalid suggested values without coercing or applying them', async () => {
  available();
  vi.spyOn(api, 'suggestWorkflow').mockResolvedValue({
    ...response,
    suggestions: [
      {
        ...response.suggestions[0],
        parameters: { Seed: '9007199254740993.5' },
      },
    ],
  });
  mockGenerateApis();
  window.location.hash = '#/workflows';
  const user = userEvent.setup();
  render(App);
  await screen.findByRole('link', { name: 'Portrait' });
  await openSuggestions(user);
  await user.click(
    await screen.findByRole('link', { name: 'Use this suggestion' }),
  );
  await user.click(
    await screen.findByRole('button', { name: 'Apply to current revision' }),
  );
  expect(await screen.findByText(/Enter a whole number/)).toBeTruthy();
  expect(screen.getByLabelText<HTMLInputElement>('Seed').value).toBe('');
  expect(
    (
      screen.getByRole('button', {
        name: 'Review parameters',
      }) as HTMLButtonElement
    ).disabled,
  ).toBe(true);
});

it('does not apply an image newly bound by the current revision preset', async () => {
  available();
  vi.spyOn(api, 'suggestWorkflow').mockResolvedValue(response);
  vi.spyOn(api, 'listWorkflows').mockResolvedValue({
    workflows: [
      {
        id: 'portrait',
        name: 'Portrait',
        version: '2',
        revision: 'current-revision',
      },
    ],
    diagnostics: [],
  });
  vi.spyOn(api, 'getWorkflow').mockResolvedValue({
    ...currentWorkflow,
    presets: {
      Studio: { Reference: `sha256:${'a'.repeat(64)}` },
    },
  });
  vi.spyOn(api, 'getSettings').mockResolvedValue({
    config_file: '',
    config_file_exists: true,
    api_key_set: false,
    api_key_read_only: false,
    restart_required: [],
    settings: {},
  });
  window.location.hash = '#/workflows';
  const user = userEvent.setup();
  render(App);
  await screen.findByRole('link', { name: 'Portrait' });
  await openSuggestions(user);
  await user.click(
    await screen.findByRole('link', { name: 'Use this suggestion' }),
  );
  const reference = await screen.findByLabelText<HTMLInputElement>('Reference');
  expect(reference.value).toBe('');
  await user.click(
    screen.getByRole('button', { name: 'Apply to current revision' }),
  );
  expect(
    await screen.findByText(
      /Preset “Studio” now sets image input “Reference”; choose the image yourself/,
    ),
  ).toBeTruthy();
  expect(reference.value).toBe('');
  expect(screen.getByLabelText<HTMLSelectElement>('Preset').value).toBe('');
  expect(
    (
      screen.getByRole('button', {
        name: 'Review parameters',
      }) as HTMLButtonElement
    ).disabled,
  ).toBe(true);
});

it('keeps an oversized suggestion and disables its Generate link safely', async () => {
  available();
  vi.spyOn(api, 'suggestWorkflow').mockResolvedValue({
    ...response,
    suggestions: [
      {
        ...response.suggestions[0],
        parameters: { Prompt: 'x'.repeat(33_000) },
      },
    ],
  });
  const user = userEvent.setup();
  render(WorkflowSuggest);
  await openSuggestions(user);
  expect(
    await screen.findByRole('heading', { name: '#1 portrait' }),
  ).toBeTruthy();
  expect(
    (
      screen.getByRole('button', {
        name: 'Use this suggestion',
      }) as HTMLButtonElement
    ).disabled,
  ).toBe(true);
  expect(screen.getByText(/The share link is too long/)).toBeTruthy();
  expect(
    screen.queryByRole('link', { name: 'Use this suggestion' }),
  ).toBeNull();
});

it('shows a 503 disabled reason and Settings pointer', async () => {
  available();
  vi.spyOn(api, 'suggestWorkflow').mockRejectedValue(
    new ApiProblemError({
      type: 'urn:test:assistant-unavailable',
      status: 503,
      title: 'Text model unavailable',
      detail: 'The suggestion model is offline.',
      recovery: 'Start the model server.',
    }),
  );
  const user = userEvent.setup();
  render(WorkflowSuggest);
  await openSuggestions(user);
  await waitFor(() =>
    expect(
      screen.getAllByText(/The suggestion model is offline/),
    ).not.toHaveLength(0),
  );
  expect(
    screen.getByRole('link', { name: 'Settings' }).getAttribute('href'),
  ).toBe('#/settings');
});
