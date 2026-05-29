<script lang="ts">
  import { app } from './state.svelte';
  import { Button } from '$lib/components/ui/button';
  import { ScrollArea } from '$lib/components/ui/scroll-area';
  import {
    Plus,
    PanelLeft,
    FolderPlus,
    Inbox,
    Folder,
    Bot,
    CalendarClock,
    Zap,
    Pin,
    Plug,
    LogOut,
    Users,
  } from '@lucide/svelte';
  import { authState } from './auth.svelte';
  import { royClient } from './client';
  import AppMark from './AppMark.svelte';
  import AgentIcon from './AgentIcon.svelte';
  import Skeleton from './Skeleton.svelte';
  import ThemeToggle from './ThemeToggle.svelte';
  import ProjectGroup from './components/ProjectGroup.svelte';
  import BackgroundGroup from './components/BackgroundGroup.svelte';
  import TeamsGroup from './components/TeamsGroup.svelte';
  import ArchivedSessionRow from './components/ArchivedSessionRow.svelte';
  import SessionRowShell from './components/SessionRowShell.svelte';
  import SessionRowMenu from './components/SessionRowMenu.svelte';
  import SpawningRow from './components/SpawningRow.svelte';
  import DeleteProjectDialog from './components/DeleteProjectDialog.svelte';
  import DeleteSessionDialog from './components/DeleteSessionDialog.svelte';
  import { type Project } from './management-client';
  import { LS, lsGet, lsSet, errMsg } from './utils';

  // Cached at module load — Tailwind `md:` breakpoint (768px). Allocating
  // a fresh MediaQueryList per row click was wasteful with N sessions.
  const desktopMql =
    typeof window !== 'undefined' ? window.matchMedia('(min-width: 768px)') : null;

  async function onSignOut() {
    await authState.logout();
    royClient.close();
  }

  let {
    onNew,
    onOpenProject,
    onOpenAgents,
    onOpenScheduled,
    onOpenSkills,
    onOpenConnections,
    activeNav = null,
    open = true,
    onClose,
    onOpen,
  }: {
    onNew: () => void;
    onOpenProject: (id: string) => void;
    onOpenAgents?: () => void;
    onOpenScheduled?: () => void;
    onOpenSkills?: () => void;
    onOpenConnections?: () => void;
    /** Highlights the matching footer pill so the user can see which top-level
     *  page they are on. `null` when the main area shows a session/project/home. */
    activeNav?: 'agents' | 'scheduled' | 'skills' | 'connections' | null;
    open?: boolean;
    onClose?: () => void;
    onOpen?: () => void;
  } = $props();

  /** Mobile UX: tapping a row in the overlay should also close the
   *  overlay. On desktop the sidebar stays put (claude-agent pattern). */
  function closeOnMobile() {
    if (!desktopMql?.matches) onClose?.();
  }

  function pickArchived(id: string) {
    // Read-only open: snapshot the journal, no resume. Bringing the
    // session back live is an explicit gesture via the "Resume" icon
    // button that appears on hover.
    void app.openSession(id);
    closeOnMobile();
  }

  // Session id whose row is currently in inline-rename mode. At most one
  // row can be editing — the three-dots "Rename" menu item sets this; the
  // row's bindable `editing` prop clears it on commit/cancel.
  let editingSessionId = $state<string | null>(null);

  const loadingSessions = $derived(app.status === 'idle' || app.status === 'connecting');
  // True iff a spawn without a project is in flight — i.e. the ghost row
  // belongs in the Personal/orphan list. Hoisted so the empty-state guard
  // and the row render share one read.
  const spawningPersonal = $derived(
    !!app.spawningSession && !app.spawningSession.projectId,
  );

  // Sidebar view tabs. Persisted across reloads. The Background tab only
  // exists when scheduler-spawned sessions are present, so a stored
  // 'background' falls back to 'personal' when none remain.
  type Tab = 'personal' | 'projects' | 'teams' | 'background';
  function loadTab(): Tab {
    const raw = lsGet(LS.sidebarTab);
    return raw === 'projects' || raw === 'teams' || raw === 'background' ? raw : 'personal';
  }
  // `activeTab` is the persisted user intent. We never mutate it from an
  // effect — doing so during the initial load (before sessions arrive,
  // when `backgroundSessions` is momentarily empty) would clobber a stored
  // 'background' choice. Instead `visibleTab` derives what to actually show.
  let activeTab = $state<Tab>(loadTab());
  let hasBackground = $derived(app.backgroundSessions.length > 0);
  let visibleTab = $derived(
    activeTab === 'background' && !hasBackground ? 'personal' : activeTab,
  );

  let tabs = $derived([
    { id: 'personal' as const, label: 'Personal', icon: Inbox },
    { id: 'projects' as const, label: 'Projects', icon: Folder },
    { id: 'teams' as const, label: 'Teams', icon: Users },
    ...(hasBackground
      ? [{ id: 'background' as const, label: 'Background', icon: Bot }]
      : []),
  ]);

  function selectTab(t: Tab) {
    activeTab = t;
    lsSet(LS.sidebarTab, t);
  }

  let deleteTarget = $state<Project | null>(null);
  let deleteSessionTarget = $state<string | null>(null);

  // Inline "New project" UX. `null` = idle (button shown); string = editing
  // (input shown, focused). On Enter we create and navigate into the new
  // project; Escape / submit-while-empty closes back to idle.
  let newProjectName = $state<string | null>(null);
  let newProjectSubmitting = $state(false);
  let newProjectInputEl: HTMLInputElement | undefined = $state();

  $effect(() => {
    if (newProjectName !== null) {
      queueMicrotask(() => newProjectInputEl?.focus());
    }
  });

  async function submitNewProject() {
    const trimmed = (newProjectName ?? '').trim();
    if (!trimmed) {
      newProjectName = null;
      return;
    }
    newProjectSubmitting = true;
    try {
      const project = await app.createProject(trimmed);
      newProjectName = null;
      onOpenProject(project.id);
      closeOnMobile();
    } catch (e) {
      app.lastError = errMsg(e);
    } finally {
      newProjectSubmitting = false;
    }
  }

  function onNewProjectKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      e.preventDefault();
      void submitNewProject();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      newProjectName = null;
    }
  }
</script>

<aside
  aria-label="Sessions"
  class={[
    'fixed inset-y-0 left-0 z-40 flex flex-col overflow-hidden border-r border-sidebar-border bg-sidebar text-sidebar-foreground transition-[transform,width] duration-200 ease-out',
    // Width: full-width overlay on mobile when open, icon rail/full panel on desktop.
    open ? 'w-full md:w-64' : 'w-full md:w-14',
    // On mobile, slide the sidebar off-screen when closed. On desktop the
    // collapsed rail stays visible (translate-x-0).
    open ? 'translate-x-0' : 'max-md:-translate-x-full md:translate-x-0',
  ]}
>
  {#if !open}
    <!-- Collapsed icon rail. Minimal: expand + new chat + theme toggle.
         No AppMark to avoid visual noise + the focus-ring artifact. -->
    <div class="flex h-full w-14 flex-col items-center gap-1 py-3">
      <Button
        variant="ghost"
        size="icon"
        onclick={onOpen}
        aria-label="Show sidebar"
        title="Show sidebar"
        class="text-muted-foreground hover:bg-sidebar-accent/60"
      >
        <PanelLeft />
      </Button>
      <Button
        variant="ghost"
        size="icon"
        onclick={onNew}
        aria-label="New chat"
        title="New chat"
        class="text-muted-foreground hover:bg-sidebar-accent/60"
      >
        <Plus />
      </Button>
      {#if onOpenAgents}
        <Button
          variant="ghost"
          size="icon"
          onclick={onOpenAgents}
          aria-label="Agents"
          title="Agents"
          class="text-muted-foreground hover:bg-sidebar-accent/60"
        >
          <Bot />
        </Button>
      {/if}
      {#if onOpenSkills}
        <Button
          variant="ghost"
          size="icon"
          onclick={onOpenSkills}
          aria-label="Skills"
          title="Skills"
          class="text-muted-foreground hover:bg-sidebar-accent/60"
        >
          <Zap />
        </Button>
      {/if}
      {#if onOpenScheduled}
        <Button
          variant="ghost"
          size="icon"
          onclick={onOpenScheduled}
          aria-label="Scheduled"
          title="Scheduled tasks"
          class="text-muted-foreground hover:bg-sidebar-accent/60"
        >
          <CalendarClock />
        </Button>
      {/if}
      {#if onOpenConnections}
        <Button
          variant="ghost"
          size="icon"
          onclick={onOpenConnections}
          aria-label="Connections"
          title="Connections — MCP servers"
          class="text-muted-foreground hover:bg-sidebar-accent/60"
        >
          <Plug />
        </Button>
      {/if}
      <div class="flex-1"></div>
      {#if authState.user}
        {@const u = authState.user}
        {@const displayName = u.display_name?.trim() || u.username}
        {@const initial = (displayName[0] ?? '?').toUpperCase()}
        <div
          aria-label={`Signed in as ${displayName}`}
          title={`Signed in as ${displayName} (@${u.username})`}
          class="flex size-8 shrink-0 items-center justify-center rounded-full bg-primary/15 text-xs font-semibold text-primary"
        >
          {initial}
        </div>
      {/if}
      <ThemeToggle />
      <Button
        variant="ghost"
        size="icon"
        onclick={() => void onSignOut()}
        aria-label="Sign out"
        title={authState.user ? `Sign out (${authState.user.username})` : 'Sign out'}
        class="text-muted-foreground hover:bg-sidebar-accent/60"
      >
        <LogOut />
      </Button>
    </div>
  {:else}
    <div class="flex h-full flex-col">
      <div class="flex items-center justify-between gap-2 px-3 pt-3 pb-2">
        <button
          type="button"
          title="Reload roy-web"
          aria-label="Reload roy-web"
          onclick={() => window.location.reload()}
          class="rounded-md border-0 bg-transparent p-1.5 transition-opacity hover:opacity-80 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/40"
        >
          <AppMark class="w-7 text-foreground" />
        </button>
        <div class="flex items-center gap-1">
          {#if onClose}
            <Button
              variant="ghost"
              size="icon"
              onclick={onClose}
              aria-label="Hide sidebar"
              title="Hide sidebar"
              class="text-muted-foreground hover:bg-sidebar-accent/60"
            >
              <PanelLeft />
            </Button>
          {/if}
        </div>
      </div>

      <!-- View tabs: segmented control (active = icon + label, inactive =
           icon-only) + a trailing "+" that always starts a new chat. -->
      <div class="flex items-center gap-1.5 px-3 pb-2">
        <div
          role="tablist"
          aria-label="Sidebar view"
          class="flex min-w-0 flex-1 items-center gap-0.5 rounded-full bg-muted/50 p-0.5"
        >
          {#each tabs as t (t.id)}
            {@const Icon = t.icon}
            {@const isActive = visibleTab === t.id}
            <button
              type="button"
              role="tab"
              aria-selected={isActive}
              title={t.label}
              onclick={() => selectTab(t.id)}
              class={[
                'flex items-center gap-1.5 rounded-full px-2.5 py-1.5 text-xs font-medium transition-colors',
                isActive
                  ? 'bg-background text-foreground shadow-sm'
                  : 'text-muted-foreground hover:text-foreground',
              ]}
            >
              <Icon class="size-4 shrink-0" />
              {#if isActive}
                <span class="truncate">{t.label}</span>
              {/if}
            </button>
          {/each}
        </div>
        <button
          type="button"
          onclick={() => { onNew(); closeOnMobile(); }}
          aria-label="New chat"
          title="New chat"
          class="flex size-8 shrink-0 items-center justify-center rounded-full border border-border bg-background text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
        >
          <Plus class="size-4" />
        </button>
      </div>

      <ScrollArea class="min-h-0 flex-1 px-3">
        <div class="flex flex-col gap-3 pb-3">
          {#if app.pinnedSessions.length > 0}
            <!-- Pinned group: spans live + archived + background so a session
                 the user marked with `tags.pinned == "true"` always lives here
                 regardless of which tab is active. Filtered out of orphanLive,
                 regularLive, backgroundSessions etc. so it appears in exactly
                 one place. -->
            <div class="flex flex-col gap-0.5">
              <h3 class="flex items-center gap-1.5 px-2 pb-1 text-[0.65rem] font-medium uppercase tracking-wide text-muted-foreground/70">
                <Pin class="size-3 shrink-0" />
                Pinned
              </h3>
              <ul class="flex flex-col">
                {#each app.pinnedSessions as s (s.session)}
                  {@const active = app.currentSession === s.session}
                  {@const working = app.activeSessions[s.session] === true}
                  {@const archived = app.isArchived(s.session)}
                  <li>
                    <SessionRowShell
                      session={s.session}
                      active={active}
                      italic={archived}
                      overlayPadding="pr-8"
                      editing={editingSessionId === s.session}
                      onEditingChange={(next) => (editingSessionId = next ? s.session : null)}
                      onPick={(id) => { void app.openSession(id); closeOnMobile(); }}
                    >
                      {#snippet icon()}
                        {#if working}
                          <AppMark animate class="w-4 shrink-0 text-foreground" />
                        {:else}
                          <AgentIcon
                            agent={s.harness}
                            model={s.model}
                            class={archived
                              ? 'size-3.5 shrink-0 text-muted-foreground opacity-70'
                              : 'size-3.5 shrink-0 text-muted-foreground'}
                          >
                            {#snippet fallback()}
                              <Inbox class="size-3.5 shrink-0 text-muted-foreground" />
                            {/snippet}
                          </AgentIcon>
                        {/if}
                      {/snippet}
                      {#snippet actions()}
                        <SessionRowMenu
                          session={s.session}
                          pinned={true}
                          {archived}
                          onRename={() => (editingSessionId = s.session)}
                        />
                      {/snippet}
                    </SessionRowShell>
                  </li>
                {/each}
              </ul>
            </div>
          {/if}

          {#if visibleTab === 'personal'}
            {#if loadingSessions}
              <ul class="flex flex-col">
                {#each Array(3) as _, i (i)}
                  <li class="flex items-center gap-2 px-2 py-1.5">
                    <Skeleton class="size-3.5 rounded" />
                    <Skeleton class="h-3 flex-1 max-w-[8rem]" />
                  </li>
                {/each}
              </ul>
            {:else if app.orphanLive.length === 0 && app.orphanArchived.length === 0 && !spawningPersonal}
              <p class="px-2 py-1 text-xs text-muted-foreground/70">No personal sessions</p>
            {:else}
              {#if spawningPersonal}
                <SpawningRow />
              {/if}
              {#if app.orphanLive.length > 0}
                <ul class="flex flex-col">
                  {#each app.orphanLive as s (s.session)}
                    {@const active = app.currentSession === s.session}
                    {@const working = app.activeSessions[s.session] === true}
                    {@const pinned = app.isPinned(s)}
                    <li>
                      <SessionRowShell
                        session={s.session}
                        active={active}
                        overlayPadding="pr-8"
                        editing={editingSessionId === s.session}
                        onEditingChange={(next) => (editingSessionId = next ? s.session : null)}
                        onPick={(id) => { void app.openSession(id); closeOnMobile(); }}
                      >
                        {#snippet icon()}
                          {#if working}
                            <AppMark animate class="w-4 shrink-0 text-foreground" />
                          {:else}
                            <AgentIcon
                              agent={s.harness}
                              model={s.model}
                              class="size-3.5 shrink-0 text-muted-foreground"
                            >
                              {#snippet fallback()}
                                <Inbox class="size-3.5 shrink-0 text-muted-foreground" />
                              {/snippet}
                            </AgentIcon>
                          {/if}
                        {/snippet}
                        {#snippet actions()}
                          <SessionRowMenu
                            session={s.session}
                            {pinned}
                            onRename={() => (editingSessionId = s.session)}
                          />
                        {/snippet}
                      </SessionRowShell>
                    </li>
                  {/each}
                </ul>
              {/if}

              {#if app.orphanArchived.length > 0}
                <ul class={['flex flex-col', app.orphanLive.length > 0 && 'border-t border-sidebar-border/40 pt-2 mt-1']}>
                  {#each app.orphanArchived as s (s.session)}
                    <li>
                      <ArchivedSessionRow
                        session={s.session}
                        onPick={pickArchived}
                        onResume={(id) => void app.resumeAndOpen(id)}
                        onDelete={(id) => (deleteSessionTarget = id)}
                      />
                    </li>
                  {/each}
                </ul>
              {/if}
            {/if}
          {:else if visibleTab === 'projects'}
            <div class="flex flex-col gap-0.5">
              {#if newProjectName === null}
                <button
                  type="button"
                  onclick={() => (newProjectName = '')}
                  class="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-sm text-foreground transition-colors hover:bg-sidebar-accent/50"
                >
                  <FolderPlus class="size-4 text-muted-foreground" />
                  <span>New project</span>
                </button>
              {:else}
                <div class="flex w-full items-center gap-2 rounded-md px-2 py-1 text-sm">
                  <FolderPlus class="size-4 shrink-0 text-muted-foreground" />
                  <input
                    bind:this={newProjectInputEl}
                    bind:value={newProjectName}
                    onkeydown={onNewProjectKeydown}
                    onblur={() => { if (!(newProjectName ?? '').trim()) newProjectName = null; }}
                    disabled={newProjectSubmitting}
                    placeholder="Project name — Enter to create"
                    class="min-w-0 flex-1 bg-transparent text-sm text-foreground placeholder:text-muted-foreground/60 focus:outline-none disabled:opacity-50"
                  />
                </div>
              {/if}

              {#if loadingSessions}
                <ul class="flex flex-col">
                  {#each Array(3) as _, i (i)}
                    <li class="flex items-center gap-2 px-2 py-1.5">
                      <Skeleton class="size-3.5 rounded" />
                      <Skeleton class="h-3 flex-1 max-w-[8rem]" />
                    </li>
                  {/each}
                </ul>
              {:else if app.projects.length === 0}
                <p class="px-2 py-1 text-xs text-muted-foreground/70">No projects yet</p>
              {:else}
                <div class="flex flex-col">
                  {#each app.projects as p (p.id)}
                    <ProjectGroup
                      project={p}
                      onPickSession={(id) => { void app.openSession(id); closeOnMobile(); }}
                      onOpen={(id) => { onOpenProject(id); closeOnMobile(); }}
                      onDelete={(proj) => (deleteTarget = proj)}
                      onDeleteSession={(id) => (deleteSessionTarget = id)}
                    />
                  {/each}
                </div>
              {/if}
            </div>
          {:else if visibleTab === 'teams'}
            <TeamsGroup />
          {:else}
            <!-- Background: scheduler-spawned sessions (live + archived). The
                 tab itself only exists while at least one is present. -->
            <BackgroundGroup
              sessions={app.backgroundSessions}
              onPickSession={(id) => { void app.openSession(id); closeOnMobile(); }}
              onArchive={(id) => void app.closeSession(id)}
              onDelete={(id) => (deleteSessionTarget = id)}
            />
          {/if}
        </div>
      </ScrollArea>

      {#snippet navPill(
        key: 'agents' | 'scheduled' | 'skills' | 'connections',
        label: string,
        title: string,
        Icon: typeof Bot,
        onPick: () => void,
      )}
        {@const isActive = activeNav === key}
        <button
          type="button"
          aria-current={isActive ? 'page' : undefined}
          {title}
          onclick={() => { onPick(); closeOnMobile(); }}
          class={[
            'flex items-center gap-1.5 rounded-full px-2.5 py-1.5 text-xs font-medium transition-colors',
            isActive
              ? 'bg-background text-foreground shadow-sm'
              : 'text-muted-foreground hover:text-foreground',
          ]}
        >
          <Icon class="size-4 shrink-0" />
          {#if isActive}
            <span class="truncate">{label}</span>
          {/if}
        </button>
      {/snippet}

      <div class="flex items-center justify-between gap-1.5 px-3 py-2">
        <div
          role="navigation"
          aria-label="Sidebar pages"
          class="flex min-w-0 items-center gap-0.5 rounded-full bg-muted/50 p-0.5"
        >
          {#if onOpenAgents}
            {@render navPill('agents', 'Agents', 'Agents — manage personas', Bot, onOpenAgents)}
          {/if}
          {#if onOpenSkills}
            {@render navPill('skills', 'Skills', 'Skills — slash command catalog', Zap, onOpenSkills)}
          {/if}
          {#if onOpenScheduled}
            {@render navPill(
              'scheduled',
              'Scheduled',
              'Scheduled — cron triggers and recent fires',
              CalendarClock,
              onOpenScheduled,
            )}
          {/if}
          {#if onOpenConnections}
            {@render navPill('connections', 'Connections', 'Connections — MCP servers', Plug, onOpenConnections)}
          {/if}
        </div>
      </div>

      {#if authState.user}
        {@const u = authState.user}
        {@const displayName = u.display_name?.trim() || u.username}
        {@const initial = (displayName[0] ?? '?').toUpperCase()}
        <div class="flex items-center gap-2 border-t border-sidebar-border/40 px-3 py-2">
          <div
            aria-hidden="true"
            class="flex size-7 shrink-0 items-center justify-center rounded-full bg-primary/15 text-xs font-semibold text-primary"
          >
            {initial}
          </div>
          <div class="flex min-w-0 flex-1 flex-col leading-tight">
            <span class="truncate text-sm font-medium text-foreground">{displayName}</span>
            {#if displayName !== u.username}
              <span class="truncate text-[11px] text-muted-foreground">@{u.username}</span>
            {/if}
          </div>
          <ThemeToggle />
          <Button
            variant="ghost"
            size="icon"
            onclick={() => void onSignOut()}
            aria-label="Sign out"
            title={`Sign out (${u.username})`}
            class="size-7 text-muted-foreground hover:bg-sidebar-accent/60"
          >
            <LogOut class="size-4" />
          </Button>
        </div>
      {/if}
    </div>
  {/if}
</aside>

{#if deleteTarget}
  <DeleteProjectDialog
    project={deleteTarget}
    onclose={() => (deleteTarget = null)}
  />
{/if}

{#if deleteSessionTarget}
  <DeleteSessionDialog
    session={deleteSessionTarget}
    onclose={() => (deleteSessionTarget = null)}
  />
{/if}
