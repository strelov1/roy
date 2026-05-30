// src/lib/agents.svelte.ts
//
// File-based agents store. Reads /management/agents (a server-side scan
// of ~/.roy/agents/*.md). Client filters out entries whose `harness`
// isn't in the Harness union — corrupted files don't crash render,
// they just don't appear in the catalog (with a console warning).

import type { Harness } from './wire';
import { KNOWN_HARNESSES } from './wire';
import { agents as api } from './management-client';
import { LoadableStore } from './list-store.svelte';

export type AgentScope =
  | { kind: 'builtin' }
  | { kind: 'personal' }
  | { kind: 'team'; team_id: string };

/** Runtime allowlist of scope kinds. Mirrors KNOWN_HARNESSES — narrows a
 *  server-returned `scope.kind` of unknown origin into the AgentScope union. */
const KNOWN_SCOPES: ReadonlySet<string> = new Set<AgentScope['kind']>([
  'builtin',
  'personal',
  'team',
]);

export type Agent = {
  /** File stem (`<slug>.md`) — stable id used by channel bindings. */
  slug: string;
  name: string;
  description: string;
  harness: Harness;
  model?: string;
  body: string;
  scope: AgentScope;
};

class AgentsState extends LoadableStore<Agent> {
  async load(force = false): Promise<void> {
    await this.run(async () => {
      const raw = await api.list();
      return raw
        .filter((a) => {
          if (!KNOWN_HARNESSES.has(a.harness as Harness)) {
            // eslint-disable-next-line no-console
            console.warn(`agent ${a.name}: unknown harness "${a.harness}", skipping`);
            return false;
          }
          if (!KNOWN_SCOPES.has(a.scope?.kind)) {
            // eslint-disable-next-line no-console
            console.warn(`agent ${a.name}: unknown scope kind, skipping`);
            return false;
          }
          return true;
        })
        .map((a) => ({
          slug: a.slug,
          name: a.name,
          description: a.description,
          harness: a.harness as Harness,
          model: a.model ?? undefined,
          body: a.body,
          scope: a.scope as AgentScope,
        }));
    }, force);
  }
}

export const agentsStore = new AgentsState();

function scopeRank(s: AgentScope): number {
  if (s.kind === 'builtin') return 0;
  if (s.kind === 'personal') return 1;
  return 2;
}

export function sortAgents(list: Agent[]): Agent[] {
  return [...list].sort((a, b) => {
    const r = scopeRank(a.scope) - scopeRank(b.scope);
    if (r !== 0) return r;
    return a.name.localeCompare(b.name);
  });
}
