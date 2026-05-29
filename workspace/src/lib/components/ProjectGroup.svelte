<script lang="ts">
  import { app } from '../state.svelte';
  import {
    ChevronDown,
    ChevronRight,
    Check,
    Folder,
    FolderOpen,
    MoreHorizontal,
    Pencil,
    Trash2,
    Users,
  } from '@lucide/svelte';
  import AppMark from '../AppMark.svelte';
  import AgentIcon from '../AgentIcon.svelte';
  import ArchivedSessionRow from './ArchivedSessionRow.svelte';
  import SessionRowShell from './SessionRowShell.svelte';
  import SessionRowMenu from './SessionRowMenu.svelte';
  import SpawningRow from './SpawningRow.svelte';
  import InlineEditable from './InlineEditable.svelte';
  import * as DropdownMenu from '$lib/components/ui/dropdown-menu';
  import { authState } from '../auth.svelte';
  import type { SessionInfo } from '../wire';
  import { type Project } from '../management-client';

  // Project name length cap. The server validates the characters
  // (`validate_project_name`) but doesn't bound the length — we keep it
  // conservative client-side so the sidebar never grows monstrously wide
  // and silly inputs round-trip cleanly.
  const PROJECT_NAME_MAX = 64;

  let {
    project,
    onPickSession,
    onOpen,
    onDelete,
    onDeleteSession,
  }: {
    project: Project;
    onPickSession: (id: string) => void;
    onOpen: (id: string) => void;
    onDelete: (project: Project) => void;
    onDeleteSession: (id: string) => void;
  } = $props();

  let expanded = $derived(!!app.expandedProjects[project.id]);
  let spawningHere = $derived(app.spawningSession?.projectId === project.id);
  // Live sessions belonging to this project, in their natural order.
  // `regularLive` excludes roy-scheduler-spawned sessions — those render in
  // the dedicated Background sidebar group instead, never twice.
  let sessions = $derived(
    app.regularLive.filter((s: SessionInfo) => s.project_id === project.id),
  );
  // Archived sessions for this project — demoted below the live ones.
  let archivedSessions = $derived(
    app.regularArchived.filter((s: SessionInfo) => s.project_id === project.id),
  );

  function toggle(e: MouseEvent) {
    e.stopPropagation();
    app.toggleExpand(project.id);
  }

  let editingName = $state(false);
  let renamingProject = $derived(app.renamingProjects[project.id] === true);
  // Session id whose row is currently in inline-rename mode within this
  // project. At most one row per group; the three-dots "Rename" menu item
  // sets this and SessionRowShell clears it via onEditingChange.
  let editingSessionId = $state<string | null>(null);

  function beginRename() {
    if (renamingProject) return;
    editingName = true;
  }

  function submitName(next: string) {
    editingName = false;
    const trimmed = next.trim().slice(0, PROJECT_NAME_MAX);
    // Empty / whitespace → no-op: project names are mandatory. Same value →
    // no-op so a stale submit doesn't fire a PUT.
    if (trimmed.length === 0 || trimmed === project.name) return;
    void app.renameProject(project.id, trimmed).catch(() => {});
  }

  let teamChoices = $derived(authState.user?.teams ?? []);
  let ownerTeamName = $derived(
    project.team_id
      ? (teamChoices.find((t) => t.id === project.team_id)?.name ?? 'Team')
      : null,
  );
  function moveTo(team_id: string | null) {
    void app.moveProject(project.id, team_id).catch(() => {});
  }
</script>

<div class="flex flex-col">
  <div class="group/proj flex w-full items-center gap-0.5 rounded-md text-sm text-foreground transition-colors hover:bg-sidebar-accent/50">
    <button
      type="button"
      onclick={toggle}
      aria-label={expanded ? 'Collapse project' : 'Expand project'}
      aria-expanded={expanded}
      title={expanded ? 'Collapse' : 'Expand'}
      class="flex size-6 shrink-0 items-center justify-center rounded text-muted-foreground hover:bg-foreground/10 hover:text-foreground"
    >
      {#if expanded}
        <ChevronDown class="size-3.5" />
      {:else}
        <ChevronRight class="size-3.5" />
      {/if}
    </button>
    {#if editingName}
      <div class="flex min-w-0 flex-1 items-center gap-2 rounded-md px-1.5 py-1.5">
        {#if expanded}
          <FolderOpen class="size-4 shrink-0 text-muted-foreground" />
        {:else}
          <Folder class="size-4 shrink-0 text-muted-foreground" />
        {/if}
        <InlineEditable
          value={project.name}
          busy={renamingProject}
          ariaLabel="Rename project"
          placeholder="Project name — Enter to save, Esc to cancel"
          onSubmit={submitName}
          onCancel={() => (editingName = false)}
        />
      </div>
    {:else}
      <button
        type="button"
        onclick={() => onOpen(project.id)}
        class="flex min-w-0 flex-1 select-none items-center gap-2 rounded-md px-1.5 py-1.5 text-left"
      >
        {#if expanded}
          <FolderOpen class="size-4 shrink-0 text-muted-foreground" />
        {:else}
          <Folder class="size-4 shrink-0 text-muted-foreground" />
        {/if}
        <span class="min-w-0 flex-1 truncate" title={project.path}>{project.name}</span>
        {#if ownerTeamName}
          <span
            class="inline-flex shrink-0 items-center gap-0.5 rounded-full bg-primary/10 px-1.5 py-0.5 text-[10px] font-medium text-primary"
            title={`Owned by team ${ownerTeamName}`}
          >
            <Users class="size-2.5" />
            <span class="max-w-[6rem] truncate">{ownerTeamName}</span>
          </span>
        {/if}
      </button>
    {/if}
    <DropdownMenu.Root>
      <DropdownMenu.Trigger
        aria-label="Project actions"
        title="Project actions"
        class="mr-1 flex size-6 shrink-0 items-center justify-center rounded text-muted-foreground opacity-0 transition-opacity hover:bg-foreground/10 hover:text-foreground group-hover/proj:opacity-100 focus-visible:opacity-100 data-[state=open]:opacity-100"
      >
        <MoreHorizontal class="size-3.5" />
      </DropdownMenu.Trigger>
      <DropdownMenu.Content align="end" class="min-w-[10rem]">
        <DropdownMenu.Item onSelect={beginRename} disabled={renamingProject}>
          <Pencil class="size-4" />
          Rename
        </DropdownMenu.Item>
        <DropdownMenu.Sub>
          <DropdownMenu.SubTrigger>
            <Users class="size-4" />
            Move to…
          </DropdownMenu.SubTrigger>
          <DropdownMenu.SubContent class="min-w-[10rem]">
            {@const personalActive = !project.team_id}
            <DropdownMenu.Item
              onSelect={() => moveTo(null)}
              disabled={personalActive}
            >
              {#if personalActive}
                <Check class="size-4" />
              {:else}
                <span class="size-4"></span>
              {/if}
              Personal
            </DropdownMenu.Item>
            {#if teamChoices.length > 0}
              <DropdownMenu.Separator />
              {#each teamChoices as t (t.id)}
                {@const active = project.team_id === t.id}
                <DropdownMenu.Item
                  onSelect={() => moveTo(t.id)}
                  disabled={active}
                >
                  {#if active}
                    <Check class="size-4" />
                  {:else}
                    <span class="size-4"></span>
                  {/if}
                  {t.name}
                </DropdownMenu.Item>
              {/each}
            {/if}
          </DropdownMenu.SubContent>
        </DropdownMenu.Sub>
        <DropdownMenu.Separator />
        <DropdownMenu.Item variant="destructive" onSelect={() => onDelete(project)}>
          <Trash2 class="size-4" />
          Delete
        </DropdownMenu.Item>
      </DropdownMenu.Content>
    </DropdownMenu.Root>
  </div>

  {#if expanded}
    <ul class="ml-4 flex flex-col border-l border-sidebar-border/40 pl-2">
      {#if spawningHere}
        <li><SpawningRow /></li>
      {/if}
      {#if sessions.length === 0 && archivedSessions.length === 0 && !spawningHere}
        <li class="px-2 py-1 text-xs text-muted-foreground/70">No sessions yet</li>
      {:else}
        {#each sessions as s (s.session)}
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
              onPick={onPickSession}
            >
              {#snippet icon()}
                {#if working}
                  <AppMark animate class="w-4 shrink-0 text-foreground" />
                {:else}
                  <AgentIcon
                    agent={s.harness}
                    model={s.model}
                    class="size-3.5 shrink-0 text-muted-foreground"
                  />
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
        {#each archivedSessions as s (s.session)}
          <li>
            <ArchivedSessionRow
              session={s.session}
              onPick={onPickSession}
              onResume={(id) => void app.resumeAndOpen(id)}
              onDelete={onDeleteSession}
            />
          </li>
        {/each}
      {/if}
    </ul>
  {/if}
</div>
