const marker = '__genaimageNavigationIndex';
type Position = { chain: string; index: number };
let position: Position | undefined;
let restoring = false;
let navigating = false;
let currentUrl = '';
let guard: (() => boolean) | undefined;

function statePosition(): Position | undefined {
  const state: unknown = history.state;
  if (!state || typeof state !== 'object' || !(marker in state)) return;
  const value = (state as Record<string, unknown>)[marker];
  if (typeof value === 'number' && Number.isSafeInteger(value))
    return { chain: 'legacy', index: value };
  if (
    value &&
    typeof value === 'object' &&
    'chain' in value &&
    'index' in value &&
    typeof value.chain === 'string' &&
    typeof value.index === 'number' &&
    Number.isSafeInteger(value.index)
  )
    return { chain: value.chain, index: value.index };
}
function stateWithPosition(state: unknown, next: Position) {
  return {
    ...(state && typeof state === 'object' ? state : {}),
    [marker]: next,
  };
}
function newChain(): string {
  return `${Date.now()}-${Math.random()}`;
}
export function initialiseNavigation() {
  position = statePosition() ?? { chain: newChain(), index: 0 };
  history.replaceState(stateWithPosition(history.state, position), '');
  restoring = false;
  currentUrl = location.href;
}
export function setNavigationGuard(next?: () => boolean) {
  guard = next;
}
export function navigateHash(hash: string) {
  if (restoring || location.hash === hash || (guard && !guard())) return;
  const oldURL = location.href;
  position = position
    ? { ...position, index: position.index + 1 }
    : { chain: newChain(), index: 0 };
  history.pushState(stateWithPosition(history.state, position), '', hash);
  currentUrl = location.href;
  navigating = true;
  try {
    window.dispatchEvent(
      new HashChangeEvent('hashchange', { oldURL, newURL: currentUrl }),
    );
  } finally {
    navigating = false;
  }
}
export function acceptNavigation(): boolean {
  if (navigating) return true;
  if (restoring) {
    restoring = false;
    return false;
  }
  const next = statePosition();
  if (guard && !guard()) {
    if (
      position &&
      next &&
      position.chain === next.chain &&
      position.index !== next.index
    ) {
      restoring = true;
      history.go(position.index - next.index);
    } else {
      // Unknown direction cannot be traversed safely: replace this one entry, preserving forward history.
      history.replaceState(history.state, '', currentUrl);
      position = undefined;
    }
    return false;
  }
  position = next;
  currentUrl = location.href;
  return true;
}
