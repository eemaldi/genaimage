import { writable } from 'svelte/store';

export type ToastInput = { message: string; kind?: 'info' | 'error' };
export type Toast = { id: number; message: string; kind: 'info' | 'error' };

const state = writable<readonly Toast[]>([]);
let nextId = 0;

export const toasts = { subscribe: state.subscribe };

export function pushToast({ message, kind = 'info' }: ToastInput): number {
  const id = ++nextId;
  state.update((items) => [...items, { id, message, kind }]);
  return id;
}

export function dismissToast(id: number): void {
  state.update((items) => items.filter((item) => item.id !== id));
}
