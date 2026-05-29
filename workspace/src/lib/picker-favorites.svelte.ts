// Reactive pin-list for the model picker. Two pinned collections:
//
//   - harnesses: Harness[]                — pinned harness rails
//   - models:    Array<{ agent, model }>  — pinned concrete (harness, model) pairs
//
// Both insertion-ordered, newest first. Persists to localStorage on every
// mutation; reads tolerate schema drift (drops unknown harnesses, keeps the
// rest). No server sync — favorites are a per-browser convenience.

import type { Harness } from './wire';
import { KNOWN_HARNESSES } from './wire';
import { LS, lsGetJSON, lsSetJSON } from './utils';

// `agent` here is legacy from the pre-rename schema; the value is the
// harness identifier, not a persona id. Kept named `agent` so the
// localStorage payload doesn't churn for already-pinned models.
export type FavoriteModel = { agent: Harness; model: string };

type Persisted = {
  harnesses: Harness[];
  models: FavoriteModel[];
};

function loadInitial(): Persisted {
  const raw = lsGetJSON<unknown>(LS.pickerFavorites, { harnesses: [], models: [] });
  // Defensive parse — anything that fails the shape check is dropped, but
  // valid neighbours survive. This is what keeps the store working after a
  // future schema bump or a hand-edited LS value.
  //
  // TODO(remove-after 2026-09): the pre-rename schema used `engines` for the
  // pinned-harness list; we read it as a fallback so users who pinned before
  // the preset→harness rename don't see their pins disappear. Drop the
  // `obj.engines` branch once enough time has passed that all live browsers
  // have re-saved at least once.
  if (!raw || typeof raw !== 'object') return { harnesses: [], models: [] };
  const obj = raw as Record<string, unknown>;
  const sourceList = Array.isArray(obj.harnesses)
    ? obj.harnesses
    : Array.isArray(obj.engines)
      ? obj.engines
      : [];
  const harnesses = sourceList.filter(
    (e): e is Harness => typeof e === 'string' && KNOWN_HARNESSES.has(e as Harness),
  );
  const models = Array.isArray(obj.models)
    ? obj.models.filter((m): m is FavoriteModel => {
        if (!m || typeof m !== 'object') return false;
        const v = m as Record<string, unknown>;
        return (
          typeof v.agent === 'string' &&
          KNOWN_HARNESSES.has(v.agent as Harness) &&
          typeof v.model === 'string'
        );
      })
    : [];
  return { harnesses, models };
}

class PickerFavorites {
  harnesses = $state<Harness[]>([]);
  models = $state<FavoriteModel[]>([]);

  constructor() {
    const initial = loadInitial();
    this.harnesses = initial.harnesses;
    this.models = initial.models;
  }

  hasHarness(a: Harness): boolean {
    return this.harnesses.includes(a);
  }

  hasModel(a: Harness, m: string): boolean {
    return this.models.some((x) => x.agent === a && x.model === m);
  }

  toggleHarness(a: Harness): void {
    this.harnesses = this.hasHarness(a)
      ? this.harnesses.filter((e) => e !== a)
      : [a, ...this.harnesses];
    this.persist();
  }

  toggleModel(a: Harness, m: string): void {
    this.models = this.hasModel(a, m)
      ? this.models.filter((x) => !(x.agent === a && x.model === m))
      : [{ agent: a, model: m }, ...this.models];
    this.persist();
  }

  private persist(): void {
    lsSetJSON(LS.pickerFavorites, { harnesses: this.harnesses, models: this.models });
  }
}

export const pickerFavorites = new PickerFavorites();
