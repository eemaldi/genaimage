const marker = '__genaimageNavigationIndex';
let index = 0;
let restoring = false;
let currentUrl = '';
let currentState: unknown;
let guard: (() => boolean) | undefined;

function stateIndex(): number | undefined {
  const state: unknown = history.state;
  if (state && typeof state === 'object' && marker in state) {
    const value = (state as Record<string, unknown>)[marker];
    if (typeof value === 'number') return value;
  }
}
function mark(value: number) {
  const state = history.state;
  history.replaceState(
    { ...(state && typeof state === 'object' ? state : {}), [marker]: value },
    '',
  );
}
export function initialiseNavigation() {
  index = stateIndex() ?? 0;
  mark(index);
  restoring = false;
  currentUrl = location.href;
  currentState = history.state;
}
export function setNavigationGuard(next?: () => boolean) {
  guard = next;
}
export function acceptNavigation(): boolean {
  if (restoring) {
    restoring = false;
    return false;
  }
  const existing = stateIndex();
  const next = existing ?? index + 1;
  if (guard && !guard()) {
    if (existing === undefined) {
      history.pushState(currentState, '', currentUrl);
    } else {
      restoring = true;
      history.go(index - next || -1);
    }
    return false;
  }
  if (existing === undefined) mark(next);
  index = next;
  currentUrl = location.href;
  currentState = history.state;
  return true;
}
