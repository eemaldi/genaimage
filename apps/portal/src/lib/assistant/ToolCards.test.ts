import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach } from 'vitest';
import { expect, it } from 'vitest';

afterEach(cleanup);
import { parseAssistantJson } from '../api/client';
import ToolCallCard from './ToolCallCard.svelte';
import { messageItems } from './model';
import ToolResultCard from './ToolResultCard.svelte';

it('renders tool argument markup as exact text', () => {
  const args = parseAssistantJson(
    '{"prompt":"<img src=x onerror=alert(1)>","seed":900719925474099312345,"strength":2.0,"offset":-0.0}',
  );
  const { container } = render(ToolCallCard, {
    toolCall: { id: 'tool-1', name: 'submit_run', arguments: args },
    conversationId: 'conversation-1',
  });
  expect(container.querySelector('img')).toBeNull();
  expect(container.textContent).toContain('<img src=x onerror=alert(1)>');
  expect(container.textContent).toContain('900719925474099312345');
  expect(container.textContent).toContain('2.0');
  expect(container.textContent).toContain('-0.0');
});

it('shows truncation, run links and proposal Generate links without submitting', () => {
  render(ToolResultCard, {
    tool: 'propose_run',
    ok: true,
    conversationId: 'conversation-1',
    toolCallId: 'tool-1',
    result: parseAssistantJson(
      '{"workflow_id":"portrait","workflow_revision":"r1","preset":null,"parameters":{"seed":900719925474099312345},"issues":[],"truncated":true,"truncation_notes":["Large fields were shortened."]}',
    ),
  });
  expect(
    screen.getAllByText(/Large fields were shortened/).length,
  ).toBeGreaterThan(0);
  expect(
    screen.getByRole('link', { name: 'Open in Generate' }).getAttribute('href'),
  ).toContain('wr=r1');

  const second = render(ToolResultCard, {
    tool: 'submit_run',
    ok: true,
    conversationId: 'conversation-1',
    toolCallId: 'tool-2',
    result: { run_id: 'run/a', status: 'queued' },
  });
  expect(
    second.getByRole('link', { name: 'Open run run/a' }).getAttribute('href'),
  ).toBe('#/runs/run%2Fa');
});

it.each([
  ['failed', false, { ok: false, error: 'Tool failed.' }],
  ['rejected', false, { ok: false, error: 'The user rejected the tool.' }],
  ['unknown', undefined, { result: {} }],
] as const)(
  'persisted %s outcomes never render as succeeded',
  (_label, _ok, wrapper) => {
    const item = messageItems(
      {
        id: 'conversation-1',
        title: 'Conversation',
        model: 'local',
        system_prompt: '',
        system_prompt_sha256: 'hash',
        system_prompt_template: 'assistant',
        created_at: '2026-01-01T00:00:00Z',
        updated_at: '2026-01-01T00:00:00Z',
        messages: [
          {
            id: 'tool-message',
            conversation_id: 'conversation-1',
            seq: 2,
            role: 'tool',
            status: 'complete',
            created_at: '2026-01-01T00:00:00Z',
            content: { tool: { call_id: 'tool-1', result: wrapper } },
          },
        ],
      },
      [
        {
          id: 1,
          conversation_id: 'conversation-1',
          tool_call_id: 'tool-1',
          tool: 'submit_run',
          proposed_arguments: {},
          validation_outcome: 'valid',
          decision: 'not_invoked',
          created_at: '2026-01-01T00:00:00Z',
        },
      ],
    ).at(-1);
    expect(item?.kind).toBe('tool_result');
    if (item?.kind !== 'tool_result')
      throw new Error('Expected a tool result.');
    const rendered = render(ToolResultCard, {
      tool: item.tool,
      ok: item.ok,
      conversationId: item.conversationId,
      toolCallId: item.toolCallId,
      result: item.result,
      error: item.error,
    });
    expect(rendered.queryByText('Succeeded')).toBeNull();
    expect(
      rendered.getByText(_ok === undefined ? 'Unknown outcome' : 'Failed'),
    ).toBeTruthy();
  },
);

it('links a persisted submit_run success to its unwrapped run result', () => {
  const item = messageItems(
    {
      id: 'conversation-1',
      title: 'Conversation',
      model: 'local',
      system_prompt: '',
      system_prompt_sha256: 'hash',
      system_prompt_template: 'assistant',
      created_at: '2026-01-01T00:00:00Z',
      updated_at: '2026-01-01T00:00:00Z',
      messages: [
        {
          id: 'tool-message',
          conversation_id: 'conversation-1',
          seq: 2,
          role: 'tool',
          status: 'complete',
          created_at: '2026-01-01T00:00:00Z',
          content: {
            tool: {
              call_id: 'tool-1',
              result: {
                ok: true,
                result: { run_id: 'persisted-run', status: 'queued' },
              },
            },
          },
        },
      ],
    },
    [
      {
        id: 1,
        conversation_id: 'conversation-1',
        tool_call_id: 'tool-1',
        tool: 'submit_run',
        proposed_arguments: {},
        validation_outcome: 'valid',
        decision: 'invocation_succeeded',
        invocation_ok: true,
        invocation_result: { run_id: 'persisted-run', status: 'queued' },
        created_at: '2026-01-01T00:00:00Z',
      },
    ],
  ).at(-1);
  if (item?.kind !== 'tool_result') throw new Error('Expected a tool result.');
  render(ToolResultCard, {
    tool: item.tool,
    ok: item.ok,
    conversationId: item.conversationId,
    toolCallId: item.toolCallId,
    result: item.result,
    error: item.error,
  });
  expect(
    screen
      .getByRole('link', { name: 'Open run persisted-run' })
      .getAttribute('href'),
  ).toBe('#/runs/persisted-run');
});
