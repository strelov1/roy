<script lang="ts">
  import { onMount } from 'svelte';
  import { Zap, Search, Plus, RefreshCw } from '@lucide/svelte';
  import { Button } from '$lib/components/ui/button';
  import { Input } from '$lib/components/ui/input';
  import * as Dialog from '$lib/components/ui/dialog';
  import { commandsStore, fetchCommandBody, type CommandInfo } from './commands.svelte';

  let {
    onCreateSkill,
  }: {
    /// Header `+` click — host navigates to `/` and pre-loads the
    /// roy-skill-builder skill into the composer so the user can talk
    /// the builder through making a new skill.
    onCreateSkill?: () => void;
  } = $props();

  let query = $state('');

  onMount(() => {
    void commandsStore.load();
  });

  const filtered = $derived.by(() => {
    const q = query.trim().toLowerCase();
    if (!q) return commandsStore.list;
    return commandsStore.list.filter(
      (c) => c.name.toLowerCase().includes(q) || c.description.toLowerCase().includes(q),
    );
  });

  // Detail panel state. `selected` is null until a card is clicked; `body` is
  // null while the fetch is in flight, then either the markdown text or the
  // sentinel '<not-found>' so we can show a fallback without conflating it
  // with the loading state.
  let selected = $state<CommandInfo | null>(null);
  let body = $state<string | null>(null);
  let bodyError = $state<string | null>(null);

  async function open(skill: CommandInfo) {
    selected = skill;
    body = null;
    bodyError = null;
    const text = await fetchCommandBody(skill.name);
    // Bail if the user already clicked another card while we were waiting —
    // assigning a stale body to the new selection would flash wrong content.
    if (selected?.name !== skill.name) return;
    if (text === null) {
      bodyError = 'Body not available — the file may have been deleted.';
    } else {
      body = text;
    }
  }

  function close() {
    selected = null;
    body = null;
    bodyError = null;
  }
</script>

<div class="flex h-full min-h-0 w-full flex-col bg-background">
  <header class="border-b border-border/40 bg-background/95 px-6 py-4 backdrop-blur">
    <div class="flex items-center justify-between gap-3">
      <div class="flex items-center gap-2.5">
        <Zap class="size-5 text-muted-foreground" />
        <div>
          <h1 class="text-lg font-semibold text-foreground">Skills</h1>
          <p class="text-xs text-muted-foreground">
            Markdown playbooks roy injects into your prompt when you call them with
            <span class="mx-1 rounded bg-muted px-1.5 py-0.5 font-mono text-[11px]">/</span>
            in chat.
          </p>
        </div>
      </div>
      <div class="flex items-center gap-1">
        {#if onCreateSkill}
          <Button
            variant="ghost"
            size="icon"
            onclick={onCreateSkill}
            title="New skill — opens chat with roy-skill-builder"
            aria-label="Create a new skill"
          >
            <Plus class="size-4" />
          </Button>
        {/if}
        <Button
          variant="ghost"
          size="icon"
          onclick={() => void commandsStore.load(true)}
          disabled={commandsStore.loading}
          title="Refresh"
          aria-label="Refresh skills list"
        >
          <RefreshCw class={['size-4', commandsStore.loading ? 'animate-spin' : '']} />
        </Button>
      </div>
    </div>

    <div class="mt-4 flex items-center gap-2">
      <div class="relative w-full max-w-md">
        <Search class="absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
        <Input
          bind:value={query}
          placeholder="Search skills"
          class="h-9 pl-8 text-sm"
          autocomplete="off"
        />
      </div>
      <span class="text-xs text-muted-foreground">
        {filtered.length} of {commandsStore.list.length}
      </span>
    </div>
  </header>

  <div class="flex-1 overflow-y-auto px-6 py-6">
    {#if commandsStore.loading && commandsStore.list.length === 0}
      <p class="text-sm text-muted-foreground">Loading…</p>
    {:else if commandsStore.error}
      <p class="text-sm text-destructive">Couldn't load: {commandsStore.error}</p>
    {:else if commandsStore.list.length === 0}
      <div class="rounded-lg border border-dashed border-border/60 p-8 text-center">
        <Zap class="mx-auto mb-3 size-8 text-muted-foreground/60" />
        <p class="text-sm text-muted-foreground">
          No skills yet. Drop a <code class="rounded bg-muted px-1 font-mono">SKILL.md</code> into
          <code class="rounded bg-muted px-1 font-mono">~/.roy/skills/&lt;name&gt;/</code>
          to populate this catalog.
        </p>
      </div>
    {:else if filtered.length === 0}
      <p class="text-sm text-muted-foreground">No skills match “{query}”.</p>
    {:else}
      <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
        {#each filtered as cmd (cmd.name + cmd.source)}
          <button
            type="button"
            onclick={() => void open(cmd)}
            class="flex h-full flex-col gap-2 rounded-lg border border-border/60 bg-card px-4 py-3 text-left transition-colors hover:border-border hover:bg-accent/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40"
          >
            <header class="flex items-baseline justify-between gap-2">
              <h2 class="truncate font-mono text-sm text-foreground">/{cmd.name}</h2>
              <span
                class="shrink-0 rounded-full bg-muted px-2 py-0.5 text-[10px] uppercase tracking-wider text-muted-foreground"
              >
                {cmd.source}
              </span>
            </header>
            {#if cmd.description}
              <p class="line-clamp-4 text-xs text-muted-foreground">{cmd.description}</p>
            {/if}
          </button>
        {/each}
      </div>
    {/if}
  </div>
</div>

<Dialog.Root open={selected !== null} onOpenChange={(o) => (o ? null : close())}>
  <Dialog.Content class="flex h-[92vh] w-[min(76rem,96vw)] max-w-none flex-col overflow-hidden p-0 sm:max-w-none">
    {#if selected}
      <Dialog.Header class="shrink-0 border-b border-border/40 px-6 py-4">
        <Dialog.Title class="flex items-center justify-between gap-3 pr-8">
          <span class="break-all font-mono text-base text-foreground">/{selected.name}</span>
          <span
            class="shrink-0 rounded-full bg-muted px-2 py-0.5 text-[10px] uppercase tracking-wider text-muted-foreground"
          >
            {selected.source}
          </span>
        </Dialog.Title>
        {#if selected.description}
          <Dialog.Description class="break-words text-xs leading-relaxed">
            {selected.description}
          </Dialog.Description>
        {/if}
      </Dialog.Header>

      <div class="min-h-0 flex-1 overflow-y-auto px-6 py-4">
        {#if body === null && bodyError === null}
          <p class="text-sm text-muted-foreground">Loading body…</p>
        {:else if bodyError}
          <p class="text-sm text-destructive">{bodyError}</p>
        {:else if body}
          <pre
            class="whitespace-pre-wrap break-words font-mono text-xs leading-relaxed text-foreground">{body}</pre>
        {/if}
      </div>
    {/if}
  </Dialog.Content>
</Dialog.Root>
