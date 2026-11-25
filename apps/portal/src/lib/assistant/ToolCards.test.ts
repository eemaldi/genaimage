import { render, screen } from '@testing-library/svelte';
import { expect, it } from 'vitest';
import { parseAssistantJson } from '../api/client';
import ToolCallCard from './ToolCallCard.svelte';
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
