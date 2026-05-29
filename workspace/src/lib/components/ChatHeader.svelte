<script lang="ts">
  import { app } from '../state.svelte';
  import { Archive, ArrowUpFromLine, Loader2, MoreHorizontal, PanelLeft } from '@lucide/svelte';
  import { Button } from '$lib/components/ui/button';
  import * as DropdownMenu from '$lib/components/ui/dropdown-menu';
  import AppMark from '../AppMark.svelte';
  import AgentIcon from '../AgentIcon.svelte';

  let {
    onOpenSidebar,
  }: {
    onOpenSidebar?: () => void;
  } = $props();

  // Show the header Archive button only for sessions that are still live.
  // Archived sessions have nothing to archive; re-archiving would be confusing.
  const isLive = $derived(
    app.currentSession ? app.live.some((s) => s.session === app.currentSession) : false,
  );

  async function archiveCurrent() {
    const sid = app.currentSession;
    if (!sid) return;
    // closeSession refreshes the lists; ChatView already handles archived
    // sessions in read-only mode, so no navigation is needed.
    await app.closeSession(sid);
  }
</script>

<header class="flex items-center gap-2 px-3 py-2 md:px-6 md:py-3">
  {#if onOpenSidebar}
    <Button
      variant="ghost"
      size="icon"
      onclick={onOpenSidebar}
      aria-label="Show sidebar"
      title="Show sidebar"
      class="shrink-0 text-muted-foreground md:hidden"
    >
      <PanelLeft />
    </Button>
  {/if}
  {#if app.loadingSession || !app.currentSession}
    <AppMark animate class="w-4 shrink-0 text-muted-foreground" />
  {:else}
    <AgentIcon
      agent={app.currentAgent}
      model={app.currentModel}
      class="size-3.5 shrink-0 text-muted-foreground"
    />
  {/if}
  {#if app.currentAgentStatus?.running}
    {@const qd = app.currentAgentStatus.queueDepth}
    <span
      class="inline-flex shrink-0 items-center gap-1 text-muted-foreground"
      title={qd > 0 ? `Running · ${qd} queued` : 'Running'}
      aria-label={qd > 0 ? `Running, ${qd} queued` : 'Running'}
    >
      <Loader2 class="size-3.5 animate-spin" />
      {#if qd > 0}
        <span class="text-[10px] font-medium leading-none">{qd}</span>
      {/if}
    </span>
  {/if}
  <h2 class="flex min-w-0 items-center overflow-hidden text-sm font-semibold" title={app.currentSession ?? ''}>
    {#if !app.currentSession || app.loadingSession}
      <span class="truncate">Loading session…</span>
    {:else}
      {@const sid = app.currentSession}
      {@const session = [...app.live, ...app.archived].find((s) => s.session === sid)}
      {@const project = session && app.projects.find((p) => p.id === session.project_id)}
      {@const parentSid = session?.tags?.['roy-scheduler:initiated_by_session']
                        ?? session?.tags?.['roy-scheduler:parent_session_id']
                        ?? null}
      {#if project}
        <span class="shrink-0 text-muted-foreground/80">{project.name}</span>
        <span class="px-1 shrink-0 text-muted-foreground/50">/</span>
      {/if}
      {#if parentSid}
        <a
          href="/s/{parentSid}"
          title="Spawned by session {parentSid}"
          onclick={(e) => {
            if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey) return;
            e.preventDefault();
            void app.openSession(parentSid);
            window.history.pushState(null, '', `/s/${parentSid}`);
          }}
          class="inline-flex shrink-0 items-center gap-1 rounded px-1.5 py-0.5 text-[0.7rem] font-medium text-muted-foreground hover:bg-accent hover:text-foreground"
        >
          <ArrowUpFromLine class="h-3 w-3" />
          <span>{app.titleFor(parentSid) ?? parentSid.slice(0, 8)}</span>
        </a>
        <span class="px-1 shrink-0 text-muted-foreground/50">/</span>
      {/if}
      <span class="truncate">{app.titleFor(sid)}</span>
    {/if}
  </h2>
  {#if isLive}
    <DropdownMenu.Root>
      <DropdownMenu.Trigger
        aria-label="Session actions"
        title="Session actions"
        class="ml-auto inline-flex size-9 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40"
      >
        <MoreHorizontal class="size-4" />
      </DropdownMenu.Trigger>
      <DropdownMenu.Content align="end">
        <DropdownMenu.Item onSelect={archiveCurrent}>
          <Archive class="size-4" />
          Archive
        </DropdownMenu.Item>
      </DropdownMenu.Content>
    </DropdownMenu.Root>
  {/if}
</header>
