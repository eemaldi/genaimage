import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { get } from 'svelte/store';
import { afterEach, expect, it, vi } from 'vitest';
import { mockNativeDialog } from '../../test/dialog';
import Button from './Button.svelte';
import Fixture from './Fixture.test.svelte';
import Input from './Input.svelte';
import ProblemDetails from './ProblemDetails.svelte';
import Select from './Select.svelte';
import Slider from './Slider.svelte';
import ToastRegion from './ToastRegion.svelte';
import { dismissToast, pushToast, toasts } from './toasts';

afterEach(() => {
  cleanup();
  for (const { id } of get(toasts)) dismissToast(id);
});
mockNativeDialog();

it('uses a named native button with Enter and Space activation', async () => {
  const user = userEvent.setup();
  const onclick = vi.fn();
  render(Button, { 'aria-label': 'Save', onclick });
  const button = screen.getByRole('button', { name: 'Save' });
  await user.tab();
  expect(document.activeElement).toBe(button);
  expect(button.getAttribute('type')).toBe('button');
  await user.keyboard('{Enter} ');
  expect(onclick).toHaveBeenCalledTimes(2);
});

it.each([false, true])(
  'labels input, links hint and error, and supports typing (multiline: %s)',
  async (multiline) => {
    const user = userEvent.setup();
    render(Input, {
      label: 'Prompt',
      multiline,
      hint: 'Describe the scene',
      error: 'Too short',
    });
    const input = screen.getByRole('textbox', {
      name: 'Prompt',
      description: 'Describe the scene Too short',
    });
    expect(input.getAttribute('aria-invalid')).toBe('true');
    await user.tab();
    expect(document.activeElement).toBe(input);
    await user.keyboard('First{Enter}Second');
    expect((input as HTMLInputElement).value).toBe(
      multiline ? 'First\nSecond' : 'FirstSecond',
    );
  },
);

it('exposes a focusable native select and associates its hint with selection', async () => {
  const user = userEvent.setup();
  render(Select, {
    label: 'Preset',
    value: 'draft',
    hint: 'Choose a preset',
    options: [
      { value: 'draft', label: 'Draft' },
      { value: 'final', label: 'Final' },
    ],
  });
  const select = screen.getByRole<HTMLSelectElement>('combobox', {
    name: 'Preset',
    description: 'Choose a preset',
  });
  await user.tab();
  expect(document.activeElement).toBe(select);
  expect(select.value).toBe('draft');
  await user.selectOptions(select, 'final');
  expect(select.selectedOptions[0].textContent).toBe('Final');
});

it('exposes a labelled native range, with keyboard focus and readable value changes', async () => {
  const user = userEvent.setup();
  render(Slider, {
    label: 'Steps',
    hint: 'Whole numbers',
    value: 4,
    min: 2,
    max: 8,
  });
  const slider = screen.getByRole<HTMLInputElement>('slider', {
    name: 'Steps',
    description: 'Whole numbers',
  });
  await user.tab();
  expect(document.activeElement).toBe(slider);
  expect([slider.min, slider.max, slider.step, slider.value]).toEqual([
    '2',
    '8',
    '1',
    '4',
  ]);
  // Simulate the input event emitted after native arrow-key adjustment.
  await fireEvent.input(slider, { target: { value: '5' } });
  expect(screen.getByText('5').getAttribute('aria-hidden')).toBe('true');
});

it('traps focus in a named modal, handles native Escape cancellation and restores focus', async () => {
  const user = userEvent.setup();
  render(Fixture, { kind: 'dialog' });
  const opener = screen.getByRole('button', { name: 'Open example' });
  await user.tab();
  await user.keyboard('{Enter}');
  const dialog = screen.getByRole('dialog', {
    name: 'Example',
    description: 'An example dialog',
  });
  expect(dialog.getAttribute('aria-modal')).toBe('true');
  const first = within(dialog).getByRole('button', { name: 'Close' });
  const last = within(dialog).getByRole('button', { name: 'Last action' });
  expect(document.activeElement).toBe(first);
  await user.tab({ shift: true });
  expect(document.activeElement).toBe(last);
  await user.tab();
  expect(document.activeElement).toBe(first);
  await fireEvent(dialog, new Event('cancel', { cancelable: true }));
  expect(screen.queryByRole('dialog')).toBeNull();
  expect(document.activeElement).toBe(opener);
});

it('leaves native video and audio control traversal to the browser', async () => {
  const user = userEvent.setup();
  render(Fixture, { kind: 'media' });
  await user.click(screen.getByRole('button', { name: 'Open example' }));
  const dialog = screen.getByRole('dialog');
  const close = within(dialog).getByRole('button', { name: 'Close' });
  const video = dialog.querySelector('video')!;
  const audio = dialog.querySelector('audio')!;
  // jsdom has no user-agent controls or focusable media hosts. Simulate the
  // browser's retargeted keydown at each host and assert it is not cancelled.
  for (const media of [video, audio]) {
    media.tabIndex = 0;
    media.focus();
    expect(document.activeElement).toBe(media);
    for (const shiftKey of [false, true]) {
      const event = new KeyboardEvent('keydown', {
        key: 'Tab',
        shiftKey,
        bubbles: true,
        cancelable: true,
      });
      await fireEvent(media, event);
      expect(event.defaultPrevented).toBe(false);
      expect(document.activeElement).toBe(media);
    }
  }
  close.focus();
  await user.tab({ shift: true });
  expect(document.activeElement).toBe(
    within(dialog).getByRole('button', { name: 'After media' }),
  );
  close.focus();
  const forward = new KeyboardEvent('keydown', {
    key: 'Tab',
    bubbles: true,
    cancelable: true,
  });
  await fireEvent(close, forward);
  expect(forward.defaultPrevented).toBe(false);
});

it.each([true, false])(
  'wraps Tab at a final radio group (checked: %s)',
  async (checkedRadio) => {
    const user = userEvent.setup();
    render(Fixture, { kind: 'radio', checkedRadio });
    await user.click(screen.getByRole('button', { name: 'Open example' }));
    const close = screen.getByRole('button', { name: 'Close' });
    const draft = screen.getByRole('radio', { name: 'Draft' });
    const final = screen.getByRole('radio', { name: 'Final' });
    expect(document.activeElement).toBe(close);
    await user.tab({ shift: true });
    expect(document.activeElement).toBe(draft);
    await user.tab();
    expect(document.activeElement).toBe(close);
    await user.click(final);
    await user.tab();
    expect(document.activeElement).toBe(close);
    await user.tab({ shift: true });
    expect(document.activeElement).toBe(final);
  },
);

it.each([undefined, -1])(
  'wraps Tab at an editing host (explicit tabindex: %s)',
  async (editorTabIndex) => {
    const user = userEvent.setup();
    render(Fixture, { kind: 'editor', editorTabIndex });
    await user.click(screen.getByRole('button', { name: 'Open example' }));
    const close = screen.getByRole('button', { name: 'Close' });
    const editor = screen.getByRole('textbox', { name: 'Prompt editor' });
    expect(document.activeElement).toBe(close);
    await user.tab();
    expect(document.activeElement).toBe(
      editorTabIndex === undefined ? editor : close,
    );
    await user.tab();
    expect(document.activeElement).toBe(close);
    await user.tab({ shift: true });
    expect(document.activeElement).toBe(
      editorTabIndex === undefined ? editor : close,
    );
  },
);

it('automatically activates tabs with roving focus, wrapping and Home/End', async () => {
  const user = userEvent.setup();
  render(Fixture, { kind: 'tabs' });
  const controls = screen.getByRole('tablist', { name: 'Options' });
  const [first, middle, last] = within(controls).getAllByRole('tab');
  await user.tab();
  expect(document.activeElement).toBe(first);
  await user.keyboard('{ArrowLeft}');
  expect(document.activeElement).toBe(last);
  expect(last.getAttribute('aria-selected')).toBe('true');
  expect(first.tabIndex).toBe(-1);
  expect(last.tabIndex).toBe(0);
  expect(screen.getByRole('tabpanel', { name: 'Three' }).id).toBe(
    last.getAttribute('aria-controls'),
  );
  await user.keyboard('{Home}{ArrowRight}');
  expect(document.activeElement).toBe(middle);
  await user.keyboard('{End}{ArrowRight}');
  expect(document.activeElement).toBe(first);
});

it('describes a tooltip trigger, opens on focus and hover, and dismisses without moving focus', async () => {
  const user = userEvent.setup();
  render(Fixture, { kind: 'tooltip' });
  const button = screen.getByRole('button', {
    name: 'Information',
    description: 'Extra context',
  });
  expect(screen.queryByRole('tooltip')).toBeNull();
  await user.tab();
  expect(screen.getByRole('tooltip').textContent?.trim()).toBe('Extra context');
  await user.keyboard('{Escape}');
  expect(screen.queryByRole('tooltip')).toBeNull();
  expect(document.activeElement).toBe(button);
  await user.tab();
  const wrapper = button.parentElement!;
  await fireEvent.pointerEnter(wrapper, { pointerType: 'mouse' });
  expect(screen.getByRole('tooltip')).toBeTruthy();
  await fireEvent.pointerLeave(wrapper, { pointerType: 'mouse' });
  expect(screen.queryByRole('tooltip')).toBeNull();
});

it('shifts a focused tooltip back inside a 320px viewport at either edge', async () => {
  const user = userEvent.setup();
  const viewport = vi
    .spyOn(document.documentElement, 'clientWidth', 'get')
    .mockReturnValue(320);
  try {
    render(Fixture, { kind: 'tooltip-edges' });
    for (const [name, left, expectedLeft] of [
      ['Left', -5, 0],
      ['Right', 280, 110],
    ] as const) {
      const trigger = screen.getByRole('button', { name });
      const bubble = document.getElementById(
        trigger.getAttribute('aria-describedby')!,
      )!;
      const width = 210;
      vi.spyOn(trigger.parentElement!, 'getBoundingClientRect').mockReturnValue(
        new DOMRect(left, 0, 40, 40),
      );
      vi.spyOn(bubble, 'getBoundingClientRect').mockReturnValue(
        new DOMRect(left, 40, width, 40),
      );
      await user.tab();
      expect(document.activeElement).toBe(trigger);
      expect(screen.getByRole('tooltip')).toBe(bubble);
      const positionedLeft =
        left + parseFloat(bubble.style.getPropertyValue('--tooltip-shift'));
      expect(positionedLeft).toBe(expectedLeft);
      expect(positionedLeft + width).toBeLessThanOrEqual(320);
    }
  } finally {
    viewport.mockRestore();
  }
});

it('announces toasts without stealing focus and supports keyboard dismissal', async () => {
  const user = userEvent.setup();
  const returnFocus = vi.fn();
  render(ToastRegion, { returnFocus });
  pushToast({ message: 'Saved' });
  pushToast({ message: 'Could not connect', kind: 'error' });
  const status = screen.getByRole('status');
  const alert = screen.getByRole('alert');
  await screen.findByRole('button', { name: 'Dismiss notification: Saved' });
  expect(status.textContent?.trim()).toBe('Saved');
  expect(alert.textContent?.trim()).toBe('Could not connect');
  expect(document.activeElement).toBe(document.body);
  await user.tab();
  await user.keyboard('{Enter}');
  expect(status.textContent?.trim()).toBe('');
  expect(document.activeElement).toBe(
    screen.getByRole('button', {
      name: 'Dismiss notification: Could not connect',
    }),
  );
  await user.keyboard(' ');
  expect(alert.textContent?.trim()).toBe('');
  expect(returnFocus).toHaveBeenCalledOnce();
});

it('renders problem details, the recovery instruction and field errors as text', () => {
  render(ProblemDetails, {
    problem: {
      title: 'Invalid settings',
      detail: '<b>Change rejected</b>',
      recovery: 'Check the values below.',
      type: 'about:blank',
      status: 400,
      instance: '/requests/1',
      errors: [{ key: 'port', reason: 'Out of range' }],
    },
  });
  const problem = within(screen.getByRole('alert'));
  expect(problem.getByText('Invalid settings')).toBeTruthy();
  expect(problem.getByText('<b>Change rejected</b>')).toBeTruthy();
  expect(problem.getByText(/Check the values below/)).toBeTruthy();
  expect(problem.getByText('What to do next:')).toBeTruthy();
  expect(problem.getByRole('listitem').textContent?.trim()).toBe(
    'port: Out of range',
  );
});
