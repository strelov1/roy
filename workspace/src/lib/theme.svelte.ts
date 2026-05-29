// Theme controller. Three modes — explicit `light` / `dark`, or `system`
// which tracks the OS preference. Persisted in localStorage under
// `roy.theme`. `main.ts` calls `initTheme()` once at boot; UI components
// read `themeStore.mode` and call `setMode(...)`.

import { LS, lsGet, lsSet } from './utils';

export type ThemeMode = 'light' | 'dark' | 'system';

const mq =
  typeof window !== 'undefined'
    ? window.matchMedia('(prefers-color-scheme: dark)')
    : null;

function readStored(): ThemeMode {
  const raw = lsGet(LS.theme);
  if (raw === 'light' || raw === 'dark' || raw === 'system') return raw;
  return 'system';
}

function apply(mode: ThemeMode) {
  const dark = mode === 'dark' || (mode === 'system' && (mq?.matches ?? false));
  document.documentElement.classList.toggle('dark', dark);
}

// Reactive state — Svelte 5 runes. Components import `themeStore` and
// read `themeStore.mode` to drive UI.
class ThemeStore {
  mode = $state<ThemeMode>(readStored());
  /** Tracks the OS `prefers-color-scheme: dark` preference; kept live by
   *  `initTheme`'s matchMedia listener. */
  systemDark = $state(mq?.matches ?? false);

  /** Effective dark state — explicit `dark`, or `system` resolving to the
   *  OS preference. */
  get isDark() {
    return this.mode === 'dark' || (this.mode === 'system' && this.systemDark);
  }

  setMode(next: ThemeMode) {
    this.mode = next;
    lsSet(LS.theme, next);
    apply(next);
  }

  /** Binary flip: light ↔ dark. `system` collapses to its current
   *  effective value, then flips — so the first click always yields a
   *  concrete (non-system) choice. */
  cycle() {
    this.setMode(this.isDark ? 'light' : 'dark');
  }
}

export const themeStore = new ThemeStore();

/** Wire OS-pref changes (only relevant when `mode === 'system'`). */
export function initTheme() {
  apply(themeStore.mode);
  const onMqChange = () => {
    themeStore.systemDark = mq?.matches ?? false;
    if (themeStore.mode === 'system') apply('system');
  };
  mq?.addEventListener('change', onMqChange);
  // Vite re-evaluates this module on every edit in dev. Without this
  // teardown, each HMR pass leaks a new listener — functionally
  // idempotent (they all write the same class) but real garbage.
  if (import.meta.hot) {
    import.meta.hot.dispose(() => mq?.removeEventListener('change', onMqChange));
  }
}
