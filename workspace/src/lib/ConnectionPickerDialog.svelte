<script lang="ts">
  import { onMount } from 'svelte';
  import { Plug, Settings } from '@lucide/svelte';
  import * as Dialog from '$lib/components/ui/dialog';
  import { connectionsStore, type Connection } from './connections.svelte';
  import { providersStore } from './providers.svelte';
  import ProviderIcon from './ProviderIcon.svelte';

  let {
    selected = $bindable<string[]>([]),
    open = $bindable(false),
    onManage,
  }: {
    selected?: string[];
    open?: boolean;
    onManage?: () => void;
  } = $props();

  onMount(() => {
    void connectionsStore.load();
    void providersStore.load();
  });

  function displayLabel(c: Connection): string {
    if (c.provider_id) {
      const p = providersStore.get(c.provider_id);
      if (p) return `${p.name} · ${c.name}`;
    }
    return c.name;
  }

  // Trim selections that no longer point at a real connection — covers the
  // race where the user deleted a connection in another tab and the cached
  // selection still references it. Runs on every list change.
  $effect(() => {
    const valid = new Set(connectionsStore.list.map((c) => c.id));
    const filtered = selected.filter((id) => valid.has(id));
    if (filtered.length !== selected.length) {
      selected = filtered;
    }
  });

  function toggle(id: string) {
    if (selected.includes(id)) {
      selected = selected.filter((x) => x !== id);
    } else {
      selected = [...selected, id];
    }
  }
</script>

<Dialog.Root bind:open>
  <Dialog.Content
    class="flex h-[24rem] max-h-[80vh] w-[22rem] max-w-[90vw] flex-col overflow-hidden p-0"
  >
    <Dialog.Header class="border-b border-border/60 px-3 py-2.5">
      <Dialog.Title class="flex items-center gap-2 text-sm font-semibold">
        <Plug class="size-4 shrink-0 text-foreground" />
        Connections
        <span class="ml-auto mr-7 text-[10px] font-normal text-muted-foreground">
          {connectionsStore.list.length} available
        </span>
      </Dialog.Title>
    </Dialog.Header>

    <div class="flex-1 overflow-y-auto p-1">
      {#if connectionsStore.error}
        <p
          class="m-2 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-xs text-destructive"
        >
          {connectionsStore.error}
        </p>
      {/if}
      {#if connectionsStore.loading && connectionsStore.list.length === 0}
        <p class="px-3 py-6 text-center text-xs text-muted-foreground">Loading…</p>
      {:else if connectionsStore.list.length === 0}
        <div class="px-3 py-6 text-center">
          <p class="mb-2 text-xs text-muted-foreground">No connections yet.</p>
          {#if onManage}
            <button
              type="button"
              onclick={() => {
                open = false;
                onManage?.();
              }}
              class="text-xs font-medium text-foreground underline-offset-2 hover:underline"
            >
              Manage…
            </button>
          {/if}
        </div>
      {:else}
        <ul class="flex flex-col gap-0.5">
          {#each connectionsStore.list as c (c.id)}
            {@const checked = selected.includes(c.id)}
            <li>
              <button
                type="button"
                onclick={() => toggle(c.id)}
                class={[
                  'flex w-full cursor-pointer items-center gap-3 rounded-md px-2 py-2 text-left transition-colors',
                  checked ? 'bg-muted' : 'hover:bg-muted/60',
                ]}
              >
                <input
                  type="checkbox"
                  {checked}
                  tabindex={-1}
                  aria-hidden="true"
                  class="size-3.5 shrink-0 rounded border-border accent-foreground"
                  onclick={(e) => e.preventDefault()}
                />
                {#if c.provider_id}
                  {@const provider = providersStore.get(c.provider_id)}
                  {#if provider}
                    <ProviderIcon
                      name={provider.icon}
                      class="size-4 shrink-0 text-foreground"
                    />
                  {/if}
                {/if}
                <div class="flex min-w-0 flex-1 flex-col">
                  <span class="truncate text-sm font-medium text-foreground"
                    >{displayLabel(c)}</span
                  >
                  {#if c.description}
                    <span class="truncate text-[11px] text-muted-foreground">
                      {c.description}
                    </span>
                  {/if}
                </div>
                <span
                  class="shrink-0 rounded-full bg-muted px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wider text-muted-foreground"
                >
                  {c.slug}
                </span>
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    </div>

    {#if onManage && connectionsStore.list.length > 0}
      <div class="border-t border-border/60 px-2 py-1.5">
        <button
          type="button"
          onclick={() => {
            open = false;
            onManage?.();
          }}
          class="flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-xs text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
        >
          <Settings class="size-3.5" />
          Manage connections…
        </button>
      </div>
    {/if}
  </Dialog.Content>
</Dialog.Root>
