import { cleanup, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';
import { ApiProblemError, type api } from '../api/client';
import type { components } from '../api/schema';
import { mockNativeDialog } from '../../test/dialog';
import TemplateAction from './TemplateAction.svelte';

mockNativeDialog();

const fake = vi.hoisted(() => ({
  listTemplates: vi.fn<typeof api.listTemplates>(),
  renderTemplate: vi.fn<typeof api.renderTemplate>(),
}));
vi.mock('../api/client', async (original) => ({
  ...(await original<typeof import('../api/client')>()),
  api: fake,
}));

type Template = components['schemas']['Template'];
const sha = (digit: string) => digit.repeat(64);
const portrait: Template = {
  ref: 'user:portrait-studio',
  scope: 'user',
  pack_id: null,
  id: 'portrait-studio',
  title: 'Studio portrait',
  description: null,
  variables: [
    { name: 'lighting', description: null, required: false, default: 'soft' },
    {
      name: 'subject',
      description: 'Who or what',
      required: true,
      default: null,
    },
  ],
  sha256: sha('a'),
  text: 'A {{subject}} photographed in a studio, {{lighting}} lighting',
};
const night: Template = {
  ref: 'pack:street:night.toml',
  scope: 'pack',
  pack_id: 'street',
  id: 'night-street',
  title: 'Night street',
  description: 'Neon reflections',
  variables: [
    { name: 'weather', description: null, required: false, default: 'dry' },
  ],
  sha256: sha('c'),
  text: 'A street at night, {{weather}} weather',
};
const listing = (...templates: Template[]) => ({
  templates,
  diagnostics: [
    {
      ref: 'user:broken',
      path: '/templates/broken.toml',
      error: 'text: broken',
    },
    { ref: 'user:odd', path: '/templates/odd.toml', error: 'id differs' },
  ],
});
const renderOk = (text: string) =>
  vi.fn(async (body: { ref: string; sha256: string }) => ({
    ref: body.ref,
    sha256: body.sha256,
    text,
  }));

function problem(
  type: string,
  status: number,
  extra: Record<string, unknown> = {},
) {
  return new ApiProblemError({
    type,
    status,
    title: 'Problem',
    detail: 'Detail',
    recovery: 'Recover.',
    ...extra,
  } as never);
}

let current = 'old text';
let inserted: string[];

beforeEach(() => {
  vi.resetAllMocks();
  current = 'old text';
  inserted = [];
  fake.listTemplates.mockResolvedValue(listing(portrait, night));
});
afterEach(() => {
  cleanup();
});

function mount() {
  render(TemplateAction, {
    name: 'Prompt',
    currentText: () => current,
    insert: (text: string) => inserted.push(text),
  });
  return userEvent.setup();
}

async function openPicker(user: ReturnType<typeof userEvent.setup>) {
  await user.click(
    screen.getByRole('button', { name: 'Use a template for Prompt' }),
  );
  await screen.findByRole('button', { name: 'Use “Studio portrait”' });
}

async function choose(user: ReturnType<typeof userEvent.setup>, title: string) {
  await user.click(screen.getByRole('button', { name: `Use “${title}”` }));
  return screen.findByRole('heading', { name: title, level: 3 });
}

const field = (name: string) =>
  screen.getByLabelText(name, { exact: true }) as HTMLTextAreaElement;
const previewButton = () => screen.getByRole('button', { name: 'Preview' });
const insertButton = () => screen.getByRole('button', { name: 'Insert' });
const renderedPre = () => document.querySelector<HTMLElement>('pre.rendered');

async function fillAndPreview(
  user: ReturnType<typeof userEvent.setup>,
  subject = 'cat',
) {
  await user.type(field('subject (required)'), subject);
  await user.click(previewButton());
}

async function previewed(
  user: ReturnType<typeof userEvent.setup>,
  subject = 'cat',
) {
  await fillAndPreview(user, subject);
  await waitFor(() => expect(renderedPre()).not.toBeNull());
}

test('lists templates with scope, text and summarised diagnostics', async () => {
  const user = mount();
  await openPicker(user);
  expect(screen.getByText('User template')).toBeTruthy();
  expect(screen.getByText('Pack street')).toBeTruthy();
  expect(screen.getByText('Neon reflections')).toBeTruthy();
  expect(screen.getByText(portrait.text, { exact: true })).toBeTruthy();
  expect(screen.getByText('2 templates could not be read')).toBeTruthy();
  await user.type(screen.getByLabelText('Filter templates'), 'night');
  expect(
    screen.queryByRole('button', { name: 'Use “Studio portrait”' }),
  ).toBeNull();
  expect(
    screen.getByRole('button', { name: 'Use “Night street”' }),
  ).toBeTruthy();
});

test('sends absent variables omitted, touched empty values as "", and defaults only when absent', async () => {
  fake.renderTemplate.mockImplementation(renderOk('rendered'));
  const user = mount();
  await openPicker(user);
  await choose(user, 'Studio portrait');
  await previewed(user);
  expect(JSON.stringify(fake.renderTemplate.mock.calls[0][0])).toBe(
    '{"ref":"user:portrait-studio","sha256":"' +
      sha('a') +
      '","variables":{"subject":"cat"}}',
  );

  await user.click(
    screen.getByRole('button', { name: 'Use an empty value for lighting' }),
  );
  await user.click(previewButton());
  await waitFor(() => expect(fake.renderTemplate).toHaveBeenCalledTimes(2));
  expect(JSON.stringify(fake.renderTemplate.mock.calls[1][0])).toBe(
    '{"ref":"user:portrait-studio","sha256":"' +
      sha('a') +
      '","variables":{"lighting":"","subject":"cat"}}',
  );

  await user.click(
    screen.getByRole('button', { name: 'Use the default for lighting' }),
  );
  await user.click(previewButton());
  await waitFor(() => expect(fake.renderTemplate).toHaveBeenCalledTimes(3));
  expect(fake.renderTemplate.mock.calls[2][0].variables).toEqual({
    subject: 'cat',
  });
});

test('shows the rendered text exactly, including whitespace', async () => {
  const text = '  A cat\n\n\tin soft light  \n';
  fake.renderTemplate.mockImplementation(renderOk(text));
  const user = mount();
  await openPicker(user);
  await choose(user, 'Studio portrait');
  await previewed(user);
  expect(renderedPre()?.textContent).toBe(text);
  expect(inserted).toEqual([]);
});

test('nothing is inserted without Insert, and the mode must be chosen', async () => {
  fake.renderTemplate.mockImplementation(renderOk('A cat'));
  const user = mount();
  await openPicker(user);
  await choose(user, 'Studio portrait');
  await previewed(user);
  expect(insertButton().hasAttribute('disabled')).toBe(true);
  await user.click(screen.getByRole('button', { name: 'Close' }));
  expect(inserted).toEqual([]);
  expect(current).toBe('old text');
});

test('inserts a replacement or an appended render only after the explicit choice', async () => {
  fake.renderTemplate.mockImplementation(renderOk('A cat'));
  const user = mount();
  await openPicker(user);
  await choose(user, 'Studio portrait');
  await previewed(user);
  await user.click(screen.getByLabelText('Replace the current text'));
  expect(insertButton().hasAttribute('disabled')).toBe(false);
  await user.click(insertButton());
  expect(inserted).toEqual(['A cat']);

  await openPicker(user);
  await choose(user, 'Studio portrait');
  await previewed(user);
  await user.click(screen.getByLabelText(/Append after the current text/));
  await user.click(insertButton());
  expect(inserted).toEqual(['A cat', 'old textA cat']);
});

test('a changed template reloads the listing and needs a new preview', async () => {
  fake.renderTemplate
    .mockRejectedValueOnce(
      problem('urn:genaimage:problem:template-changed', 409, {
        current_sha256: sha('b'),
      }),
    )
    .mockImplementation(renderOk('A cat, sharp'));
  const changed = {
    ...portrait,
    sha256: sha('b'),
    text: 'A {{subject}} photographed, sharp',
  };
  fake.listTemplates
    .mockResolvedValueOnce(listing(portrait, night))
    .mockResolvedValueOnce(listing(changed, night));
  const user = mount();
  await openPicker(user);
  await choose(user, 'Studio portrait');
  await fillAndPreview(user);
  await screen.findByText(/The template changed after you reviewed it/);
  expect(fake.renderTemplate).toHaveBeenCalledTimes(1);
  expect(fake.listTemplates).toHaveBeenCalledTimes(2);
  expect(document.querySelector('pre.raw')?.textContent).toBe(changed.text);
  expect(field('subject (required)').value).toBe('cat');
  expect(insertButton().hasAttribute('disabled')).toBe(true);
  expect(renderedPre()).toBeNull();

  await user.click(previewButton());
  await waitFor(() => expect(fake.renderTemplate).toHaveBeenCalledTimes(2));
  expect(fake.renderTemplate.mock.calls[1][0]).toEqual({
    ref: 'user:portrait-studio',
    sha256: sha('b'),
    variables: { subject: 'cat' },
  });
});

test('maps render validation errors to the variable fields', async () => {
  fake.renderTemplate.mockRejectedValueOnce(
    problem('urn:genaimage:problem:template-render-invalid', 422, {
      errors: [
        { key: 'variables.subject', reason: 'Enter a shorter subject.' },
        { key: '', reason: '1 more problem was omitted' },
      ],
    }),
  );
  const user = mount();
  await openPicker(user);
  await choose(user, 'Studio portrait');
  await fillAndPreview(user, 'cat');
  await waitFor(() =>
    expect(field('subject (required)').getAttribute('aria-invalid')).toBe(
      'true',
    ),
  );
  const describedBy =
    field('subject (required)').getAttribute('aria-describedby') ?? '';
  const described = describedBy
    .split(' ')
    .map((id) => document.getElementById(id)?.textContent)
    .join(' ');
  expect(described).toContain('Enter a shorter subject.');
  await screen.findByText('1 more problem was omitted');
  await waitFor(() =>
    expect(document.activeElement).toBe(field('subject (required)')),
  );
});

test('ignores a render that arrives after the template was switched', async () => {
  let finish: (value: {
    ref: string;
    sha256: string;
    text: string;
  }) => void = () => {};
  fake.renderTemplate.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const user = mount();
  await openPicker(user);
  await choose(user, 'Studio portrait');
  await user.type(field('subject (required)'), 'cat');
  await user.click(previewButton());
  await user.click(screen.getByRole('button', { name: 'Back to templates' }));
  await choose(user, 'Night street');
  finish({ ref: portrait.ref, sha256: portrait.sha256, text: 'LATE A' });
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(screen.queryByText('LATE A')).toBeNull();
  expect(renderedPre()).toBeNull();
  expect(insertButton().hasAttribute('disabled')).toBe(true);
});

test('a network failure is retryable', async () => {
  fake.renderTemplate
    .mockRejectedValueOnce(
      new Error('Could not complete the request to GenAImage.'),
    )
    .mockImplementation(renderOk('A cat'));
  const user = mount();
  await openPicker(user);
  await choose(user, 'Studio portrait');
  await fillAndPreview(user);
  expect(await screen.findByRole('alert')).toBeTruthy();
  expect(
    screen.getByText('Could not complete the request to GenAImage.'),
  ).toBeTruthy();
  await user.click(previewButton());
  await waitFor(() => expect(renderedPre()?.textContent).toBe('A cat'));
});

test('a listing failure offers a retry', async () => {
  fake.listTemplates.mockRejectedValueOnce(new Error('offline'));
  const user = mount();
  await user.click(
    screen.getByRole('button', { name: 'Use a template for Prompt' }),
  );
  expect(await screen.findByRole('alert')).toBeTruthy();
  await user.click(screen.getByRole('button', { name: 'Try again' }));
  await screen.findByRole('button', { name: 'Use “Studio portrait”' });
});

test('closing the picker unmounts it cleanly and returns focus to the action', async () => {
  const user = mount();
  await openPicker(user);
  const dialog = document.querySelector('dialog[open]') as HTMLDialogElement;
  const errors: unknown[] = [];
  const record = (event: ErrorEvent) => errors.push(event.error);
  window.addEventListener('error', record);
  await user.click(screen.getByRole('button', { name: 'Close' }));
  // A native dialog reports its close after the picker is gone.
  dialog.dispatchEvent(new Event('close'));
  window.removeEventListener('error', record);
  expect(errors).toEqual([]);
  const trigger = screen.getByRole('button', {
    name: 'Use a template for Prompt',
  });
  await waitFor(() => expect(document.activeElement).toBe(trigger));
  expect(document.querySelector('dialog[open]')).toBeNull();
});

test('focus follows the step: the fill heading after choosing, the list heading after going back', async () => {
  const user = mount();
  await openPicker(user);
  const fill = await choose(user, 'Studio portrait');
  await waitFor(() => expect(document.activeElement).toBe(fill));
  await user.click(screen.getByRole('button', { name: 'Back to templates' }));
  const list = await screen.findByRole('heading', {
    name: 'Choose a template',
  });
  await waitFor(() => expect(document.activeElement).toBe(list));
});
