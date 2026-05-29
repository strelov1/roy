<script lang="ts">
  import { onMount } from 'svelte';
  import { Bot, Play, Plus, RefreshCw, Search } from '@lucide/svelte';
  import { Button } from '$lib/components/ui/button';
  import { Input } from '$lib/components/ui/input';
  import * as Dialog from '$lib/components/ui/dialog';
  import { agentsStore, sortAgents, type Agent } from './agents.svelte';
  import { authState } from './auth.svelte';
  import { app } from './state.svelte';
  import { harnessesConfig, defaultModelFor } from './harnesses-config.svelte';
  import ProviderIcon from './ProviderIcon.svelte';
  import { agentIcon } from './provider-icons';
  import { errMsg } from './utils';
  import type { Harness } from './wire';

  function teamName(id: string): string {
    const t = authState.user?.teams.find((x) => x.id === id);
    return t?.name ?? `team · ${id.slice(0, 8)}`;
  }

  let {
    onOpenSession,
    onCreateAgent,
  }: {
    onOpenSession?: (id: string) => void;
    /// Header `+` click — host navigates to `/` and pre-loads the
    /// roy-agent-builder skill into the composer so the user can talk
    /// the builder through making a new agent.
    onCreateAgent?: () => void;
  } = $props();

  let query = $state('');
  let selected = $state<Agent | null>(null);
  let running = $state<string | null>(null);

  onMount(() => {
    void agentsStore.load();
    harnessesConfig.refresh();
  });

  const filtered = $derived.by(() => {
    const q = query.trim().toLowerCase();
    const sorted = sortAgents(agentsStore.list);
    if (!q) return sorted;
    return sorted.filter(
      (a) =>
        a.name.toLowerCase().includes(q) ||
        a.description.toLowerCase().includes(q),
    );
  });

  async function run(a: Agent) {
    if (running) return;
    const resolved =
      a.model ??
      defaultModelFor(harnessesConfig.harnesses, a.harness)?.id;
    if (!resolved) {
      app.lastError = `Agent "${a.name}": harness "${a.harness}" not in the harness catalog.`;
      return;
    }
    running = a.name;
    try {
      const sessionId = await app.createSession({
        agent: a.harness as Harness,
        model: resolved,
        persona: { prompt: a.body, name: a.name },
      });
      selected = null;
      onOpenSession?.(sessionId);
    } catch (e) {
      app.lastError = errMsg(e);
    } finally {
      running = null;
    }
  }
</script>

<div class="flex h-full min-h-0 w-full flex-col bg-background">
  <header class="border-b border-border/40 bg-background/95 px-6 py-4 backdrop-blur">
    <div class="flex items-center justify-between gap-3">
      <div class="flex items-center gap-2.5">
        <Bot class="size-5 text-muted-foreground" />
        <div>
          <h1 class="text-lg font-semibold text-foreground">Agents</h1>
          <p class="text-xs text-muted-foreground">
            Personas stored as markdown files under
            <code class="mx-1 rounded bg-muted px-1.5 py-0.5 font-mono text-[11px]">~/.roy/agents/</code>.
            Click a card to inspect, hit Run to spawn a chat.
          </p>
        </div>
      </div>
      <div class="flex items-center gap-1">
        {#if onCreateAgent}
          <Button
            variant="ghost"
            size="icon"
            onclick={onCreateAgent}
            title="New agent — opens chat with roy-agent-builder"
            aria-label="Create a new agent"
          >
            <Plus class="size-4" />
          </Button>
        {/if}
        <Button
          variant="ghost"
          size="icon"
          onclick={() => void agentsStore.load(true)}
          disabled={agentsStore.loading}
          title="Refresh"
          aria-label="Refresh agents list"
        >
          <RefreshCw class={['size-4', agentsStore.loading ? 'animate-spin' : '']} />
        </Button>
      </div>
    </div>
    <div class="mt-4 flex items-center gap-2">
      <div class="relative w-full max-w-md">
        <Search class="absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
        <Input
          bind:value={query}
          placeholder="Search agents"
          class="h-9 pl-8 text-sm"
          autocomplete="off"
        />
      </div>
      <span class="text-xs text-muted-foreground">
        {filtered.length} of {agentsStore.list.length}
      </span>
    </div>
  </header>

  <div class="flex-1 overflow-y-auto px-6 py-6">
    {#if agentsStore.loading && agentsStore.list.length === 0}
      <p class="text-sm text-muted-foreground">Loading…</p>
    {:else if agentsStore.error}
      <p class="text-sm text-destructive">Couldn't load: {agentsStore.error}</p>
    {:else if agentsStore.list.length === 0}
      <div class="rounded-lg border border-dashed border-border/60 p-8 text-center">
        <Bot class="mx-auto mb-3 size-8 text-muted-foreground/60" />
        <p class="text-sm text-muted-foreground">
          No agents yet. Drop a markdown file into
          <code class="rounded bg-muted px-1 font-mono">~/.roy/agents/&lt;name&gt;.md</code>
          to populate this catalog.
        </p>
      </div>
    {:else if filtered.length === 0}
      <p class="text-sm text-muted-foreground">No agents match "{query}".</p>
    {:else}
      <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
        {#each filtered as a (a.name)}
          <button
            type="button"
            onclick={() => (selected = a)}
            class="flex h-full flex-col gap-2 rounded-lg border border-border/60 bg-card px-4 py-3 text-left transition-colors hover:border-border hover:bg-accent/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40"
          >
            <header class="flex items-baseline justify-between gap-2">
              <h2 class="truncate font-mono text-sm text-foreground">{a.name}</h2>
              <div class="flex shrink-0 items-center gap-1.5">
                <span class="rounded-full bg-muted/60 px-2 py-0.5 text-[10px] uppercase tracking-wider text-muted-foreground">
                  {#if a.scope.kind === 'builtin'}
                    roy
                  {:else if a.scope.kind === 'personal'}
                    personal
                  {:else}
                    team · {teamName(a.scope.team_id)}
                  {/if}
                </span>
                <span class="flex items-center gap-1 rounded-full bg-muted px-2 py-0.5 text-[10px] uppercase tracking-wider text-muted-foreground">
                  <ProviderIcon name={agentIcon(a.harness)!} class="size-3" />
                  {a.harness}
                </span>
              </div>
            </header>
            {#if a.description}
              <p class="line-clamp-3 text-xs text-muted-foreground">{a.description}</p>
            {/if}
            {#if a.model}
              <code class="truncate text-[10px] text-muted-foreground/80">{a.model}</code>
            {/if}
          </button>
        {/each}
      </div>
    {/if}
  </div>
</div>

<Dialog.Root open={selected !== null} onOpenChange={(o) => (o ? null : (selected = null))}>
  <Dialog.Content class="flex h-[92vh] w-[min(76rem,96vw)] max-w-none flex-col overflow-hidden p-0 sm:max-w-none">
    {#if selected}
      {@const sel = selected}
      <Dialog.Header class="shrink-0 border-b border-border/40 px-6 py-4">
        <Dialog.Title class="flex items-center justify-between gap-3 pr-8">
          <span class="break-all font-mono text-base text-foreground">{sel.name}</span>
          <div class="flex shrink-0 items-center gap-1.5">
            <span class="rounded-full bg-muted/60 px-2 py-0.5 text-[10px] uppercase tracking-wider text-muted-foreground">
              {#if sel.scope.kind === 'builtin'}
                roy
              {:else if sel.scope.kind === 'personal'}
                personal
              {:else}
                team · {teamName(sel.scope.team_id)}
              {/if}
            </span>
            <span class="flex items-center gap-1 rounded-full bg-muted px-2 py-0.5 text-[10px] uppercase tracking-wider text-muted-foreground">
              <ProviderIcon name={agentIcon(sel.harness)!} class="size-3" />
              {sel.harness}{sel.model ? ` · ${sel.model}` : ''}
            </span>
          </div>
        </Dialog.Title>
        {#if sel.description}
          <Dialog.Description class="break-words text-xs leading-relaxed">
            {sel.description}
          </Dialog.Description>
        {/if}
      </Dialog.Header>

      <div class="min-h-0 flex-1 overflow-y-auto px-6 py-4">
        <pre
          class="whitespace-pre-wrap break-words font-mono text-xs leading-relaxed text-foreground">{sel.body}</pre>
      </div>

      <div class="shrink-0 border-t border-border/40 px-6 py-3">
        <Button
          onclick={() => void run(sel)}
          disabled={running !== null}
          class="ml-auto flex"
        >
          <Play class="size-4" />
          {running === sel.name ? 'Spawning…' : 'Run'}
        </Button>
      </div>
    {/if}
  </Dialog.Content>
</Dialog.Root>
