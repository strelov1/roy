<script lang="ts">
  import { onMount } from 'svelte';
  import { Plug, RefreshCw, Search, Trash2, Plus } from '@lucide/svelte';
  import { Button } from '$lib/components/ui/button';
  import { Input } from '$lib/components/ui/input';
  import { connectionsStore } from './connections.svelte';
  import { providersStore } from './providers.svelte';
  import ConnectDialog from './ConnectDialog.svelte';
  import CustomConnectionDialog from './CustomConnectionDialog.svelte';
  import ProviderIcon from './ProviderIcon.svelte';
  import type { Provider } from './providers.svelte';
  import type { Connection } from './connections.svelte';

  let query = $state('');
  let selectedId = $state<string | null>(null);
  let dialogOpen = $state(false);
  let customDialogOpen = $state(false);

  onMount(() => {
    void connectionsStore.load();
    void providersStore.load();
  });

  // Group user's connections by provider_id so the left pane can render
  // "Connected" entries one-per-provider, each expandable to instances.
  const grouped = $derived.by(() => {
    const map = new Map<string, Connection[]>();
    for (const c of connectionsStore.list) {
      if (!c.provider_id) continue;
      const arr = map.get(c.provider_id) ?? [];
      arr.push(c);
      map.set(c.provider_id, arr);
    }
    return map;
  });

  const connectedProviders = $derived(
    providersStore.list.filter((p) => grouped.has(p.id)),
  );
  const availableProviders = $derived(
    providersStore.list.filter((p) => !grouped.has(p.id)),
  );

  function filterByQuery(arr: Provider[], q: string): Provider[] {
    const norm = q.trim().toLowerCase();
    if (!norm) return arr;
    return arr.filter(
      (p) =>
        p.name.toLowerCase().includes(norm) ||
        p.description.toLowerCase().includes(norm),
    );
  }

  const filteredConnected = $derived(filterByQuery(connectedProviders, query));
  const filteredAvailable = $derived(filterByQuery(availableProviders, query));

  const selected = $derived(
    selectedId
      ? providersStore.list.find((p) => p.id === selectedId) ?? null
      : null,
  );

  async function disconnect(c: Connection) {
    try {
      await connectionsStore.remove(c.id);
    } catch (e) {
      console.error('disconnect failed', e);
    }
  }
</script>

<div class="flex h-full min-h-0 w-full">
  <!-- Left pane: list -->
  <aside
    class="flex w-72 shrink-0 flex-col border-r border-border/40 bg-background/95"
  >
    <header class="border-b border-border/40 px-4 py-3">
      <div class="mb-3 flex items-center justify-between gap-2">
        <h1 class="flex items-center gap-2 text-sm font-semibold">
          <Plug class="size-4 text-muted-foreground" /> Connections
        </h1>
        <div class="flex items-center gap-1">
          <Button
            variant="ghost"
            size="icon"
            onclick={() => (customDialogOpen = true)}
            aria-label="Add custom MCP server"
            title="Add custom MCP server"
          >
            <Plus class="size-3.5" />
          </Button>
          <Button
            variant="ghost"
            size="icon"
            onclick={() => {
              void connectionsStore.load(true);
              void providersStore.load(true);
            }}
            aria-label="Refresh"
          >
            <RefreshCw
              class={[
                'size-3.5',
                connectionsStore.loading || providersStore.loading
                  ? 'animate-spin'
                  : '',
              ]}
            />
          </Button>
        </div>
      </div>
      <div class="relative">
        <Search
          class="absolute left-2 top-1/2 size-3 -translate-y-1/2 text-muted-foreground"
        />
        <Input
          bind:value={query}
          placeholder="Search"
          class="h-8 pl-7 text-sm"
          autocomplete="off"
        />
      </div>
    </header>

    <div class="flex-1 space-y-4 overflow-y-auto p-2">
      {#if filteredConnected.length > 0}
        <section>
          <h2
            class="mb-1 px-2 text-[10px] uppercase tracking-wider text-muted-foreground"
          >
            Connected
          </h2>
          {#each filteredConnected as p (p.id)}
            <button
              type="button"
              onclick={() => (selectedId = p.id)}
              class={[
                'flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm hover:bg-accent/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40',
                selectedId === p.id ? 'bg-accent/60' : '',
              ]}
            >
              <ProviderIcon name={p.icon} class="size-4" />
              <span class="flex-1 truncate">{p.name}</span>
              <span class="text-[10px] text-muted-foreground">
                {grouped.get(p.id)!.length}
              </span>
            </button>
          {/each}
        </section>
      {/if}

      {#if filteredAvailable.length > 0}
        <section>
          <h2
            class="mb-1 px-2 text-[10px] uppercase tracking-wider text-muted-foreground"
          >
            Available
          </h2>
          {#each filteredAvailable as p (p.id)}
            <button
              type="button"
              onclick={() => (selectedId = p.id)}
              class={[
                'flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm text-muted-foreground hover:bg-accent/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40',
                selectedId === p.id ? 'bg-accent/60' : '',
              ]}
            >
              <ProviderIcon name={p.icon} class="size-4" />
              <span class="flex-1 truncate">{p.name}</span>
            </button>
          {/each}
        </section>
      {/if}

      {#if providersStore.list.length === 0 && !providersStore.loading}
        <p class="px-2 text-xs text-muted-foreground">
          Catalog is empty. Edit
          <code class="rounded bg-muted px-1 font-mono">~/.roy/connections.yaml</code>
          to add providers.
        </p>
      {/if}
    </div>
  </aside>

  <!-- Right pane: details -->
  <main class="flex-1 overflow-y-auto">
    {#if selected}
      {@const instances = grouped.get(selected.id) ?? []}
      <div class="max-w-2xl space-y-6 px-8 py-6">
        <header class="flex items-start gap-4">
          <ProviderIcon name={selected.icon} class="size-10" />
          <div class="min-w-0 flex-1">
            <h2 class="text-xl font-semibold">{selected.name}</h2>
            <p class="mt-1 text-sm text-muted-foreground">{selected.description}</p>
          </div>
          <Button onclick={() => (dialogOpen = true)}>
            <Plus class="size-4" />
            {instances.length === 0 ? 'Connect' : 'Connect another'}
          </Button>
        </header>

        {#if instances.length > 0}
          <section>
            <h3 class="mb-2 text-sm font-medium">Connected instances</h3>
            <div class="space-y-2">
              {#each instances as c (c.id)}
                <div
                  class="flex items-center gap-3 rounded-md border border-border/40 px-4 py-2.5"
                >
                  <div class="min-w-0 flex-1">
                    <p class="font-mono text-sm">{c.name}</p>
                    <p class="text-[11px] text-muted-foreground">
                      Created {new Date(c.created_at * 1000).toLocaleDateString()}
                    </p>
                  </div>
                  <Button
                    variant="ghost"
                    size="icon"
                    onclick={() => void disconnect(c)}
                    aria-label="Disconnect"
                    class="text-destructive hover:bg-destructive/10"
                  >
                    <Trash2 class="size-4" />
                  </Button>
                </div>
              {/each}
            </div>
          </section>
        {/if}
      </div>

      <ConnectDialog provider={selected} bind:open={dialogOpen} />
    {:else}
      <div
        class="flex h-full items-center justify-center text-sm text-muted-foreground"
      >
        Select a provider from the list.
      </div>
    {/if}
  </main>

  <CustomConnectionDialog bind:open={customDialogOpen} />
</div>
