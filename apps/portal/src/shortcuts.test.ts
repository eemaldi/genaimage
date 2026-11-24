import { describe, expect, it, vi } from 'vitest';
import { createShortcutHandler, shortcuts } from './shortcuts';

function setup() {
  let now = 0;
  const actions = { navigate: vi.fn(), help: vi.fn() };
  const handler = createShortcutHandler(actions, () => now);
  function press(
    key: string,
    options: KeyboardEventInit = {},
    target: EventTarget = window,
  ) {
    const event = new KeyboardEvent('keydown', {
      key,
      bubbles: true,
      cancelable: true,
      ...options,
    });
    target.addEventListener(
      'keydown',
      (event) => handler.handle(event as KeyboardEvent),
      { once: true },
    );
    target.dispatchEvent(event);
    return event.defaultPrevented;
  }
  return {
    ...actions,
    press,
    reset: handler.reset,
    expire: () => (now += 1200),
  };
}

describe('character shortcut registry', () => {
  it.each(shortcuts.filter(({ path }) => path !== null))(
    'navigates to $label only after its full chord',
    ({ keys, path }) => {
      const { press, navigate } = setup();
      expect(press(keys[0])).toBe(false);
      expect(navigate).not.toHaveBeenCalled();
      expect(press(keys[1])).toBe(true);
      expect(navigate).toHaveBeenCalledExactlyOnceWith(path);
    },
  );

  it('opens help with a shifted question mark and ignores auto-repeat', () => {
    const { press, help } = setup();
    expect(press('?', { shiftKey: true, repeat: true })).toBe(false);
    expect(press('?', { shiftKey: true })).toBe(true);
    expect(help).toHaveBeenCalledOnce();
  });

  it('expires prefixes and cancels them on noise, reset, composition and modifiers', () => {
    const { press, navigate, expire, reset } = setup();
    press('g');
    expire();
    press('d');
    press('g');
    press('x');
    press('d');
    press('g');
    reset();
    press('d');
    press('g');
    press('d', { isComposing: true });
    press('d');
    press('g');
    press('d', { ctrlKey: true });
    press('d');
    expect(navigate).not.toHaveBeenCalled();
  });

  it.each([
    '<input>',
    '<textarea></textarea>',
    '<select></select>',
    '<div contenteditable><span></span></div>',
    '<div role="textbox"></div>',
  ])('ignores keys in %s and clears a pending chord', (html) => {
    const { press, navigate, help } = setup();
    const root = document.createElement('div');
    root.innerHTML = html;
    const target = root.querySelector('span') ?? root.firstElementChild!;
    press('g');
    expect(press('?', {}, target)).toBe(false);
    press('d');
    expect(help).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
  });
});
