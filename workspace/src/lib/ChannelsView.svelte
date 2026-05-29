<script lang="ts">
  import { onMount } from 'svelte';
  import { MessageCircle, RefreshCw, Trash2, Plus, PanelLeft } from '@lucide/svelte';
  import { Button } from '$lib/components/ui/button';
  import { channelsStore } from './channels.svelte';
  import { connectionsStore } from './connections.svelte';
  import AddChannelDialog from './AddChannelDialog.svelte';
  import { app } from './state.svelte';
  import { errMsg } from './utils';
  import type { ChannelBinding } from './channels.svelte';

  let { onOpenSidebar }: { onOpenSidebar?: () => void } = $props();

  let dialogOpen = $state(false);

  onMount(() => {
    void channelsStore.load();
    void connectionsStore.load();
  });

  // connection_id → channel name, for display.
  const nameById = $derived.by(() => {
    const m = new Map<string, string>();
    for (const c of connectionsStore.list) {
      if (c.kind === 'telegram_bot') m.set(c.id, c.name);
    }
    return m;
  });

  const strategyLabel: Record<string, string> = {
    per_sender_sticky: 'Per sender',
    persistent_one: 'One session',
    ephemeral: 'Ephemeral',
  };

  async function toggle(b: ChannelBinding) {
    try {
      await channelsStore.setEnabled(b.id, !b.enabled);
    } catch (e) {
      app.lastError = errMsg(e);
    }
  }

  async function remove(b: ChannelBinding) {
    try {
      await channelsStore.removeChannel(b);
    } catch (e) {
      app.lastError = errMsg(e);
    }
  }
</script>

<div class="flex h-full min-h-0 w-full flex-col">
  <header class="flex items-center gap-2 border-b border-border/40 px-4 py-3 md:px-8">
    <Button
      variant="ghost"
      size="icon"
      class="md:hidden"
      onclick={() => onOpenSidebar?.()}
      aria-label="Show sidebar"
    >
      <PanelLeft class="size-4" />
    </Button>
    <h1 class="flex items-center gap-2 text-sm font-semibold">
      <MessageCircle class="size-4 text-muted-foreground" /> Channels
    </h1>
    <div class="ml-auto flex items-center gap-1">
      <Button
        variant="ghost"
        size="icon"
        onclick={() => void channelsStore.load(true)}
        aria-label="Refresh"
      >
        <RefreshCw class={['size-3.5', channelsStore.loading ? 'animate-spin' : '']} />
      </Button>
      <Button onclick={() => (dialogOpen = true)}>
        <Plus class="size-4" /> Add channel
      </Button>
    </div>
  </header>

  <div class="flex-1 overflow-y-auto px-4 py-6 md:px-8">
    <div class="mx-auto max-w-2xl space-y-3">
      {#if channelsStore.error}
        <p class="text-sm text-destructive">{channelsStore.error}</p>
      {:else if channelsStore.list.length === 0 && channelsStore.loaded}
        <p class="text-sm text-muted-foreground">
          No channels yet. Click "Add channel" to connect one.
        </p>
      {:else}
        {#each channelsStore.list as b (b.id)}
          <div class="flex items-center gap-3 rounded-md border border-border/40 px-4 py-3">
            <MessageCircle class="size-4 shrink-0 text-muted-foreground" />
            <div class="min-w-0 flex-1">
              <p class="truncate text-sm font-medium">
                {nameById.get(b.connection_id) ?? 'Channel'}
              </p>
              <p class="text-[11px] text-muted-foreground">
                {b.channel_kind} · {b.agent_slug} · {strategyLabel[b.session_strategy] ?? b.session_strategy}
                {#if b.allowed_user_ids.length > 0}· {b.allowed_user_ids.length} allowed{/if}
              </p>
            </div>
            <button
              type="button"
              onclick={() => void toggle(b)}
              aria-pressed={b.enabled}
              title={b.enabled ? 'Enabled — click to disable' : 'Disabled — click to enable'}
              class={[
                'rounded-full px-2.5 py-1 text-[11px] font-medium transition-colors',
                b.enabled
                  ? 'bg-primary/15 text-primary hover:bg-primary/25'
                  : 'bg-muted text-muted-foreground hover:bg-muted/70',
              ]}
            >
              {b.enabled ? 'Enabled' : 'Disabled'}
            </button>
            <Button
              variant="ghost"
              size="icon"
              onclick={() => void remove(b)}
              aria-label="Delete channel"
              class="text-destructive hover:bg-destructive/10"
            >
              <Trash2 class="size-4" />
            </Button>
          </div>
        {/each}
      {/if}
    </div>
  </div>
</div>

<AddChannelDialog bind:open={dialogOpen} />
