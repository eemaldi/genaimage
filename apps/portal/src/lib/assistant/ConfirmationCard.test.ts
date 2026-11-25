import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { mockNativeDialog } from '../../test/dialog';
import { api, ApiProblemError, parseAssistantJson } from '../api/client';
import type { Confirmation } from './model';
import ConfirmationCard from './ConfirmationCard.svelte';

mockNativeDialog();

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const previewText = `{
  "workflow_id":"portrait",
  "workflow_revision":"revision-1",
  "preset":"studio",
  "parameters":{
    "seed":900719925474099312345,
    "strength":2.0,
    "offset":-0.0,
    "prompt":"<img src=x onerror=alert(1)>"
  },
  "issues":[]
}`;

function confirmation(overrides: Partial<Confirmation> = {}): Confirmation {
  return {
    confirmation_id: 'confirmation-1',
    status: 'pending',
    conversation_id: 'conversation-1',
    tool_call_id: 'tool-1',
    tool: 'submit_run',
    arguments: {
      workflow_id: 'portrait',
      workflow_revision: 'revision-1',
      preset: 'studio',
      parameters: { seed: 7n },
    },
    json_schema: {
      type: 'object',
      required: ['workflow_id', 'workflow_revision', 'parameters'],
      properties: {
        workflow_id: { type: 'string' },
        workflow_revision: { type: 'string' },
        preset: { type: 'string' },
        parameters: { type: 'object' },
      },
    },
    preview: parseAssistantJson(previewText),
    preview_token: 'token-1',
    ...overrides,
  };
}

async function* stream() {
  yield* [] as never[];
}

function mockDecided(status: Confirmation['status'] = 'succeeded') {
  vi.spyOn(api, 'decideAssistantConfirmation').mockResolvedValue(stream());
  vi.spyOn(api, 'listAssistantToolCalls').mockResolvedValue({
    tool_calls: [
      {
        id: 1,
        conversation_id: 'conversation-1',
        confirmation_id: 'confirmation-1',
        confirmation_status: status,
        tool_call_id: 'tool-1',
        tool: 'submit_run',
        proposed_arguments: {},
        validation_outcome: 'valid',
        decision: 'confirm',
        invocation_ok: status === 'succeeded',
        invocation_result:
          status === 'succeeded'
            ? { run_id: 'run-1', status: 'queued' }
            : undefined,
        created_at: '2026-01-01T00:00:00Z',
      },
    ],
  });
}

it('shows every effective value with exact number lexemes and pins Generate', () => {
  render(ConfirmationCard, {
    confirmation: confirmation(),
    conversationId: 'conversation-1',
  });
  expect(screen.getByText('900719925474099312345')).toBeTruthy();
  expect(screen.getByText('2.0')).toBeTruthy();
  expect(screen.getByText('-0.0')).toBeTruthy();
  expect(screen.getByText('"<img src=x onerror=alert(1)>"')).toBeTruthy();
  expect(screen.queryByRole('img')).toBeNull();
  const link = screen.getByRole('link', { name: 'Open in Generate' });
  const query = new URLSearchParams(link.getAttribute('href')!.split('?')[1]);
  expect(query.get('workflow')).toBe('portrait');
  expect(query.get('wr')).toBe('revision-1');
  expect(query.get('assistant')).toBe('chat');
  expect(link.textContent).toBe('Open in Generate');
  expect(screen.getByText(/It does not submit a run/)).toBeTruthy();
  expect(screen.getByText(/Not submitted/)).toBeTruthy();
});

it('confirms with the reviewed token only once', async () => {
  mockDecided();
  const decide = vi.mocked(api.decideAssistantConfirmation);
  render(ConfirmationCard, {
    confirmation: confirmation(),
    conversationId: 'conversation-1',
  });
  const button = screen.getByRole('button', { name: 'Confirm run' });
  await fireEvent.click(button);
  await fireEvent.click(button);
  expect(decide).toHaveBeenCalledTimes(1);
  expect(decide).toHaveBeenCalledWith(
    'conversation-1',
    'confirmation-1',
    { decision: 'confirm', preview_token: 'token-1' },
    expect.anything(),
  );
  expect(
    await screen.findByText('The backend confirmed the submitted run.'),
  ).toBeTruthy();
  expect(screen.getByRole('link', { name: 'Open run run-1' })).toBeTruthy();
});

it('requires a fresh preview after changing edited arguments and sends its token', async () => {
  const first = parseAssistantJson(previewText);
  const second = parseAssistantJson(
    previewText.replace('900719925474099312345', '900719925474099312346'),
  );
  const preview = vi
    .spyOn(api, 'previewAssistantConfirmation')
    .mockResolvedValueOnce({ preview: first, preview_token: 'edit-token-1' })
    .mockResolvedValueOnce({ preview: second, preview_token: 'edit-token-2' });
  mockDecided();
  const user = userEvent.setup();
  render(ConfirmationCard, {
    confirmation: confirmation(),
    conversationId: 'conversation-1',
  });
  await user.click(screen.getByRole('button', { name: 'Edit' }));
  const dialog = screen.getByRole('dialog', {
    name: 'Edit proposed run arguments',
  });
  await user.click(
    within(dialog).getByRole('button', { name: 'Use exact JSON editor' }),
  );
  const editor = within(dialog).getByLabelText('Arguments (exact JSON)');
  expect(document.activeElement).toBe(editor);
  await fireEvent.input(editor, {
    target: {
      value:
        '{"workflow_id":"portrait","workflow_revision":"revision-1","parameters":{"seed":900719925474099312345}}',
    },
  });
  const confirmEdit = within(dialog).getByRole('button', {
    name: 'Confirm edit',
  });
  expect((confirmEdit as HTMLButtonElement).disabled).toBe(true);
  await user.click(
    within(dialog).getByRole('button', { name: 'Preview edit' }),
  );
  expect((confirmEdit as HTMLButtonElement).disabled).toBe(false);
  await fireEvent.input(editor, {
    target: { value: `${(editor as HTMLTextAreaElement).value} ` },
  });
  expect((confirmEdit as HTMLButtonElement).disabled).toBe(true);
  expect(within(dialog).getByText(/Preview the edit again/)).toBeTruthy();
  await user.click(
    within(dialog).getByRole('button', { name: 'Preview edit' }),
  );
  expect((confirmEdit as HTMLButtonElement).disabled).toBe(false);
  await user.click(confirmEdit);
  expect(preview).toHaveBeenCalledTimes(2);
  expect(api.decideAssistantConfirmation).toHaveBeenCalledWith(
    'conversation-1',
    'confirmation-1',
    expect.objectContaining({
      decision: 'edit',
      preview_token: 'edit-token-2',
    }),
    expect.anything(),
  );
  const sent = vi
    .mocked(api.decideAssistantConfirmation)
    .mock.calls.at(-1)?.[2];
  expect(
    (sent?.arguments as { parameters: { seed: bigint } }).parameters.seed,
  ).toBe(900719925474099312345n);
});

it('rejects with an optional reason', async () => {
  mockDecided('rejected');
  const user = userEvent.setup();
  render(ConfirmationCard, {
    confirmation: confirmation(),
    conversationId: 'conversation-1',
  });
  await user.click(screen.getByRole('button', { name: 'Reject' }));
  const dialog = screen.getByRole('dialog', { name: 'Reject proposed run' });
  await user.type(
    within(dialog).getByLabelText('Reason (optional)'),
    'Not this run.',
  );
  await user.click(
    within(dialog).getByRole('button', { name: 'Reject proposal' }),
  );
  expect(api.decideAssistantConfirmation).toHaveBeenCalledWith(
    'conversation-1',
    'confirmation-1',
    { decision: 'reject', reason: 'Not this run.' },
    expect.anything(),
  );
  expect(
    await screen.findByText(/Rejected\. The run was not submitted/),
  ).toBeTruthy();
});

it('shows a 409 replacement preview and requires a fresh explicit confirmation', async () => {
  const replacement = parseAssistantJson(
    previewText.replace('revision-1', 'revision-2').replace('2.0', '3.0'),
  );
  vi.spyOn(api, 'decideAssistantConfirmation').mockRejectedValue(
    new ApiProblemError({
      type: 'urn:genaimage:problem:confirmation-preview-changed',
      title: 'Conflict',
      status: 409,
      detail: 'The workflow changed.',
      recovery: 'Review the replacement preview.',
      preview: replacement,
      preview_token: 'replacement-token',
    }),
  );
  const user = userEvent.setup();
  render(ConfirmationCard, {
    confirmation: confirmation(),
    conversationId: 'conversation-1',
  });
  await user.click(screen.getByRole('button', { name: 'Confirm run' }));
  expect(
    (await screen.findAllByText('The workflow changed.')).length,
  ).toBeGreaterThan(0);
  expect(screen.getByText('revision-2')).toBeTruthy();
  expect(screen.getByText('3.0')).toBeTruthy();
  expect(api.decideAssistantConfirmation).toHaveBeenCalledTimes(1);
  const fresh = screen.getByRole('button', {
    name: 'Confirm replacement preview',
  });
  expect((fresh as HTMLButtonElement).disabled).toBe(false);
  await user.click(fresh);
  expect(api.decideAssistantConfirmation).toHaveBeenLastCalledWith(
    'conversation-1',
    'confirmation-1',
    { decision: 'confirm', preview_token: 'replacement-token' },
    expect.anything(),
  );
});

it('shows 422 issues and stays pending', async () => {
  vi.spyOn(api, 'decideAssistantConfirmation').mockRejectedValue(
    new ApiProblemError({
      type: 'about:blank',
      title: 'Arguments invalid',
      status: 422,
      detail: 'seed: must be positive.',
      recovery: 'Edit the arguments.',
      errors: [{ key: 'seed', reason: 'must be positive' }],
    }),
  );
  const user = userEvent.setup();
  render(ConfirmationCard, {
    confirmation: confirmation(),
    conversationId: 'conversation-1',
  });
  await user.click(screen.getByRole('button', { name: 'Confirm run' }));
  expect(
    (await screen.findAllByText('seed: must be positive.')).length,
  ).toBeGreaterThan(0);
  expect(screen.getByText('seed:')).toBeTruthy();
  expect(screen.getByText(/Not submitted/)).toBeTruthy();
  expect(
    (screen.getByRole('button', { name: 'Confirm run' }) as HTMLButtonElement)
      .disabled,
  ).toBe(false);
});

it.each([
  'invoking',
  'failed',
  'outcome_unknown',
  'rejected',
  'expired',
] as const)('renders persisted %s without false success', (status) => {
  render(ConfirmationCard, {
    confirmation: confirmation({ status, invocation_error: 'Backend reason.' }),
    conversationId: 'conversation-1',
  });
  expect(
    screen.queryByText('The backend confirmed the submitted run.'),
  ).toBeNull();
  expect(screen.getByText(status)).toBeTruthy();
});

it('disables Open in Generate and explains an oversized URL', () => {
  const oversized = confirmation({
    preview: parseAssistantJson(
      `{"workflow_id":"portrait","workflow_revision":"r1","parameters":{"text":"${'x'.repeat(33_000)}"},"issues":[]}`,
    ),
  });
  render(ConfirmationCard, {
    confirmation: oversized,
    conversationId: 'conversation-1',
  });
  expect(
    (
      screen.getByRole('button', {
        name: 'Open in Generate',
      }) as HTMLButtonElement
    ).disabled,
  ).toBe(true);
  expect(screen.getByText(/The share link is too long/)).toBeTruthy();
});

it('supports keyboard entry to the edit dialog and returns focus on close', async () => {
  const user = userEvent.setup();
  render(ConfirmationCard, {
    confirmation: confirmation(),
    conversationId: 'conversation-1',
  });
  const edit = screen.getByRole('button', { name: 'Edit' });
  edit.focus();
  await user.keyboard('{Enter}');
  const dialog = screen.getByRole('dialog', {
    name: 'Edit proposed run arguments',
  });
  expect(dialog).toBeTruthy();
  await waitFor(() =>
    expect(document.activeElement?.tagName.toLowerCase()).toBe('input'),
  );
  const close = within(dialog).getByRole('button', { name: 'Close' });
  close.focus();
  await user.keyboard('{Enter}');
  await waitFor(() => expect(dialog.hasAttribute('open')).toBe(false));
  expect(document.activeElement).toBe(edit);
});
