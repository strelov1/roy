<script lang="ts">
  import { app } from '../state.svelte';
  import { Bot, Archive, Trash2 } from '@lucide/svelte';
  import AppMark from '../AppMark.svelte';
  import SessionRowShell from './SessionRowShell.svelte';
  import { Badge } from '$lib/components/ui/badge';
  import type { SessionInfo } from '../wire';

  let {
    sessions,
    onPickSession,
    onArchive,
    onDelete,
  }: {
    sessions: SessionInfo[];
    onPickSession: (id: string) => void;
    /** Soft-close a live session (keeps the journal on disk). */
    onArchive: (id: string) => void;
    /** Hard-delete an already-archived session (opens a confirmation). */
    onDelete: (id: string) => void;
  } = $props();

  function bgKind(s: SessionInfo): 'scheduled' | 'ad-hoc' {
    return s.tags?.['roy-scheduler:kind'] === 'background_fire'
      ? 'scheduled'
      : 'ad-hoc';
  }

  function agentId(s: SessionInfo): string | undefined {
    return s.tags?.['roy-scheduler:agent_id'];
  }
</script>

<ul class="flex flex-col">
  {#each sessions as s (s.session)}
    {@const active = app.currentSession === s.session}
    {@const working = app.activeSessions[s.session] === true}
    {@const kind = bgKind(s)}
    {@const aid = agentId(s)}
    {@const archived = app.isArchived(s.session)}
    <li>
      <SessionRowShell
        session={s.session}
        onPick={onPickSession}
        active={active}
        italic={archived}
        overlayPadding="pr-20"
      >
        {#snippet icon()}
          {#if working}
            <AppMark animate class="w-4 shrink-0 text-foreground" />
          {:else}
            <Bot class={['size-3.5 shrink-0 text-muted-foreground', archived && 'opacity-70']} />
          {/if}
        {/snippet}
        {#snippet actions()}
          <Badge
            variant={kind === 'scheduled' ? 'secondary' : 'outline'}
            class="shrink-0 px-1.5 py-0 text-[0.6rem] uppercase tracking-wide"
            title={aid ? `agent ${aid}` : kind}
          >
            {kind}
          </Badge>
          <button
            type="button"
            aria-label={archived ? 'Delete session' : 'Archive session'}
            title={archived ? 'Delete (wipe from disk)' : 'Archive (close + keep journal)'}
            onclick={() => (archived ? onDelete : onArchive)(s.session)}
            class={[
              'flex size-6 shrink-0 items-center justify-center rounded text-muted-foreground opacity-0 transition-opacity group-hover/row:opacity-100 focus-visible:opacity-100',
              archived
                ? 'hover:bg-destructive/15 hover:text-destructive'
                : 'hover:bg-foreground/10 hover:text-foreground',
            ]}
          >
            {#if archived}
              <Trash2 class="size-3.5" />
            {:else}
              <Archive class="size-3.5" />
            {/if}
          </button>
        {/snippet}
      </SessionRowShell>
    </li>
  {/each}
</ul>
