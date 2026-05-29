<script lang="ts">
  import { app } from './state.svelte';
  import Composer from './Composer.svelte';
  import AppMark from './AppMark.svelte';
  import { Archive, Folder, Inbox, PanelLeft } from '@lucide/svelte';
  import { Button } from '$lib/components/ui/button';
  import { ScrollArea } from '$lib/components/ui/scroll-area';
  import SessionRowShell from './components/SessionRowShell.svelte';
  import SpawningRow from './components/SpawningRow.svelte';

  let {
    projectId,
    onCreated,
    onPickSession,
    onOpenSidebar,
    onOpenConnections,
  }: {
    projectId: string;
    onCreated: (sessionId: string) => void;
    onPickSession: (id: string) => void;
    onOpenSidebar?: () => void;
    onOpenConnections?: () => void;
  } = $props();

  let project = $derived(app.projects.find((p) => p.id === projectId));
  let liveSessions = $derived(
    app.regularLive.filter((s) => s.project_id === projectId),
  );
  let archivedSessions = $derived(
    app.regularArchived.filter((s) => s.project_id === projectId),
  );
  let spawningHere = $derived(app.spawningSession?.projectId === projectId);
</script>

<div class="flex h-full flex-col overflow-hidden bg-background">
  <div class="flex items-center gap-2 border-b border-border/40 px-4 py-3 md:px-6">
    {#if onOpenSidebar}
      <Button
        variant="ghost"
        size="icon"
        onclick={onOpenSidebar}
        aria-label="Show sidebar"
        title="Show sidebar"
        class="text-muted-foreground md:hidden"
      >
        <PanelLeft />
      </Button>
    {/if}
    <Folder class="size-5 text-muted-foreground" />
    <h1 class="truncate text-base font-medium">{project?.name ?? '…'}</h1>
  </div>

  <ScrollArea class="min-h-0 flex-1">
    <div class="mx-auto w-full max-w-3xl space-y-8 px-4 py-8 md:px-6">
      <Composer onCreated={onCreated} lockedProjectId={projectId} {onOpenConnections} />

      <section class="space-y-2">
        <h2 class="px-1 text-xs font-medium uppercase tracking-wide text-muted-foreground/80">
          Chats
        </h2>
        {#if liveSessions.length === 0 && archivedSessions.length === 0 && !spawningHere}
          <p class="px-1 text-sm text-muted-foreground/70">No sessions in this project yet.</p>
        {:else}
          <ul class="flex flex-col">
            {#if spawningHere}
              <li><SpawningRow /></li>
            {/if}
            {#each liveSessions as s (s.session)}
              {@const working = app.activeSessions[s.session] === true}
              <li>
                <SessionRowShell session={s.session} onPick={onPickSession}>
                  {#snippet icon()}
                    {#if working}
                      <AppMark animate class="w-4 shrink-0 text-foreground" />
                    {:else}
                      <Inbox class="size-4 shrink-0 text-muted-foreground" />
                    {/if}
                  {/snippet}
                </SessionRowShell>
              </li>
            {/each}
            {#each archivedSessions as s (s.session)}
              <li>
                <SessionRowShell
                  session={s.session}
                  onPick={onPickSession}
                  italic
                  title="Click to resume"
                >
                  {#snippet icon()}
                    <Archive class="size-4 shrink-0 opacity-70" />
                  {/snippet}
                </SessionRowShell>
              </li>
            {/each}
          </ul>
        {/if}
      </section>
    </div>
  </ScrollArea>
</div>
