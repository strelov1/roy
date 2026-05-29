<script lang="ts">
  import { untrack } from 'svelte';
  import type { Harness, HarnessInfo, ModelInfo } from './wire';
  import { KNOWN_HARNESSES } from './wire';
  import * as Popover from '$lib/components/ui/popover';
  import { Bot, ChevronDown, Search, Star } from '@lucide/svelte';
  import ProviderIcon from './ProviderIcon.svelte';
  import AgentIcon from './AgentIcon.svelte';
  import { agentIcon } from './provider-icons';
  import { pickerFavorites } from './picker-favorites.svelte';
  import { agentsStore } from './agents.svelte';
  import { defaultModelFor } from './harnesses-config.svelte';

  let {
    agent = $bindable(),
    model = $bindable(),
    catalog,
    disabled = false,
    // When true, the picker only chooses *within* `agent`'s catalog —
    // the agent rail is read-only, and a model click never writes back
    // to `agent`. Used by ChatView, where the underlying ACP transport
    // is bound to the agent at spawn time and can't be hot-swapped.
    lockAgent = false,
    // Optional side-effect callback fired *after* a model is picked.
    // Useful for ChatView to push the new value through to the daemon
    // via `setModel` without listening on a separate `$effect`.
    onChange,
    /** Called when the user picks a saved Agent persona from the Agents
     *  panel. The component still updates `agent` + `model` from the
     *  persona; this callback lets the parent record the agent id so it
     *  can be forwarded as `system_prompt` + `agent_name` on submit. */
    onPickAgent,
    /** Label override for the trigger pill — used by Composer to surface
     *  the selected Agent's name. Falls back to the model label. */
    agentLabel,
  }: {
    agent: Harness;
    model: string;
    catalog: HarnessInfo[];
    disabled?: boolean;
    lockAgent?: boolean;
    onChange?: (model: string) => void;
    onPickAgent?: (agentId: string) => void;
    agentLabel?: string;
  } = $props();

  type RailView =
    | { kind: 'favorites' }
    | { kind: 'agents' }
    | { kind: 'harness'; harness: Harness };

  let open = $state(false);
  // Tagged-union rail state: lets the rail icon and the right panel share
  // one source of truth. External `agent` changes only ever map back to a
  // harness view (never favorites/agents), so the sync $effect stays simple.
  let railView = $state<RailView>({ kind: 'harness', harness: agent });
  let search = $state('');

  // O(1) lookup: catalog by harness.
  const catalogByName = $derived(
    new Map(catalog.map((entry) => [entry.name, entry])),
  );

  // O(1) membership checks for pin state — avoids array scans in the template.
  const pinnedHarnesses = $derived(new Set(pickerFavorites.harnesses));
  const pinnedModels = $derived(
    new Set(pickerFavorites.models.map((f) => `${f.agent}/${f.model}`)),
  );

  // Sync the rail when the parent's `agent` prop changes externally — but
  // only for harness views. If the user is browsing favorites/agents we
  // leave them where they were. `untrack` prevents the write to `railView`
  // from re-firing this effect (new object literals don't `safe_equal`).
  $effect(() => {
    const harness = agent;
    untrack(() => {
      if (railView.kind === 'harness' && railView.harness !== harness) {
        railView = { kind: 'harness', harness };
      }
    });
  });

  // Fetch the agents list when the popover opens — the rail decides
  // whether to show the Agents tab based on `agents.list.length`, so we
  // need data *before* the user clicks anything. Guarded against repeat
  // fetches while loading or once populated.
  $effect(() => {
    if (open && agentsStore.list.length === 0 && !agentsStore.loading) {
      void agentsStore.load();
    }
  });

  // Visibility for the new rail tabs: hide entirely when there's nothing
  // to show. `pickerFavorites` derives reactively; `agents.list` updates
  // after the fetch above resolves.
  const showFavoritesTab = $derived(
    pickerFavorites.harnesses.length > 0 || pickerFavorites.models.length > 0,
  );
  const showAgentsTab = $derived(agentsStore.list.length > 0);

  // Per-agent label. Icon mapping lives in `agentIcon()` so
  // header/sidebar/picker share one source of truth.
  const agentMeta: Record<Harness, string> = {
    opencode: 'OpenCode',
    claude: 'Claude',
    gemini: 'Gemini',
    codex: 'Codex',
    pi: 'pi',
  };

  const harnesses: Harness[] = ['opencode', 'claude', 'gemini', 'codex', 'pi'];

  // Left-rail icon-button classes (active vs idle), shared across the
  // Favorites / Agents / harness buttons.
  const railBtn = 'flex size-9 items-center justify-center rounded-lg transition-colors';
  const railBtnActive = 'bg-muted text-foreground';
  const railBtnIdle = 'text-muted-foreground/70 hover:bg-muted/60 hover:text-foreground';

  const currentList = $derived.by(() => {
    const rv = railView;
    if (rv.kind !== 'harness') return [];
    return catalog.find((a) => a.name === rv.harness)?.models ?? [];
  });
  const filteredList = $derived.by(() => {
    const q = search.trim().toLowerCase();
    if (!q) return currentList;
    return currentList.filter((m) => m.label.toLowerCase().includes(q));
  });
  const agentModels = $derived(catalog.find((a) => a.name === agent)?.models ?? []);

  const currentInfo = $derived(
    agentModels.find((m) => m.id === model) ?? agentModels[0],
  );

  function pickAgent(a: Harness) {
    if (lockAgent) return;
    railView = { kind: 'harness', harness: a };
    search = '';
  }

  function pickModel(a: Harness, m: ModelInfo) {
    if (!lockAgent) agent = a;
    model = m.id;
    open = false;
    onChange?.(m.id);
  }

  const railHarness = $derived(
    railView.kind === 'harness' ? railView.harness : agent,
  );
</script>

<Popover.Root bind:open>
  <Popover.Trigger
    {disabled}
    class="inline-flex h-8 items-center gap-1.5 rounded-full border border-border bg-background px-3 text-xs font-medium hover:bg-muted disabled:cursor-not-allowed disabled:opacity-50"
  >
    <AgentIcon
      agent={agent}
      model={model}
      class="size-3.5 shrink-0 text-foreground"
    />
    <span class="font-mono">{agentLabel ?? currentInfo?.label ?? model}</span>
    <ChevronDown class="size-3.5 text-muted-foreground" aria-hidden="true" />
  </Popover.Trigger>

  <Popover.Content
    side="top"
    align="start"
    sideOffset={8}
    class="flex flex-row h-[28rem] w-[40rem] max-w-[90vw] gap-0 overflow-hidden rounded-2xl border border-border/60 bg-popover p-0 text-popover-foreground shadow-xl"
  >
    <!-- Left rail: harness icons. `h-full` lets the rail span the popover
         height so the right column's search/header align cleanly with it.
         Hidden when the parent locks us to a single agent (ChatView). -->
    {#if !lockAgent}
    <div class="flex h-full w-14 shrink-0 flex-col items-center gap-1.5 border-r border-border/60 bg-muted/30 py-3">
      {#if showFavoritesTab}
        <button
          type="button"
          title="Favorites"
          aria-label="Favorites"
          onclick={() => { railView = { kind: 'favorites' }; search = ''; }}
          class={[railBtn, railView.kind === 'favorites' ? railBtnActive : railBtnIdle]}
        >
          <Star class="size-[18px]" />
        </button>
      {/if}

      {#if showAgentsTab}
        <button
          type="button"
          title="Agents"
          aria-label="Agents"
          onclick={() => { railView = { kind: 'agents' }; search = ''; }}
          class={[railBtn, railView.kind === 'agents' ? railBtnActive : railBtnIdle]}
        >
          <Bot class="size-[18px]" />
        </button>
      {/if}

      {#each harnesses as a (a)}
        {@const active = railView.kind === 'harness' && railView.harness === a}
        <button
          type="button"
          title={agentMeta[a]}
          aria-label={agentMeta[a]}
          onclick={() => pickAgent(a)}
          class={[railBtn, active ? railBtnActive : railBtnIdle]}
        >
          <ProviderIcon name={agentIcon(a)!} class="size-[18px]" />
        </button>
      {/each}
    </div>
    {/if}

    <div class="flex h-full flex-1 flex-col">
      {#if railView.kind === 'harness'}
        <!-- header -->
        <div class="flex items-center gap-2 border-b border-border/60 px-3 py-2.5">
          <ProviderIcon name={agentIcon(railHarness)!} class="size-4 shrink-0 text-foreground" />
          <span class="text-sm font-semibold">{agentMeta[railHarness]}</span>
          <button
            type="button"
            aria-label={pinnedHarnesses.has(railHarness) ? 'Unpin harness' : 'Pin harness'}
            title={pinnedHarnesses.has(railHarness) ? 'Unpin harness' : 'Pin harness'}
            onclick={() => pickerFavorites.toggleHarness(railHarness)}
            class="ml-auto flex size-6 items-center justify-center rounded text-muted-foreground hover:bg-muted hover:text-foreground"
          >
            <Star
              class={[
                'size-4',
                pinnedHarnesses.has(railHarness) ? 'fill-yellow-400 text-yellow-400' : '',
              ]}
            />
          </button>
        </div>
        <!-- search + list -->
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
              {@const selected = railHarness === agent && m.id === model}
              {@const pinned = pinnedModels.has(`${railHarness}/${m.id}`)}
              <li>
                <button
                  type="button"
                  onclick={() => pickModel(railHarness, m)}
                  class={[
                    'group/row flex w-full cursor-pointer items-center gap-3 rounded-md px-2 py-2 text-left transition-colors',
                    selected ? 'bg-muted' : 'hover:bg-muted/60',
                  ]}
                >
                  <div class="flex min-w-0 flex-1 flex-col">
                    <span class="truncate text-sm font-medium text-foreground">{m.label}</span>
                    <span class="flex items-center gap-1.5 truncate text-xs text-muted-foreground">
                      <AgentIcon agent={railHarness} model={m.id} class="size-3 shrink-0" />
                      <span>{agentMeta[railHarness]}</span>
                    </span>
                  </div>
                  <span
                    role="button"
                    tabindex="0"
                    aria-label={pinned ? 'Unpin model' : 'Pin model'}
                    title={pinned ? 'Unpin model' : 'Pin model'}
                    onclick={(e) => {
                      e.stopPropagation();
                      pickerFavorites.toggleModel(railHarness, m.id);
                    }}
                    onkeydown={(e) => {
                      if (e.key === 'Enter' || e.key === ' ') {
                        e.preventDefault();
                        e.stopPropagation();
                        pickerFavorites.toggleModel(railHarness, m.id);
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
          {#if pickerFavorites.harnesses.length === 0 && pickerFavorites.models.length === 0}
            <p class="px-3 py-6 text-center text-xs text-muted-foreground">
              No favorites yet. Star a model in any harness to pin it here.
            </p>
          {:else}
            {#if pickerFavorites.harnesses.length > 0}
              <p class="px-2 pt-1 pb-1 text-[10px] font-semibold uppercase tracking-wide text-muted-foreground">Harnesses</p>
              <ul class="mb-2 flex flex-col gap-0.5">
                {#each pickerFavorites.harnesses as a (a)}
                  <li>
                    <button
                      type="button"
                      onclick={() => {
                        const def = defaultModelFor(catalog, a);
                        if (def) pickModel(a, def);
                      }}
                      class="flex w-full cursor-pointer items-center gap-2 rounded-md px-2 py-2 text-left transition-colors hover:bg-muted/60"
                    >
                      <ProviderIcon name={agentIcon(a)!} class="size-4 shrink-0 text-foreground" />
                      <span class="truncate text-sm font-medium">{agentMeta[a]}</span>
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
                  {@const info = catalogByName.get(f.agent)?.models.find((m) => m.id === f.model)}
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
                            <span>{agentMeta[f.agent]}</span>
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
      {:else if railView.kind === 'agents'}
        <div class="flex items-center gap-2 border-b border-border/60 px-3 py-2.5">
          <Bot class="size-4 shrink-0" />
          <span class="text-sm font-semibold">Agents</span>
        </div>
        <div class="flex-1 overflow-y-auto p-1">
          {#if agentsStore.error}
            <p class="m-2 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-xs text-destructive">
              {agentsStore.error}
            </p>
          {/if}
          {#if agentsStore.loading && agentsStore.list.length === 0}
            <p class="px-3 py-6 text-center text-xs text-muted-foreground">Loading…</p>
          {:else if agentsStore.list.length === 0}
            <p class="px-3 py-6 text-center text-xs text-muted-foreground">
              No agents yet. Drop a markdown file into <code class="rounded bg-muted px-1 font-mono">~/.roy/agents/</code>.
            </p>
          {:else}
            <ul class="flex flex-col gap-0.5">
              {#each agentsStore.list as a (a.name)}
                <li>
                  <button
                    type="button"
                    onclick={() => {
                      if (!KNOWN_HARNESSES.has(a.harness as Harness)) return;
                      const harness = a.harness as Harness;
                      const info =
                        (a.model
                          ? catalogByName.get(harness)?.models.find((m) => m.id === a.model)
                          : undefined) ?? defaultModelFor(catalog, harness);
                      if (!info) return;
                      pickModel(harness, info);
                      onPickAgent?.(a.name);
                    }}
                    class="flex w-full cursor-pointer flex-col items-start gap-1 rounded-md px-2 py-2 text-left transition-colors hover:bg-muted/60"
                  >
                    <div class="flex w-full items-center gap-2">
                      <span class="truncate text-sm font-semibold">{a.name}</span>
                      <span class="ml-auto rounded bg-muted px-1.5 py-0.5 text-[10px] font-mono uppercase text-muted-foreground">
                        {a.harness}
                      </span>
                    </div>
                    {#if a.description}
                      <p class="line-clamp-2 w-full whitespace-pre-wrap text-xs text-muted-foreground">
                        {a.description}
                      </p>
                    {/if}
                  </button>
                </li>
              {/each}
            </ul>
          {/if}
        </div>
      {/if}
    </div>
  </Popover.Content>
</Popover.Root>
