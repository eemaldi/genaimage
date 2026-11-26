export const sections = [
  { path: '/', label: 'Dashboard', key: 'd' },
  { path: '/generate', label: 'Generate', key: 'g' },
  { path: '/jobs', label: 'Jobs', key: 'j' },
  { path: '/gallery', label: 'Gallery', key: 'a' },
  { path: '/workflows', label: 'Workflows', key: 'w' },
  { path: '/assistant', label: 'Assistant' },
  { path: '/models', label: 'Models' },
  { path: '/diagnostics', label: 'Diagnostics' },
  { path: '/settings', label: 'Settings', key: 's' },
  { path: '/setup', label: 'Setup' },
] as const;

export const shortcuts = [
  ...sections.flatMap(({ path, label, ...section }) =>
    'key' in section ? [{ keys: ['g', section.key], label, path }] : [],
  ),
  { keys: ['?'], label: 'Show keyboard shortcuts', path: null },
];

const typing =
  'input, textarea, select, [contenteditable]:not([contenteditable="false"]), [role="textbox"], [role="combobox"], [role="spinbutton"]';

export function createShortcutHandler(
  actions: { navigate: (path: string) => void; help: () => void },
  now = () => performance.now(),
) {
  let deadline = -Infinity;
  function reset() {
    deadline = -Infinity;
  }

  return {
    reset,
    handle(event: KeyboardEvent) {
      if (
        event.defaultPrevented ||
        event.isComposing ||
        event.altKey ||
        event.ctrlKey ||
        event.metaKey ||
        event
          .composedPath()
          .some((node) => node instanceof Element && node.matches(typing))
      ) {
        reset();
        return;
      }
      if (event.repeat) return;
      const time = now();
      const key = event.key.toLowerCase();
      const shortcut = shortcuts.find(({ keys }) =>
        keys.length === 1
          ? keys[0] === key
          : time < deadline && keys[1] === key,
      );
      reset();
      if (shortcut) {
        event.preventDefault();
        if (shortcut.path) actions.navigate(shortcut.path);
        else actions.help();
      } else if (key === 'g') {
        deadline = time + 1200;
      }
    },
  };
}
