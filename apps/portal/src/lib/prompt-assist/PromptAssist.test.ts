import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { api, ApiProblemError } from '../api/client';
import type { components } from '../api/schema';
import { mockNativeDialog } from '../../test/dialog';
import PromptAssist from './PromptAssist.svelte';

mockNativeDialog();
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

type Response = components['schemas']['PromptAssistResponse'];
const proposal: Response = {
  applied: false,
  mode: 'expand',
  model: 'local-text',
  proposal_count: 1,
  proposals: [
    {
      text: 'A lighthouse at blue hour',
      diff: [
        { op: 'equal', text: 'A lighthouse' },
        { op: 'insert', text: ' at blue hour' },
      ],
    },
  ],
  template: { name: 'prompt-expand', sha256: 'hash' },
};

function available() {
  vi.spyOn(api, 'health').mockResolvedValue({
    self: 'ok',
    image_backend: 'ok',
    llm: 'ok',
    llm_text: { status: 'ok', model: 'local-text' },
  });
}

type Props = {
  workflowId: string;
  parameter: string;
  value: string;
  positivePrompt?: string;
  negative?: boolean;
  disabled?: boolean;
  accept: (value: string) => void;
};

function mount(overrides: Partial<Props> = {}) {
  return render(PromptAssist, {
    workflowId: 'portrait',
    parameter: 'prompt',
    value: 'A lighthouse',
    positivePrompt: 'A lighthouse',
    accept: vi.fn(),
    ...overrides,
  });
}

it.each([
  ['Expand', 'expand', undefined],
  ['Negative', 'negative', undefined],
  ['Refine', 'refine', 'Make it cinematic'],
  ['Variations', 'variations', undefined],
] as const)(
  '%s proposes without applying',
  async (label, mode, instruction) => {
    available();
    const accept = vi.fn();
    const response = { ...proposal, mode };
    const assist = vi.spyOn(api, 'promptAssist').mockResolvedValue(response);
    const user = userEvent.setup();
    mount({ accept, negative: label === 'Negative' });
    await screen.findByRole('button', { name: label });
    await user.click(screen.getByRole('button', { name: label }));
    if (instruction) {
      await user.type(
        screen.getByLabelText('Refinement instruction'),
        instruction,
      );
      await user.click(screen.getByRole('button', { name: 'Propose' }));
    }
    if (mode === 'variations') {
      await user.selectOptions(
        screen.getByLabelText('Number of variations'),
        '2',
      );
      await user.click(screen.getByRole('button', { name: 'Propose' }));
    }
    await screen.findByRole('button', { name: 'Accept' });
    expect(accept).not.toHaveBeenCalled();
    expect(assist).toHaveBeenCalledTimes(1);
    expect(assist.mock.calls[0][0]).toMatchObject({
      mode,
      workflow_id: 'portrait',
      parameter: 'prompt',
      ...(instruction ? { instruction } : {}),
      ...(mode === 'variations' ? { n: 2 } : {}),
      ...(mode === 'negative' ? { original: 'A lighthouse' } : {}),
    });
  },
);

it('accepts one proposal and Undo restores only the previous field value', async () => {
  available();
  const accept = vi.fn();
  vi.spyOn(api, 'promptAssist').mockResolvedValue(proposal);
  const user = userEvent.setup();
  const view = mount({ accept });
  await user.click(await screen.findByRole('button', { name: 'Expand' }));
  await user.click(await screen.findByRole('button', { name: 'Accept' }));
  expect(accept).toHaveBeenCalledWith('A lighthouse at blue hour');
  await view.rerender({
    workflowId: 'portrait',
    parameter: 'prompt',
    value: 'A lighthouse at blue hour',
    accept,
  });
  await user.click(screen.getByRole('button', { name: 'Undo' }));
  expect(accept).toHaveBeenLastCalledWith('A lighthouse');
});

it('Discard leaves the field unchanged', async () => {
  available();
  const accept = vi.fn();
  vi.spyOn(api, 'promptAssist').mockResolvedValue(proposal);
  const user = userEvent.setup();
  mount({ accept });
  await user.click(await screen.findByRole('button', { name: 'Expand' }));
  await user.click(await screen.findByRole('button', { name: 'Discard' }));
  expect(accept).not.toHaveBeenCalled();
});

it('does not apply a stale response after the field was edited', async () => {
  available();
  let resolve!: (response: Response) => void;
  vi.spyOn(api, 'promptAssist').mockReturnValue(
    new Promise((done) => {
      resolve = done;
    }),
  );
  const accept = vi.fn();
  const user = userEvent.setup();
  const view = mount({ accept });
  await user.click(await screen.findByRole('button', { name: 'Expand' }));
  await view.rerender({
    workflowId: 'portrait',
    parameter: 'prompt',
    value: 'An edited lighthouse',
    accept,
  });
  resolve(proposal);
  expect(
    await screen.findByText(/field changed while assistance/),
  ).toBeTruthy();
  expect(
    (screen.getByRole('button', { name: 'Accept' }) as HTMLButtonElement)
      .disabled,
  ).toBe(true);
  expect(accept).not.toHaveBeenCalled();
});

it('shows a 503 reason and points to Settings', async () => {
  available();
  vi.spyOn(api, 'promptAssist').mockRejectedValue(
    new ApiProblemError({
      type: 'urn:test:assistant-unavailable',
      status: 503,
      title: 'Text model unavailable',
      detail: 'The configured text role is not reachable.',
      recovery: 'Start the model server.',
    }),
  );
  const user = userEvent.setup();
  mount();
  await user.click(await screen.findByRole('button', { name: 'Expand' }));
  expect(
    await screen.findAllByText(/The configured text role is not reachable/),
  ).not.toHaveLength(0);
  expect(
    screen.getAllByRole('link', { name: 'Settings' })[0].getAttribute('href'),
  ).toBe('#/settings');
});

it('renders model markup as text in proposals and diffs', async () => {
  available();
  const text = '<img src=x onerror=alert(1)> luminous';
  vi.spyOn(api, 'promptAssist').mockResolvedValue({
    ...proposal,
    proposals: [
      {
        text,
        diff: [{ op: 'insert', text }],
      },
    ],
  });
  const user = userEvent.setup();
  const { container } = mount();
  await user.click(await screen.findByRole('button', { name: 'Expand' }));
  await waitFor(() => expect(screen.getAllByText(text)).toHaveLength(2));
  expect(container.querySelector('img')).toBeNull();
});
