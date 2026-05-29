<script lang="ts">
  import { app } from '../state.svelte';
  import { Archive, Loader2, RotateCcw, Trash2 } from '@lucide/svelte';
  import SessionRowShell from './SessionRowShell.svelte';

  let {
    session,
    onPick,
    onResume,
    onDelete,
  }: {
    session: string;
    onPick: (id: string) => void;
    onResume: (id: string) => void;
    onDelete: (id: string) => void;
  } = $props();

  let active = $derived(app.currentSession === session);
  let resuming = $derived(app.resumingSession === session);
</script>

<SessionRowShell
  {session}
  onPick={onPick}
  active={active}
  italic
  title="Click to resume"
  overlayPadding="pr-14"
>
  {#snippet icon()}
    {#if resuming}
      <Loader2 class="size-3.5 shrink-0 animate-spin text-foreground/80" />
    {:else}
      <Archive class="size-3.5 shrink-0 opacity-70" />
    {/if}
  {/snippet}
  {#snippet actions()}
    <button
      type="button"
      aria-label={resuming ? 'Resuming session…' : 'Resume session'}
      title={resuming ? 'Resuming…' : 'Resume (re-attach the agent)'}
      onclick={() => onResume(session)}
      disabled={resuming}
      class={[
        'flex size-6 items-center justify-center rounded text-muted-foreground transition-opacity hover:bg-foreground/10 hover:text-foreground focus-visible:opacity-100 disabled:cursor-default disabled:opacity-100 disabled:hover:bg-transparent disabled:hover:text-muted-foreground',
        resuming ? 'opacity-100' : 'opacity-0 group-hover/row:opacity-100',
      ]}
    >
      {#if resuming}
        <Loader2 class="size-3.5 animate-spin" />
      {:else}
        <RotateCcw class="size-3.5" />
      {/if}
    </button>
    <button
      type="button"
      aria-label="Delete session"
      title="Delete (wipe from disk)"
      onclick={() => onDelete(session)}
      disabled={resuming}
      class="flex size-6 items-center justify-center rounded text-muted-foreground opacity-0 transition-opacity hover:bg-destructive/15 hover:text-destructive group-hover/row:opacity-100 focus-visible:opacity-100 disabled:cursor-default disabled:opacity-30"
    >
      <Trash2 class="size-3.5" />
    </button>
  {/snippet}
</SessionRowShell>
