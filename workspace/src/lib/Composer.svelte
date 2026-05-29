<script lang="ts">
  import { onMount } from 'svelte';
  import { app } from './state.svelte';
  import { supportsSlashCommands, type Harness } from './wire';
  import { ArrowUp, Loader2, RefreshCw } from '@lucide/svelte';
  import * as Select from '$lib/components/ui/select';
  import ModelPicker from './ModelPicker.svelte';
  import ConnectionPickerDialog from './ConnectionPickerDialog.svelte';
  import NewProjectDialog from './components/NewProjectDialog.svelte';
  import ComposerActions from './ComposerActions.svelte';
  import ComposerTextarea from './components/ComposerTextarea.svelte';
  import { harnessesConfig, defaultModelFor } from './harnesses-config.svelte';
  import { authState } from './auth.svelte';
  import { agentsStore } from './agents.svelte';
  import { focusCaret, INTERACTIVE_SEL, LS, lsGet, lsSet } from './utils';
  import { useSlashCommands } from './useSlashCommands.svelte';

  let {
    onCreated,
    lockedProjectId,
    onOpenConnections,
  }: {
    onCreated: (sessionId: string) => void;
    /** When set, the project picker is hidden and every spawn pins to this id. */
    lockedProjectId?: string | undefined;
    /** Open the /connections management page. The picker's footer "Manage…"
     *  link routes here when present. */
    onOpenConnections?: () => void;
  } = $props();

  let agent = $state<Harness | ''>('');
  let model = $state<string>('');
  let selectedAgentName = $state<string | undefined>(undefined);
  // Presets whose CLI accepts a roy-managed MCP config file. Other presets
  // are rejected by the daemon with a non-empty connection list, so we hide
  // the picker for them.
  const MCP_PRESETS = new Set<Harness>(['claude', 'opencode', 'gemini', 'codex']);
  const mcpEligible = $derived(
    agent !== '' && MCP_PRESETS.has(agent as Harness),
  );
  // MCP connections to attach to the spawned session. Only meaningful for
  // MCP-eligible presets (daemon rejects others with a 502). Reset whenever
  // the active agent changes — see the $effect below.
  let selectedConnectionIds = $state<string[]>([]);
  let connectionsDialogOpen = $state(false);
  // Restore last-used project from localStorage; the $effect below pins this
  // to `lockedProjectId` synchronously on first run when locked.
  let selectedProjectId = $state<string | undefined>(
    lsGet(LS.lastProjectId) || undefined,
  );
  // '' = personal; otherwise team_id. Membership is re-validated below.
  let selectedTeamId = $state<string>(lsGet(LS.lastScope) ?? '');
  let userTeams = $derived(authState.user?.teams ?? []);
  let draft = $state('');
  // `app.spawningSession` is global — every composer reflects the same
  // in-flight spawn, which is correct: the daemon serializes them anyway.
  let submitting = $derived(app.spawningSession !== null);
  let showNewProject = $state(false);

  // Picker groups: Personal first, then one section per team the user belongs
  // to. Each section lists its projects; the dropdown renders a "No project"
  // entry at the top of each section.
  type PickerGroup = { teamId: string; name: string; projects: typeof app.projects };
  let pickerGroups = $derived.by<PickerGroup[]>(() => {
    const groups: PickerGroup[] = [
      { teamId: '', name: 'Personal', projects: app.projects.filter((p) => !p.team_id) },
    ];
    for (const t of userTeams) {
      groups.push({
        teamId: t.id,
        name: t.name,
        projects: app.projects.filter((p) => p.team_id === t.id),
      });
    }
    return groups;
  });

  // Combined value encodes scope+project as a single Select value so the
  // dropdown can switch both at once. Format:
  //   personal:                 → Personal, no project
  //   personal:<projectId>      → Personal + project
  //   team:<teamId>:            → Team, no project
  //   team:<teamId>:<projectId> → Team + project
  //   __new__                   → pseudo-value that opens NewProjectDialog
  const NEW_PROJECT_VALUE = '__new__';
  function makeValue(teamId: string, projectId: string | undefined): string {
    return teamId ? `team:${teamId}:${projectId ?? ''}` : `personal:${projectId ?? ''}`;
  }
  function parseValue(v: string): { teamId: string; projectId: string } | null {
    if (v.startsWith('personal:')) {
      return { teamId: '', projectId: v.slice('personal:'.length) };
    }
    if (v.startsWith('team:')) {
      const rest = v.slice('team:'.length);
      const sep = rest.indexOf(':');
      if (sep === -1) return null;
      return { teamId: rest.slice(0, sep), projectId: rest.slice(sep + 1) };
    }
    return null;
  }

  let pickerValue = $derived(makeValue(selectedTeamId, selectedProjectId));
  let pillLabel = $derived.by(() => {
    const proj = app.projects.find((p) => p.id === selectedProjectId);
    if (selectedTeamId) {
      const teamName = userTeams.find((t) => t.id === selectedTeamId)?.name ?? 'Team';
      return `${teamName} · ${proj?.name ?? 'No project'}`;
    }
    return proj ? `Personal · ${proj.name}` : 'Personal';
  });

  let selectedAgent = $derived(
    selectedAgentName !== undefined
      ? agentsStore.list.find((a) => a.name === selectedAgentName)
      : undefined,
  );
  let selectedAgentLabel = $derived(
    selectedAgent ? `⌗ ${selectedAgent.name}` : undefined,
  );

  function onPickerChange(v: string | undefined) {
    if (!v) return;
    if (v === NEW_PROJECT_VALUE) {
      showNewProject = true;
      return;
    }
    const parsed = parseValue(v);
    if (!parsed) return;
    selectedTeamId = parsed.teamId;
    selectedProjectId = parsed.projectId || undefined;
    app.focusComposer();
  }
  let textareaEl: HTMLTextAreaElement | undefined = $state();

  // Slash-command popover. Triggers when the caret sits in a token that
  // starts with `/` *at* the start of the draft or right after whitespace —
  // matches the convention agents already use for slash commands. Closes
  // when the user types a space, backspaces past the `/`, navigates away,
  // or hits Escape. The machinery is shared with ChatView via this hook;
  // only the agent source (local `agent`) differs here.
  const slash = useSlashCommands({
    getTextarea: () => textareaEl,
    getAgent: () => agent,
    getDraft: () => draft,
    setDraft: (v) => (draft = v),
  });

  let projectLocked = $derived(lockedProjectId !== undefined);

  // Keep selectedProjectId pinned when locked; locked id can change if the
  // user navigates between project pages without unmount.
  $effect(() => {
    if (lockedProjectId !== undefined && selectedProjectId !== lockedProjectId) {
      selectedProjectId = lockedProjectId;
    }
  });

  // Sync agent/model from the store. Runs on initial population and whenever
  // the store updates (e.g. after refresh or agent change).
  $effect(() => {
    const harnesses = harnessesConfig.harnesses;
    if (harnesses.length === 0) return;

    const agentEntry = harnesses.find((a) => a.name === agent) ?? harnesses[0];
    if (agentEntry.name !== agent) {
      agent = agentEntry.name;
    }

    const modelEntry =
      agentEntry.models.find((m) => m.id === model) ??
      defaultModelFor(harnesses, agentEntry.name);
    if (modelEntry && modelEntry.id !== model) {
      model = modelEntry.id;
    }
  });

  // Connections only make sense for MCP-eligible presets — the daemon rejects
  // others with a non-empty list. Clear any selection when the user switches
  // to an ineligible preset so a stale list can't poison the next spawn.
  $effect(() => {
    if (!mcpEligible && selectedConnectionIds.length > 0) {
      selectedConnectionIds = [];
    }
  });

  // If the selected project was deleted, fall back to orphan. Skip when locked.
  $effect(() => {
    if (projectLocked) return;
    if (selectedProjectId && !app.projects.some((p) => p.id === selectedProjectId)) {
      selectedProjectId = undefined;
    }
  });

  $effect(() => {
    if (projectLocked) return;
    if (!selectedProjectId) return;
    const proj = app.projects.find((p) => p.id === selectedProjectId);
    if (!proj) return;
    const wantTeam = selectedTeamId || null;
    if ((proj.team_id ?? null) !== wantTeam) {
      selectedProjectId = undefined;
    }
  });

  $effect(() => {
    if (selectedTeamId && !userTeams.some((t) => t.id === selectedTeamId)) {
      selectedTeamId = '';
    }
  });

  // Persist selection when not locked (locked spawns shouldn't pollute history).
  $effect(() => {
    if (projectLocked) return;
    lsSet(LS.lastProjectId, selectedProjectId ?? '');
  });

  $effect(() => {
    lsSet(LS.lastScope, selectedTeamId);
  });

  $effect(() => {
    void app.composerFocusTick;
    // Append (with separator) instead of replacing, so an injection never
    // silently clobbers a draft the user already typed. Then park the
    // caret at the end via `focusCaret` — for plain focus bumps (no
    // inject) we keep the old behavior of just focusing without moving
    // the caret, so "+ new chat" while typing doesn't snap selection.
    const inject = app.takeComposerPrefill();
    if (inject !== null) {
      const trimmed = inject.trimEnd();
      draft = draft.length === 0
        ? `${trimmed}\n\n`
        : `${draft.replace(/\s*$/, '')}\n\n${trimmed}\n\n`;
      if (textareaEl) focusCaret(textareaEl, draft.length);
    } else {
      queueMicrotask(() => textareaEl?.focus());
    }
  });

  onMount(() => {
    harnessesConfig.refresh();
  });

  async function onSubmit(e: SubmitEvent) {
    e.preventDefault();
    const text = draft.trim();
    if (!text || submitting || !agent) return;

    // `selectedAgent` is reactive — if the saved Agent was deleted between
    // selection and submit it resolves to undefined, and the stale id is
    // cleared here.
    if (selectedAgentName !== undefined && !selectedAgent) {
      selectedAgentName = undefined;
    }

    try {
      const sessionId = await app.createSession({
        agent: agent as Harness,
        project_id: selectedProjectId || undefined,
        scope: selectedTeamId ? 'team' : 'personal',
        team_id: selectedTeamId || undefined,
        model,
        firstPrompt: text,
        persona: selectedAgent
          ? { prompt: selectedAgent.body, name: selectedAgent.name }
          : undefined,
        connection_ids:
          mcpEligible && selectedConnectionIds.length > 0
            ? selectedConnectionIds
            : undefined,
      });
      onCreated(sessionId);
    } catch {
      // createSession already published into `app.lastError`.
    }
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<form
  onsubmit={onSubmit}
  onclick={(e) => {
    if ((e.target as HTMLElement).closest(INTERACTIVE_SEL)) return;
    textareaEl?.focus();
  }}
  class="relative flex w-full cursor-text flex-col gap-2 rounded-3xl border border-border/60 bg-card px-4 pb-2 pt-3 shadow-sm transition-colors focus-within:border-ring/60"
>
  <ComposerTextarea
    bind:value={draft}
    bind:ref={textareaEl}
    {slash}
    cap={280}
    disabled={submitting}
    placeholder="Ask anything — Enter to send, Shift+Enter for newline. / for commands."
  />

  <div class="flex flex-wrap items-center gap-2 pt-1">
    <ComposerActions
      disabled={submitting}
      slashSupported={supportsSlashCommands(agent)}
      mcpEligible={mcpEligible}
      mcpCount={selectedConnectionIds.length}
      onAttach={(path) => slash.insertAtCaret(`@${path} `)}
      onPickSkill={slash.openSlashFromMenu}
      onPickConnections={() => (connectionsDialogOpen = true)}
      onError={(msg) => (app.lastError = msg)}
    />
    {#if harnessesConfig.status.kind === 'invalid'}
      <div class="flex w-full items-start gap-2 rounded-lg border border-destructive/40 bg-destructive/10 px-3 py-2 text-xs text-destructive">
        <span class="mt-0.5 shrink-0">⚠</span>
        <div class="flex min-w-0 flex-1 flex-col gap-1">
          <span>{harnessesConfig.status.reason}</span>
          <span class="text-destructive/70">Config: <code class="rounded bg-destructive/20 px-1">{harnessesConfig.configPath}</code></span>
        </div>
        <button
          type="button"
          onclick={() => harnessesConfig.refresh()}
          disabled={harnessesConfig.loading}
          class="ml-1 shrink-0 rounded p-1 hover:bg-destructive/20 disabled:opacity-50"
          title="Retry"
        >
          <RefreshCw class="size-3.5" />
        </button>
      </div>
    {:else if harnessesConfig.harnesses.length === 0}
      <div class="flex w-full items-center gap-2 rounded-lg border bg-muted/50 px-3 py-2 text-xs text-muted-foreground">
        {#if harnessesConfig.status.kind === 'created'}
          <span>Created a sample at <code class="rounded bg-muted px-1">{harnessesConfig.configPath}</code> — edit it and</span>
        {:else}
          <span>No harnesses in <code class="rounded bg-muted px-1">{harnessesConfig.configPath}</code> —</span>
        {/if}
        <button
          type="button"
          onclick={() => harnessesConfig.refresh()}
          disabled={harnessesConfig.loading}
          class="inline-flex items-center gap-1 rounded px-1.5 py-0.5 hover:bg-muted disabled:opacity-50"
          title="Refresh"
        >
          <RefreshCw class="size-3" />
          <span>refresh</span>
        </button>
      </div>
    {:else if agent}
      <ModelPicker
        bind:agent={agent as Harness}
        bind:model
        catalog={harnessesConfig.harnesses}
        disabled={submitting}
        agentLabel={selectedAgentLabel}
        onChange={() => { selectedAgentName = undefined; app.focusComposer(); }}
        onPickAgent={(name) => { selectedAgentName = name; app.focusComposer(); }}
      />
    {/if}


    {#if !projectLocked}
      <!-- Unified scope+project picker. Sections per scope (Personal, then one
           per team), each with a "No project" entry. Last item opens the
           NewProjectDialog via a pseudo-value (see NEW_PROJECT_VALUE). -->
      <Select.Root
        type="single"
        value={pickerValue}
        onValueChange={onPickerChange}
        disabled={submitting}
      >
        <Select.Trigger class="h-8 rounded-full border-border px-3 text-xs font-medium hover:bg-muted">
          {pillLabel}
        </Select.Trigger>
        <Select.Content>
          {#each pickerGroups as g, gi (g.teamId || 'personal')}
            <Select.Group>
              <Select.Label>{g.teamId ? `Team · ${g.name}` : g.name}</Select.Label>
              <Select.Item value={makeValue(g.teamId, undefined)}>No project</Select.Item>
              {#each g.projects as p (p.id)}
                <Select.Item value={makeValue(g.teamId, p.id)}>{p.name}</Select.Item>
              {/each}
            </Select.Group>
            {#if gi < pickerGroups.length - 1}
              <Select.Separator />
            {/if}
          {/each}
          <Select.Separator />
          <Select.Item value={NEW_PROJECT_VALUE}>+ New project</Select.Item>
        </Select.Content>
      </Select.Root>
    {/if}

    <button
      type="submit"
      aria-label={submitting ? 'Spawning session' : 'Send message'}
      title={submitting ? 'Spawning session — this can take a few seconds' : 'Send message'}
      disabled={submitting || !draft.trim()}
      aria-busy={submitting}
      class="ml-auto flex h-9 w-9 items-center justify-center rounded-full bg-foreground text-background transition-opacity hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-60"
    >
      {#if submitting}
        <Loader2 class="h-4 w-4 animate-spin" />
      {:else}
        <ArrowUp class="h-4 w-4" strokeWidth={2.5} />
      {/if}
    </button>
  </div>

  {#if submitting}
    <p class="flex items-center gap-1.5 px-1 text-xs text-muted-foreground">
      <Loader2 class="size-3 animate-spin" />
      Spawning session — this can take a few seconds…
    </p>
  {/if}
</form>

{#if showNewProject}
  <NewProjectDialog
    onclose={() => (showNewProject = false)}
    defaultTeamId={selectedTeamId}
  />
{/if}

<ConnectionPickerDialog
  bind:selected={selectedConnectionIds}
  bind:open={connectionsDialogOpen}
  onManage={onOpenConnections}
/>
