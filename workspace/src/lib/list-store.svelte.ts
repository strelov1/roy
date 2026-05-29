// Shared base for the lazy-load catalog stores (providers / connections /
// commands / agents). They all expose the same list/loading/loaded/error
// runes and the same load(force) guard+try/catch/finally lifecycle, so it
// lives here once. Concrete stores extend this and add their own methods.

import { errMsg } from './utils';

export class LoadableStore<T> {
  list = $state<T[]>([]);
  loading = $state(false);
  loaded = $state(false);
  error = $state<string | null>(null);

  /// Run the fetcher with the standard guard/try/catch/finally. Skips when
  /// already loaded or in-flight unless `force` is set.
  protected async run(fetcher: () => Promise<T[]>, force = false): Promise<void> {
    if ((this.loaded || this.loading) && !force) return;
    this.loading = true;
    this.error = null;
    try {
      this.list = await fetcher();
      this.loaded = true;
    } catch (e) {
      this.error = errMsg(e);
    } finally {
      this.loading = false;
    }
  }
}
