// src/lib/harnesses-config.svelte.ts
//
// Reactive store mirroring the daemon's HarnessesList response (the
// harness+model catalog from ~/.config/roy/harnesses.toml). The word
// "agent" in roy-web is reserved for user-defined personas served by
// roy-management; "harness" is one of the ACP-adapter binaries
// (claude/gemini/opencode/codex/pi).

import type { Harness, HarnessInfo, HarnessesConfigStatus, ModelInfo } from './wire';
import { royClient } from './client';
import { errMsg } from './utils';

class HarnessesConfigState {
  harnesses = $state<HarnessInfo[]>([]);
  configPath = $state('');
  status = $state<HarnessesConfigStatus>({ kind: 'ok' });
  loading = $state(false);

  async refresh(): Promise<void> {
    if (this.loading) return;
    this.loading = true;
    try {
      const ev = await royClient.call({ op: 'list_harnesses' }, 'harnesses_list');
      this.harnesses = ev.harnesses;
      this.configPath = ev.config_path;
      this.status = ev.status;
    } catch (e) {
      this.status = {
        kind: 'invalid',
        reason: errMsg(e),
      };
    } finally {
      this.loading = false;
    }
  }
}

export const harnessesConfig = new HarnessesConfigState();

/** Pick the default model for a harness: the one with `default: true`, or
 *  the first model in the catalog entry. Returns `undefined` if the
 *  harness isn't in the catalog at all. */
export function defaultModelFor(
  catalog: HarnessInfo[],
  harness: Harness,
): ModelInfo | undefined {
  const entry = catalog.find((h) => h.name === harness);
  if (!entry) return undefined;
  return entry.models.find((m) => m.default) ?? entry.models[0];
}
