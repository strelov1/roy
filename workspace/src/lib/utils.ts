import { clsx, type ClassValue } from 'clsx';
import { twMerge } from 'tailwind-merge';

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

export type WithoutChild<T> = T extends { child?: unknown } ? Omit<T, 'child'> : T;
export type WithoutChildren<T> = T extends { children?: unknown } ? Omit<T, 'children'> : T;
export type WithoutChildrenOrChild<T> = WithoutChildren<WithoutChild<T>>;
export type WithElementRef<T, U extends HTMLElement = HTMLElement> = T & { ref?: U | null };

/** localStorage.getItem, safe across SSR / private mode / quota. */
export function lsGet(key: string): string | null {
  if (typeof localStorage === 'undefined') return null;
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

/** localStorage.setItem, swallowing SSR / quota / private-mode failures. */
export function lsSet(key: string, value: string): void {
  if (typeof localStorage === 'undefined') return;
  try {
    localStorage.setItem(key, value);
  } catch {
    // best-effort
  }
}

/** Read+parse JSON from localStorage. Returns `fallback` on missing / invalid. */
export function lsGetJSON<T>(key: string, fallback: T): T {
  const raw = lsGet(key);
  if (raw === null) return fallback;
  try {
    return JSON.parse(raw) as T;
  } catch {
    return fallback;
  }
}

/** Stringify+write JSON to localStorage. */
export function lsSetJSON(key: string, value: unknown): void {
  try {
    lsSet(key, JSON.stringify(value));
  } catch {
    // serialization failure — drop
  }
}

// INTERACTIVE_SEL lives here too (used by Composer + ChatView): a click on
// the composer card focuses the textarea, except when it lands on something
// the user is actually targeting (a button, the picker, etc.).
export const INTERACTIVE_SEL =
  'button, textarea, input, select, a, [role="button"], [contenteditable]';

/** Splice `insert` into `value` at the textarea's current caret. Returns
 *  the new value and the caret position after the insertion. Shared by
 *  Composer + ChatView so attach/skill insertions behave identically. */
export function spliceAtCaret(
  el: HTMLTextAreaElement,
  value: string,
  insert: string,
): { next: string; caret: number } {
  const caret = el.selectionStart ?? value.length;
  return {
    next: value.slice(0, caret) + insert + value.slice(caret),
    caret: caret + insert.length,
  };
}

/** Focus the textarea and park the caret at `pos` on the next microtask —
 *  Svelte's reactive write to `bind:value` happens between frames, so the
 *  selection set must wait until the DOM has the new text. */
export function focusCaret(el: HTMLTextAreaElement, pos: number): void {
  queueMicrotask(() => {
    el.focus();
    el.setSelectionRange(pos, pos);
  });
}

/** Auto-grow a textarea to fit its content: reset height so scrollHeight
 *  reflects the true content size, then clamp to `cap` px (uncapped when
 *  omitted). Shared by Composer / ChatView / QueuePanel so their grow
 *  behavior stays identical. */
export function autosize(el: HTMLTextAreaElement, cap = Infinity): void {
  el.style.height = 'auto';
  el.style.height = `${Math.min(el.scrollHeight, cap)}px`;
}

/** Length-cap with trailing ellipsis. */
export function truncate(text: string, max: number, ellipsis = '…'): string {
  return text.length > max ? text.slice(0, max).trimEnd() + ellipsis : text;
}

/** Safely coerce a caught value to a message string. Avoids the literal
 *  "undefined" that `(e as Error).message` renders when a non-Error is thrown. */
export function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** Squash whitespace + truncate for sidebar/header titles. Single source of
 *  truth so `state.titleFor` and the spawning placeholder don't drift. */
export function formatTitle(text: string, max = 48): string {
  return truncate(text.replace(/\s+/g, ' ').trim(), max);
}

/** All localStorage keys used by the app. Centralized so they don't drift
 *  apart (note: legacy `roy:` colon-prefixed keys are kept as-is to avoid
 *  invalidating users' stored state — don't normalize them). */
export const LS = {
  sidebarOpen: 'roy.sidebar.open',
  sidebarTab: 'roy.sidebar.tab',
  theme: 'roy.theme',
  recentCwds: 'roy.recentCwds',
  expandedProjects: 'roy:expanded_projects',
  lastProjectId: 'roy:last_project_id',
  /** Last-used composer scope. Empty / missing = personal; otherwise the
   *  team_id. Cleared automatically when membership is revoked. */
  lastScope: 'roy:last_scope',
  /** Pinned picker harnesses and (agent, model) pairs.
   *  See picker-favorites.svelte.ts for the schema. */
  pickerFavorites: 'roy:picker:favorites',
} as const;
