# Model Picker: Favorites + Agents Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `★ Favorites` and `⌗ Agents` sections to the top of the `ModelPicker` left rail (Composer only), so users can pin (agent, model) pairs and engines, and can launch saved Agent personas from the composer with their typed draft preserved.

**Architecture:** New `picker-favorites.svelte.ts` store backed by `localStorage`. `ModelPicker.svelte` replaces `railAgent: AgentPreset` with a tagged-union `railView` so the rail icon and right-panel render share a single source of truth. `Composer.svelte` gains a `selectedAgentId` state; `app.createSession` is extended to forward `system_prompt`/`agent_name` to `mgmtSessions.create` — the wire already supports both.

**Tech Stack:** Svelte 5 (runes: `$state`, `$derived`, `$effect`), TypeScript, Vite, Tailwind, bits-ui (already used by `ModelPicker`), `@lucide/svelte`. No new dependencies. No test runner in the repo — verification = `npm run check` for types + manual run in dev server (`npm run dev`).

**Spec:** `docs/superpowers/specs/2026-05-27-model-picker-favorites-agents-design.md`

---

## File map

| Action | Path | Responsibility |
|---|---|---|
| Create | `src/lib/picker-favorites.svelte.ts` | Reactive favorites store, LS-backed |
| Modify | `src/lib/utils.ts` | Add `LS.pickerFavorites` key |
| Modify | `src/lib/state.svelte.ts` | Extend `createSession` opts with `system_prompt` + `agent_name` |
| Modify | `src/lib/ModelPicker.svelte` | New rail icons, `RailView` union, three panel modes, star toggles |
| Modify | `src/lib/Composer.svelte` | Hold `selectedAgentId`, wire `onPickAgent`, attach persona on submit |

Layout stays a single Svelte file per concern; no decomposition of `ModelPicker` because the three modes share rail + container chrome and live <300 lines combined.

---

## Task 1: LS key registration

**Files:**
- Modify: `src/lib/utils.ts:94-107`

- [ ] **Step 1: Add the new key to the `LS` registry**

Edit `src/lib/utils.ts` — add a new entry inside the `LS` object literal. Place it after `lastScope` and before `builderSession`, alphabetic-ish to match the existing loose order:

```ts
  /** Pinned picker presets: engines and (agent, model) pairs.
   *  See picker-favorites.svelte.ts for the schema. */
  pickerFavorites: 'roy:picker:favorites',
```

- [ ] **Step 2: Verify types**

Run: `npm run check`
Expected: 0 errors. The new key is just a string literal added to a `const` object — no consumer references it yet.

- [ ] **Step 3: Commit**

```bash
git add src/lib/utils.ts
git commit -m "chore(utils): register LS.pickerFavorites key"
```

---

## Task 2: Favorites store

**Files:**
- Create: `src/lib/picker-favorites.svelte.ts`

- [ ] **Step 1: Create the store**

Create `src/lib/picker-favorites.svelte.ts` with the full contents below.

```ts
// Reactive pin-list for the model picker. Two pinned collections:
//
//   - engines: AgentPreset[]              — pinned engine rails
//   - models:  Array<{ agent, model }>    — pinned concrete (agent, model) pairs
//
// Both insertion-ordered, newest first. Persists to localStorage on every
// mutation; reads tolerate schema drift (drops unknown engines, keeps the
// rest). No server sync — favorites are a per-browser convenience.

import type { AgentPreset } from './wire';
import { LS, lsGetJSON, lsSetJSON } from './utils';

export type FavoriteModel = { agent: AgentPreset; model: string };

type Persisted = {
  engines: AgentPreset[];
  models: FavoriteModel[];
};

const KNOWN_PRESETS: ReadonlySet<AgentPreset> = new Set<AgentPreset>([
  'claude',
  'gemini',
  'opencode',
  'codex',
  'pi',
]);

function loadInitial(): Persisted {
  const raw = lsGetJSON<unknown>(LS.pickerFavorites, { engines: [], models: [] });
  // Defensive parse — anything that fails the shape check is dropped, but
  // valid neighbours survive. This is what keeps the store working after a
  // future schema bump or a hand-edited LS value.
  if (!raw || typeof raw !== 'object') return { engines: [], models: [] };
  const obj = raw as Record<string, unknown>;
  const engines = Array.isArray(obj.engines)
    ? (obj.engines.filter(
        (e): e is AgentPreset => typeof e === 'string' && KNOWN_PRESETS.has(e as AgentPreset),
      ) as AgentPreset[])
    : [];
  const models = Array.isArray(obj.models)
    ? obj.models.filter((m): m is FavoriteModel => {
        if (!m || typeof m !== 'object') return false;
        const v = m as Record<string, unknown>;
        return (
          typeof v.agent === 'string' &&
          KNOWN_PRESETS.has(v.agent as AgentPreset) &&
          typeof v.model === 'string'
        );
      })
    : [];
  return { engines, models };
}

class PickerFavorites {
  engines = $state<AgentPreset[]>([]);
  models = $state<FavoriteModel[]>([]);

  constructor() {
    const initial = loadInitial();
    this.engines = initial.engines;
    this.models = initial.models;
  }

  hasEngine(a: AgentPreset): boolean {
    return this.engines.includes(a);
  }

  hasModel(a: AgentPreset, m: string): boolean {
    return this.models.some((x) => x.agent === a && x.model === m);
  }

  toggleEngine(a: AgentPreset): void {
    this.engines = this.hasEngine(a)
      ? this.engines.filter((e) => e !== a)
      : [a, ...this.engines];
    this.persist();
  }

  toggleModel(a: AgentPreset, m: string): void {
    this.models = this.hasModel(a, m)
      ? this.models.filter((x) => !(x.agent === a && x.model === m))
      : [{ agent: a, model: m }, ...this.models];
    this.persist();
  }

  private persist(): void {
    lsSetJSON(LS.pickerFavorites, { engines: this.engines, models: this.models });
  }
}

export const pickerFavorites = new PickerFavorites();
```

- [ ] **Step 2: Verify types**

Run: `npm run check`
Expected: 0 errors.

- [ ] **Step 3: Sanity-check in the browser console**

Run: `npm run dev`, open the app, then in DevTools console:

```js
const m = await import('/src/lib/picker-favorites.svelte.ts');
m.pickerFavorites.toggleEngine('claude');
m.pickerFavorites.toggleModel('codex', 'gpt-5.4');
console.log(localStorage.getItem('roy:picker:favorites'));
// Expected: {"engines":["claude"],"models":[{"agent":"codex","model":"gpt-5.4"}]}
m.pickerFavorites.toggleEngine('claude'); // remove
console.log(m.pickerFavorites.engines);   // []
```

Expected: localStorage contains the JSON shown above between toggles. Final `engines` array is empty.

- [ ] **Step 4: Clean the LS key**

In the same console: `localStorage.removeItem('roy:picker:favorites')` — leaves the user's storage clean before the next task.

- [ ] **Step 5: Commit**

```bash
git add src/lib/picker-favorites.svelte.ts
git commit -m "feat(picker): favorites store backed by localStorage"
```

---

## Task 3: Extend `createSession` to carry persona

**Files:**
- Modify: `src/lib/state.svelte.ts:567-595`

- [ ] **Step 1: Inspect current signature and call site**

Run: `grep -n -A 30 "async createSession" src/lib/state.svelte.ts | head -45`
Expected output includes the existing `opts` param object (agent, project_id, scope, team_id, model, permission, firstPrompt) and a `mgmtSessions.create({ ... })` call.

- [ ] **Step 2: Add two optional fields to `createSession` opts**

Edit `src/lib/state.svelte.ts:567-575` — extend the inline parameter type. The block becomes:

```ts
  async createSession(opts: {
    agent: AgentPreset;
    project_id?: string;
    scope?: 'personal' | 'team';
    team_id?: string;
    model?: string;
    permission?: 'allow' | 'deny';
    firstPrompt?: string;
    /** Saved Agent persona to inject — forwarded to mgmtSessions.create
     *  as `system_prompt` / `agent_name`. Both nullable-on-purpose: omit
     *  for a plain engine spawn. */
    system_prompt?: string;
    agent_name?: string;
  }) {
```

- [ ] **Step 3: Forward the new fields to `mgmtSessions.create`**

Edit the `mgmtSessions.create({...})` call (around `state.svelte.ts:588-595`) so it passes the new fields through. After the change, that call reads:

```ts
      const created = await mgmtSessions.create({
        agent: opts.agent,
        project_id: opts.project_id || undefined,
        scope: opts.scope,
        team_id: opts.team_id || undefined,
        model: opts.model || undefined,
        permission: opts.permission || undefined,
        system_prompt: opts.system_prompt || undefined,
        agent_name: opts.agent_name || undefined,
      });
```

- [ ] **Step 4: Verify types**

Run: `npm run check`
Expected: 0 errors. (`CreateSessionReq` in `management-client.ts:80-91` already declares both fields, so this is purely additive at the boundary.)

- [ ] **Step 5: Commit**

```bash
git add src/lib/state.svelte.ts
git commit -m "feat(state): createSession forwards system_prompt + agent_name"
```

---

## Task 4: `ModelPicker` — `RailView` union (no UI yet)

**Files:**
- Modify: `src/lib/ModelPicker.svelte:32-46, 60-69`

This task only refactors internal state. UI is unchanged. Splits the larger redesign into a safe, type-checked intermediate state.

- [ ] **Step 1: Add the `RailView` type and replace `railAgent`**

Inside the `<script lang="ts">` block of `src/lib/ModelPicker.svelte`, replace the `let railAgent = $state<AgentPreset>(agent);` declaration and its sync `$effect` with the union-shaped version. The replaced region (currently lines 32-46) becomes:

```ts
  type RailView =
    | { kind: 'favorites' }
    | { kind: 'agents' }
    | { kind: 'engine'; preset: AgentPreset };

  let open = $state(false);
  // Tagged-union rail state: lets the rail icon and the right panel share
  // one source of truth. External `agent` changes only ever map back to an
  // engine view (never favorites/agents), so the sync $effect stays simple.
  let railView = $state<RailView>({ kind: 'engine', preset: agent });
  let search = $state('');

  // Sync the rail when the parent's `agent` changes externally — but only
  // for engine views. If the user is browsing favorites/agents we leave
  // them where they were (engine-change isn't a navigation event for them).
  $effect(() => {
    if (railView.kind === 'engine') {
      railView = { kind: 'engine', preset: agent };
    }
    search = '';
  });
```

- [ ] **Step 2: Update `currentList` / `filteredList` to read from the union**

Replace the existing `currentList` / `filteredList` `$derived` blocks (lines 60-65) with the version below — `currentList` is empty unless we're in engine mode:

```ts
  const currentList = $derived(
    railView.kind === 'engine'
      ? (catalog.find((a) => a.preset === railView.preset)?.models ?? [])
      : [],
  );
  const filteredList = $derived.by(() => {
    const q = search.trim().toLowerCase();
    if (!q) return currentList;
    return currentList.filter((m) => m.label.toLowerCase().includes(q));
  });
```

- [ ] **Step 3: Update `pickAgent` and `pickModel` to the union shape**

Replace the `pickAgent` and `pickModel` functions (around lines 71-81) with:

```ts
  function pickAgent(a: AgentPreset) {
    if (lockAgent) return;
    railView = { kind: 'engine', preset: a };
  }

  function pickModel(a: AgentPreset, m: ModelInfo) {
    if (!lockAgent) agent = a;
    model = m.id;
    open = false;
    onChange?.(m.id);
  }
```

- [ ] **Step 4: Update the rail-icon template to test the union**

Replace the `{@const active = a === railAgent}` line inside the `{#each presets ...}` block (around line 110) with:

```svelte
        {@const active = railView.kind === 'engine' && railView.preset === a}
```

- [ ] **Step 5: Update the header + list bindings inside the right column**

The right column currently reads `railAgent` in three places (header `agentIcon`, `agentMeta` label, and the per-row tint chip inside the model list). Replace with conditional reads scoped to engine mode. The simplest patch is to lift the active preset into a `$derived`:

```ts
  const railPreset = $derived(
    railView.kind === 'engine' ? railView.preset : agent,
  );
```

Then replace remaining `railAgent` references in the template with `railPreset`. (There are exactly three: the header `ProviderIcon`/label, and the per-row `AgentIcon`/label inside the `{#each filteredList}` block.)

- [ ] **Step 6: Verify types and behavior**

Run: `npm run check`
Expected: 0 errors.

Run: `npm run dev`. Open the picker in the new-chat composer. Expected: behavior is identical to today — clicking engine icons switches the model list, search works, picking a model closes the popover. No new UI yet.

- [ ] **Step 7: Commit**

```bash
git add src/lib/ModelPicker.svelte
git commit -m "refactor(picker): replace railAgent with RailView union"
```

---

## Task 5: `ModelPicker` — `★` Favorites rail icon and panel

**Files:**
- Modify: `src/lib/ModelPicker.svelte` (script + template)

- [ ] **Step 1: Import the favorites store and the Star icon**

Add to the existing import block at the top of `<script lang="ts">`:

```ts
  import { Star } from '@lucide/svelte';
  import { pickerFavorites } from './picker-favorites.svelte';
```

- [ ] **Step 2: Add a helper to resolve a default model for an engine**

Add this `$derived`-style helper inside the script, near the existing `agentModels` declaration. It's used both by the favorites panel (clicking a favorite engine) and by the agents panel later (Task 7) when `Agent.model` is null.

```ts
  function defaultModelFor(preset: AgentPreset): ModelInfo | undefined {
    const entry = catalog.find((a) => a.preset === preset);
    if (!entry) return undefined;
    return entry.models.find((m) => m.default) ?? entry.models[0];
  }
```

- [ ] **Step 3: Inject the `★` icon at the top of the rail**

In the rail template (currently `{#if !lockAgent} ... {#each presets ...}`), add a button BEFORE the `{#each}` so the star is the first item. The block becomes:

```svelte
    <div class="flex h-full w-14 shrink-0 flex-col items-center gap-1.5 border-r border-border/60 bg-muted/30 py-3">
      <button
        type="button"
        title="Favorites"
        aria-label="Favorites"
        onclick={() => (railView = { kind: 'favorites' })}
        class={[
          'flex size-9 items-center justify-center rounded-lg transition-colors',
          railView.kind === 'favorites'
            ? 'bg-muted text-foreground'
            : 'text-muted-foreground/70 hover:bg-muted/60 hover:text-foreground',
        ]}
      >
        <Star class="size-[18px]" />
      </button>

      {#each presets as a (a)}
        <!-- existing engine button (unchanged) -->
        ...
      {/each}
    </div>
```

(Keep the existing `{#each}` body as-is; only the wrapping `<div>` gets the new sibling.)

- [ ] **Step 4: Render the favorites panel in place of the engine list**

Wrap the existing header + search + list in `{#if railView.kind === 'engine'}` and add a `{:else if railView.kind === 'favorites'}` branch. Replace the entire right-column block (the `<div class="flex h-full flex-1 flex-col">` that follows the rail) with:

```svelte
    <div class="flex h-full flex-1 flex-col">
      {#if railView.kind === 'engine'}
        <!-- header -->
        <div class="flex items-center gap-2 border-b border-border/60 px-3 py-2.5">
          <ProviderIcon name={agentIcon(railPreset)!} class="size-4 shrink-0 text-foreground" />
          <span class="text-sm font-semibold">{agentMeta[railPreset].label}</span>
          <button
            type="button"
            aria-label={pickerFavorites.hasEngine(railPreset) ? 'Unpin engine' : 'Pin engine'}
            title={pickerFavorites.hasEngine(railPreset) ? 'Unpin engine' : 'Pin engine'}
            onclick={() => pickerFavorites.toggleEngine(railPreset)}
            class="ml-auto flex size-6 items-center justify-center rounded text-muted-foreground hover:bg-muted hover:text-foreground"
          >
            <Star
              class={[
                'size-4',
                pickerFavorites.hasEngine(railPreset) ? 'fill-yellow-400 text-yellow-400' : '',
              ]}
            />
          </button>
        </div>
        <!-- search + list (unchanged from today) -->
        <div class="relative border-b border-border/60">
          <Search class="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" aria-hidden="true" />
          <input
            type="text"
            bind:value={search}
            placeholder="Search models…"
            class="h-10 w-full bg-transparent pl-9 pr-3 text-sm text-foreground placeholder:text-muted-foreground focus:outline-none"
          />
        </div>
        <ul class="flex-1 overflow-y-auto p-1">
          {#if filteredList.length === 0}
            <li class="px-3 py-4 text-center text-xs text-muted-foreground">No models.</li>
          {:else}
            {#each filteredList as m (m.id)}
              {@const selected = railPreset === agent && m.id === model}
              {@const pinned = pickerFavorites.hasModel(railPreset, m.id)}
              <li>
                <button
                  type="button"
                  onclick={() => pickModel(railPreset, m)}
                  class={[
                    'group/row flex w-full cursor-pointer items-center gap-3 rounded-md px-2 py-2 text-left transition-colors',
                    selected ? 'bg-muted' : 'hover:bg-muted/60',
                  ]}
                >
                  <div class="flex min-w-0 flex-1 flex-col">
                    <span class="truncate text-sm font-medium text-foreground">{m.label}</span>
                    <span class="flex items-center gap-1.5 truncate text-xs text-muted-foreground">
                      <AgentIcon agent={railPreset} model={m.id} class="size-3 shrink-0" />
                      <span>{agentMeta[railPreset].label}</span>
                    </span>
                  </div>
                  <span
                    role="button"
                    tabindex="0"
                    aria-label={pinned ? 'Unpin model' : 'Pin model'}
                    title={pinned ? 'Unpin model' : 'Pin model'}
                    onclick={(e) => {
                      e.stopPropagation();
                      pickerFavorites.toggleModel(railPreset, m.id);
                    }}
                    onkeydown={(e) => {
                      if (e.key === 'Enter' || e.key === ' ') {
                        e.preventDefault();
                        e.stopPropagation();
                        pickerFavorites.toggleModel(railPreset, m.id);
                      }
                    }}
                    class={[
                      'flex size-6 shrink-0 items-center justify-center rounded transition-opacity hover:bg-muted',
                      pinned ? 'opacity-100' : 'opacity-0 group-hover/row:opacity-100 focus-within:opacity-100',
                    ]}
                  >
                    <Star
                      class={[
                        'size-3.5',
                        pinned ? 'fill-yellow-400 text-yellow-400' : 'text-muted-foreground',
                      ]}
                    />
                  </span>
                </button>
              </li>
            {/each}
          {/if}
        </ul>
      {:else if railView.kind === 'favorites'}
        <div class="flex items-center gap-2 border-b border-border/60 px-3 py-2.5">
          <Star class="size-4 fill-yellow-400 text-yellow-400" />
          <span class="text-sm font-semibold">Favorites</span>
        </div>
        <div class="flex-1 overflow-y-auto p-1">
          {#if pickerFavorites.engines.length === 0 && pickerFavorites.models.length === 0}
            <p class="px-3 py-6 text-center text-xs text-muted-foreground">
              No favorites yet. Star a model in any engine to pin it here.
            </p>
          {:else}
            {#if pickerFavorites.engines.length > 0}
              <p class="px-2 pt-1 pb-1 text-[10px] font-semibold uppercase tracking-wide text-muted-foreground">Engines</p>
              <ul class="mb-2 flex flex-col gap-0.5">
                {#each pickerFavorites.engines as a (a)}
                  <li>
                    <button
                      type="button"
                      onclick={() => {
                        const def = defaultModelFor(a);
                        if (def) pickModel(a, def);
                      }}
                      class="flex w-full cursor-pointer items-center gap-2 rounded-md px-2 py-2 text-left transition-colors hover:bg-muted/60"
                    >
                      <ProviderIcon name={agentIcon(a)!} class="size-4 shrink-0 text-foreground" />
                      <span class="truncate text-sm font-medium">{agentMeta[a].label}</span>
                      <span class="ml-auto text-[10px] text-muted-foreground">default model</span>
                    </button>
                  </li>
                {/each}
              </ul>
            {/if}

            {#if pickerFavorites.models.length > 0}
              <p class="px-2 pt-1 pb-1 text-[10px] font-semibold uppercase tracking-wide text-muted-foreground">Models</p>
              <ul class="flex flex-col gap-0.5">
                {#each pickerFavorites.models as f (f.agent + '/' + f.model)}
                  {@const entry = catalog.find((e) => e.preset === f.agent)}
                  {@const info = entry?.models.find((m) => m.id === f.model)}
                  {#if info}
                    {@const selected = f.agent === agent && info.id === model}
                    <li>
                      <button
                        type="button"
                        onclick={() => pickModel(f.agent, info)}
                        class={[
                          'flex w-full cursor-pointer items-center gap-3 rounded-md px-2 py-2 text-left transition-colors',
                          selected ? 'bg-muted' : 'hover:bg-muted/60',
                        ]}
                      >
                        <div class="flex min-w-0 flex-1 flex-col">
                          <span class="truncate text-sm font-medium text-foreground">{info.label}</span>
                          <span class="flex items-center gap-1.5 truncate text-xs text-muted-foreground">
                            <AgentIcon agent={f.agent} model={info.id} class="size-3 shrink-0" />
                            <span>{agentMeta[f.agent].label}</span>
                          </span>
                        </div>
                      </button>
                    </li>
                  {/if}
                {/each}
              </ul>
            {/if}
          {/if}
        </div>
      {/if}
    </div>
```

Note: the snippet uses the `railPreset` `$derived` introduced in Task 4 step 5 — make sure that derivation is in scope before saving.

- [ ] **Step 6: Verify types**

Run: `npm run check`
Expected: 0 errors.

- [ ] **Step 7: Manual verification**

Run: `npm run dev`. In the new-chat composer:

1. Open the picker — see the `★` icon at the top of the rail.
2. Click an engine, hover a model row — the star toggle appears on the right. Click it — the model row keeps a yellow filled star and the storage key `roy:picker:favorites` (check DevTools → Application → Local Storage) gains a `models` entry.
3. Click another engine's header star icon — `engines` array gains that preset.
4. Click the `★` rail icon — see two subsections (`ENGINES`, `MODELS`) populated. Click a model row — picker closes, that model is selected in the trigger pill.
5. Click `★` again with no favorites (empty `roy:picker:favorites` first) — see the "No favorites yet…" placeholder.

- [ ] **Step 8: Commit**

```bash
git add src/lib/ModelPicker.svelte
git commit -m "feat(picker): Favorites rail icon + panel with engine/model pins"
```

---

## Task 6: Thread the Agent-pick callback through `ModelPicker`

**Files:**
- Modify: `src/lib/ModelPicker.svelte` (props block)

This task only adds the new optional prop. The Agents panel itself is built in Task 7. Splitting these keeps the diff per commit small and lets the prop addition stay independent of the panel markup.

- [ ] **Step 1: Add the optional `onPickAgent` prop**

Edit the `$props()` destructuring at the top of `src/lib/ModelPicker.svelte` (currently around lines 9-30). The block becomes:

```ts
  let {
    agent = $bindable(),
    model = $bindable(),
    catalog,
    disabled = false,
    lockAgent = false,
    onChange,
    /** Called when the user picks a saved Agent persona from the Agents
     *  panel. The component still updates `agent` + `model` from the
     *  persona; this callback lets the parent record the agent id so it
     *  can be forwarded as `system_prompt` + `agent_name` on submit. */
    onPickAgent,
  }: {
    agent: AgentPreset;
    model: string;
    catalog: AgentInfo[];
    disabled?: boolean;
    lockAgent?: boolean;
    onChange?: (model: string) => void;
    onPickAgent?: (agentId: string) => void;
  } = $props();
```

- [ ] **Step 2: Verify types**

Run: `npm run check`
Expected: 0 errors. The prop is optional and unused so far.

- [ ] **Step 3: Commit**

```bash
git add src/lib/ModelPicker.svelte
git commit -m "refactor(picker): add optional onPickAgent prop"
```

---

## Task 7: `ModelPicker` — `⌗` Agents rail icon and panel

**Files:**
- Modify: `src/lib/ModelPicker.svelte`

- [ ] **Step 1: Import the agents store**

Add to the script imports:

```ts
  import { onMount } from 'svelte';
  import { Star, Plus } from '@lucide/svelte';  // Plus already imported elsewhere if present; keep one
  import { agents } from './management-agents.svelte';
```

Reuse the existing `Plus` import if already imported elsewhere — keep one. The `Star` import was added in Task 5.

- [ ] **Step 2: Refresh agents lazily when the panel opens**

Add this `$effect` inside the script (after the existing `$effect` that syncs `railView`):

```ts
  // Lazy-fetch the agents list the first time the user opens the panel
  // for this picker instance. `agents.refresh()` is a no-op while loading,
  // so it's safe to re-trigger as `railView` changes back and forth.
  $effect(() => {
    if (railView.kind === 'agents' && agents.list.length === 0 && !agents.loading) {
      void agents.refresh();
    }
  });
```

- [ ] **Step 3: Add the `⌗` rail icon**

Inside the rail `<div>` template (Task 5 added the `★` button before `{#each presets}`), add a second button between `★` and `{#each presets}`:

```svelte
      <button
        type="button"
        title="Agents"
        aria-label="Agents"
        onclick={() => (railView = { kind: 'agents' })}
        class={[
          'flex size-9 items-center justify-center rounded-lg transition-colors',
          railView.kind === 'agents'
            ? 'bg-muted text-foreground'
            : 'text-muted-foreground/70 hover:bg-muted/60 hover:text-foreground',
        ]}
      >
        <!-- Inline `⌗` glyph matches AgentsView's tone. Using lucide's
             `Hash` would shift the visual register; a span keeps the rail
             monolithic without pulling another icon import. -->
        <span class="text-[18px] leading-none">⌗</span>
      </button>
```

- [ ] **Step 4: Add the Agents panel branch**

Inside the right column wrapper, add a third branch after `{:else if railView.kind === 'favorites'}`:

```svelte
      {:else if railView.kind === 'agents'}
        <div class="flex items-center gap-2 border-b border-border/60 px-3 py-2.5">
          <span class="text-base leading-none">⌗</span>
          <span class="text-sm font-semibold">Agents</span>
        </div>
        <div class="flex-1 overflow-y-auto p-1">
          {#if agents.error}
            <p class="m-2 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-xs text-destructive">
              {agents.error}
            </p>
          {/if}
          {#if agents.loading && agents.list.length === 0}
            <p class="px-3 py-6 text-center text-xs text-muted-foreground">Loading…</p>
          {:else if agents.list.length === 0}
            <p class="px-3 py-6 text-center text-xs text-muted-foreground">
              No agents yet. Create one in the Agents tab.
            </p>
          {:else}
            <ul class="flex flex-col gap-0.5">
              {#each agents.list as a (a.id)}
                <li>
                  <button
                    type="button"
                    onclick={() => {
                      const preset = a.preset as AgentPreset;
                      const info =
                        (a.model
                          ? catalog.find((e) => e.preset === preset)?.models.find((m) => m.id === a.model)
                          : undefined) ?? defaultModelFor(preset);
                      if (!info) return;
                      // Pick the model first (this also closes the popover
                      // via pickModel's `open = false`); the parent will
                      // attach the persona on submit.
                      pickModel(preset, info);
                      onPickAgent?.(a.id);
                    }}
                    class="flex w-full cursor-pointer flex-col items-start gap-1 rounded-md px-2 py-2 text-left transition-colors hover:bg-muted/60"
                  >
                    <div class="flex w-full items-center gap-2">
                      <span class="truncate text-sm font-semibold">{a.name}</span>
                      <span class="text-xs text-muted-foreground">·</span>
                      <code class="truncate text-xs text-muted-foreground">{a.slug}</code>
                      <span class="ml-auto rounded bg-muted px-1.5 py-0.5 text-[10px] font-mono uppercase text-muted-foreground">
                        {a.preset}
                      </span>
                    </div>
                    {#if a.prompt}
                      <p class="line-clamp-2 w-full whitespace-pre-wrap text-xs text-muted-foreground">
                        {a.prompt}
                      </p>
                    {/if}
                  </button>
                </li>
              {/each}
            </ul>
          {/if}
        </div>
      {/if}
```

- [ ] **Step 5: Hide both new rail icons under `lockAgent`**

The rail block is already wrapped in `{#if !lockAgent}` (existing line 107). Both new icons inherit that guard automatically — no extra work, but verify by reading the surrounding `{#if}/{/if}` brackets after the edits.

- [ ] **Step 6: Verify types**

Run: `npm run check`
Expected: 0 errors.

- [ ] **Step 7: Manual verification**

Run: `npm run dev`. In the new-chat composer:

1. Open the picker, click the `⌗` icon. If no agents exist: see the empty-state hint. If agents exist: see the list with name · slug · preset chip + prompt preview.
2. Click an agent — popover closes, the engine/model in the trigger pill update to that agent's preset+model.
3. Open the picker on an existing chat (open any session in `ChatView`) — only the engine icons should be visible (no `★`/`⌗`). The rail is hidden entirely in `ChatView` already, so verify by checking that today's locked-agent picker still renders a single column.

- [ ] **Step 8: Commit**

```bash
git add src/lib/ModelPicker.svelte
git commit -m "feat(picker): Agents rail icon + panel applies persona as preset"
```

---

## Task 8: `Composer` — hold selected Agent and attach persona on submit

**Files:**
- Modify: `src/lib/Composer.svelte`

- [ ] **Step 1: Add `selectedAgentId` state and import the agents store**

Inside the `<script lang="ts">` of `src/lib/Composer.svelte`, add the store import and a new state declaration near the existing `let agent / let model`:

```ts
  import { agents } from './management-agents.svelte';

  // ... existing state ...
  let selectedAgentId = $state<string | undefined>(undefined);
```

- [ ] **Step 2: Wire `onPickAgent` and clear on manual changes**

Pass the callback into `<ModelPicker>`. The render at line 381 becomes:

```svelte
      <ModelPicker
        bind:agent={agent as AgentPreset}
        bind:model
        catalog={enginesConfig.engines}
        disabled={submitting}
        onChange={() => { selectedAgentId = undefined; }}
        onPickAgent={(id) => { selectedAgentId = id; }}
      />
```

The `onChange` here is the model-change side-effect callback already used by `ChatView` for hot-swap; in the composer it's repurposed to invalidate the agent selection when the user picks a different model manually. Note: when an agent is picked, `pickModel` fires `onChange` first and `onPickAgent` second. Order matters — `selectedAgentId` ends up set, which is what we want. Verify by reading `ModelPicker.svelte:pickModel` after Task 7 and confirming the call order.

- [ ] **Step 3: Verify call order in `pickModel`**

Open `src/lib/ModelPicker.svelte` and find the `pickModel` function. The body should be (left as-is from Task 4):

```ts
  function pickModel(a: AgentPreset, m: ModelInfo) {
    if (!lockAgent) agent = a;
    model = m.id;
    open = false;
    onChange?.(m.id);
  }
```

The agents-panel button (Task 7 step 4) calls `pickModel(preset, info)` first, then `onPickAgent?.(a.id)`. So inside `pickModel`, `onChange` fires before `onPickAgent` — the composer clears `selectedAgentId` first, then re-sets it. Correct order, no extra work.

- [ ] **Step 4: Forward persona on submit**

Edit `onSubmit` in `src/lib/Composer.svelte` (around lines 283-301). The body becomes:

```ts
  async function onSubmit(e: SubmitEvent) {
    e.preventDefault();
    const text = draft.trim();
    if (!text || submitting || !agent) return;

    // Resolve persona from selectedAgentId. If the saved Agent was
    // deleted between selection and submit, fall back to a plain spawn
    // and clear the stale id.
    const persona =
      selectedAgentId !== undefined
        ? agents.list.find((a) => a.id === selectedAgentId)
        : undefined;
    if (selectedAgentId !== undefined && !persona) {
      selectedAgentId = undefined;
    }

    try {
      const sessionId = await app.createSession({
        agent: agent as AgentPreset,
        project_id: selectedProjectId || undefined,
        scope: selectedTeamId ? 'team' : 'personal',
        team_id: selectedTeamId || undefined,
        model,
        firstPrompt: text,
        system_prompt: persona?.prompt || undefined,
        agent_name: persona?.name || undefined,
      });
      onCreated(sessionId);
    } catch {
      // createSession already published into `app.lastError`.
    }
  }
```

- [ ] **Step 5: Verify types**

Run: `npm run check`
Expected: 0 errors.

- [ ] **Step 6: Manual verification — happy path**

Run: `npm run dev`. Sign in if needed.

1. Visit `/agents`. Create a test agent: name "Test persona", preset `codex`, prompt "You are a pirate. End every reply with 'Arr.'".
2. Go to a fresh `/new` (or root) and open the picker → `⌗` Agents → click "Test persona". The trigger pill now shows the agent's preset/model.
3. Type "hello" in the composer and submit.
4. Confirm: the new session opens, and the first assistant response includes the pirate persona (ends with "Arr." or similar). If the persona didn't take, check the network panel for the `POST /management/sessions` request body — it should include `system_prompt` and `agent_name` fields.

- [ ] **Step 7: Manual verification — manual override clears selection**

Repeat step 6 up to picking the agent. Then, without submitting, open the picker again and pick a different model from any engine view. Submit. The spawned session should NOT have the persona applied. Confirm by reading the assistant's first reply — it should be neutral. Check the network panel to verify `system_prompt`/`agent_name` are absent.

- [ ] **Step 8: Manual verification — deleted Agent doesn't crash submit**

Pick a saved Agent in the composer, then in a second tab delete that agent from `/agents`. Switch back, submit. Expected: spawn proceeds without persona; no console errors. `selectedAgentId` is cleared by the fallback in step 4.

- [ ] **Step 9: Commit**

```bash
git add src/lib/Composer.svelte
git commit -m "feat(composer): apply saved Agent persona as spawn preset"
```

---

## Task 9: Polish — picker pill reflects selected Agent

**Files:**
- Modify: `src/lib/ModelPicker.svelte` (template only)

A small UX improvement so the user can tell at a glance whether the next spawn carries a persona.

- [ ] **Step 1: Add an optional `agentLabel` prop**

Inside the `$props()` block, add:

```ts
    /** Label override for the trigger pill — used by Composer to surface
     *  the selected Agent's name. Falls back to the model label. */
    agentLabel?: string;
```

Update the destructure list at the top to include `agentLabel`.

- [ ] **Step 2: Use `agentLabel` in the trigger pill**

In the `<Popover.Trigger>` (currently around lines 85-96), change:

```svelte
    <span class="font-mono">{currentInfo?.label ?? model}</span>
```

to:

```svelte
    <span class="font-mono">{agentLabel ?? currentInfo?.label ?? model}</span>
```

- [ ] **Step 3: Wire the label from `Composer`**

In `src/lib/Composer.svelte`, derive the label from `selectedAgentId`:

```ts
  let selectedAgentLabel = $derived.by(() => {
    if (selectedAgentId === undefined) return undefined;
    const a = agents.list.find((x) => x.id === selectedAgentId);
    return a ? `⌗ ${a.name}` : undefined;
  });
```

Then pass it to `<ModelPicker>`:

```svelte
      <ModelPicker
        bind:agent={agent as AgentPreset}
        bind:model
        catalog={enginesConfig.engines}
        disabled={submitting}
        agentLabel={selectedAgentLabel}
        onChange={() => { selectedAgentId = undefined; }}
        onPickAgent={(id) => { selectedAgentId = id; }}
      />
```

- [ ] **Step 4: Verify types and behavior**

Run: `npm run check`
Expected: 0 errors.

Run: `npm run dev`. Pick an agent — the trigger pill now shows `⌗ <agent name>` instead of the raw model id. Pick a different model from any engine view — pill falls back to the model label.

- [ ] **Step 5: Commit**

```bash
git add src/lib/ModelPicker.svelte src/lib/Composer.svelte
git commit -m "feat(picker): pill shows selected Agent name"
```

---

## Task 10: Final end-to-end pass + cleanup

- [ ] **Step 1: Full type check**

Run: `npm run check`
Expected: 0 errors, 0 warnings.

- [ ] **Step 2: Walkthrough**

Run: `npm run dev`. Walk through every interaction one more time:

1. Open new-chat composer → picker shows `★`, `⌗`, then five engine icons.
2. In an engine view: hover a model row → star toggle appears → click → pinned. Header star toggles engine pin.
3. `★` view: shows two subsections; clicking a pinned engine selects its default model; clicking a pinned model selects that pair. Empty state appears with no favorites.
4. `⌗` view: shows agents (or empty state); clicking applies persona; pill shows `⌗ <name>`.
5. Submitting a draft with a selected agent injects `system_prompt`/`agent_name` (verify in DevTools → Network).
6. Open any existing chat → picker omits the rail entirely (current `lockAgent` behavior, untouched).
7. Delete a pinned model's engine from `enginesConfig` (simulate by editing the daemon-side config or skip — exercise the schema-drift guard by writing a bogus entry directly into LS: `localStorage.setItem('roy:picker:favorites', JSON.stringify({engines: ['ghost'], models: [{agent: 'ghost', model: 'x'}]}))`, reload). Expected: bogus entries are dropped on load; valid favorites survive.

- [ ] **Step 3: Reset any test agents**

Delete the "Test persona" agent from `/agents` and clear the `roy:picker:favorites` LS key if you don't want to keep your test pins.

- [ ] **Step 4: Final commit (if anything stragglers)**

If walkthrough revealed nothing, skip. Otherwise:

```bash
git add -p   # review each hunk
git commit -m "fix(picker): <whatever you found>"
```

---

## Self-review notes (pre-handoff)

- All spec sections have a task: Favorites store (Task 2), Agents store usage (Task 7), `createSession` extension (Task 3), rail union refactor (Task 4), Favorites UI (Task 5), Agents UI (Task 7), Composer wiring (Task 8), pill label (Task 9).
- Type consistency: `RailView` defined in Task 4 referenced consistently in Tasks 5, 7; `pickerFavorites` API (`hasEngine`, `hasModel`, `toggleEngine`, `toggleModel`, `engines`, `models`) used identically across Tasks 5, 7, 10.
- No placeholders. Every step contains either a literal code block, an exact command, or a concrete verification step.
- Order: store before UI; rail-union refactor (no UI change) before each panel; `onPickAgent` prop before its usage; persona forwarding in state.svelte.ts before the composer relies on it.
