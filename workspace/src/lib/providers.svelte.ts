// Read-only providers catalog. Loaded once per session; load(true) forces a reload.
// Mirrors the agentsStore / connectionsStore shape.

import { providers as api, type Provider } from './management-client';
import { LoadableStore } from './list-store.svelte';

class ProvidersState extends LoadableStore<Provider> {
  async load(force = false): Promise<void> {
    await this.run(() => api.list(), force);
  }

  get(id: string): Provider | undefined {
    return this.list.find((p) => p.id === id);
  }
}

export const providersStore = new ProvidersState();
export type { Provider } from './management-client';
