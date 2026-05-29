<script lang="ts">
  import { app } from './state.svelte';
  import { ArrowUp, ArrowDown, Loader2, Square } from '@lucide/svelte';
  import ModelPicker from './ModelPicker.svelte';
  import QueuePanel from './QueuePanel.svelte';
  import ChatHeader from './components/ChatHeader.svelte';
  import LastErrorBanner from './components/LastErrorBanner.svelte';
  import MessageGroups from './components/MessageGroups.svelte';
  import ComposerActions from './ComposerActions.svelte';
  import ComposerTextarea from './components/ComposerTextarea.svelte';
  import { harnessesConfig } from './harnesses-config.svelte';
  import { supportsSlashCommands, type HarnessInfo, type Harness } from './wire';
  import { INTERACTIVE_SEL } from './utils';
  import { useSlashCommands } from './useSlashCommands.svelte';

  let { onOpenSidebar }: { onOpenSidebar?: () => void } = $props();

  let draft = $state('');
  let scrollEl: HTMLDivElement | undefined = $state();
  let textareaEl: HTMLTextAreaElement | undefined = $state();

  // Bottom-anchored auto-scroll. `shouldAutoScroll` flips off the moment
  // the user scrolls up, so we don't yank the viewport away from text they
  // are reading; the floating ArrowDown button below re-arms it.
  let shouldAutoScroll = $state(true);
  const BOTTOM_FUZZ_PX = 24;

  function onScroll() {
    if (!scrollEl) return;
    const distance = scrollEl.scrollHeight - scrollEl.scrollTop - scrollEl.clientHeight;
    shouldAutoScroll = distance <= BOTTOM_FUZZ_PX;
  }

  function scrollToBottom() {
    if (!scrollEl) return;
    scrollEl.scrollTop = scrollEl.scrollHeight;
    shouldAutoScroll = true;
  }

  // Re-pin to bottom on every new journal entry (streamed text chunks count
  // — `groups.length` would miss assistant_text appended to an existing
  // bubble). rAF coalesces a burst of token frames into one layout read +
  // scroll write per frame instead of one per token.
  let pendingScrollRaf = 0;
  $effect(() => {
    void app.entries.length;
    if (!shouldAutoScroll || pendingScrollRaf) return;
    pendingScrollRaf = requestAnimationFrame(() => {
      pendingScrollRaf = 0;
      if (scrollEl) scrollEl.scrollTop = scrollEl.scrollHeight;
    });
  });

  // Drop the caret into the composer whenever the session changes (open /
  // switch) — no extra click needed before typing.
  $effect(() => {
    void app.currentSession;
    queueMicrotask(() => {
      if (app.inputAcquired) textareaEl?.focus();
    });
  });

  function onSubmit(e: SubmitEvent) {
    e.preventDefault();
    trySend();
  }

  // Slash-command popover. Shared with Composer via this hook; here the
  // agent source is the session's `app.currentAgent`.
  const slash = useSlashCommands({
    getTextarea: () => textareaEl,
    getAgent: () => app.currentAgent,
    getDraft: () => draft,
    setDraft: (v) => (draft = v),
  });

  // Composer-side model picker. Bound to `pickerModel`, which mirrors
  // `app.currentModel`. The picker's `onChange` fires `app.setModel`,
  // which is the only thing that actually mutates the daemon-side
  // value — `pickerModel` is a UI buffer that the resulting
  // `model_changed` event keeps in sync via the $effect below.
  const pickerAgent = $derived<Harness>(app.currentAgent ?? 'opencode');
  let pickerModel = $state<string>('');
  $effect(() => {
    pickerModel = app.currentModel ?? '';
  });

  // Build an effective catalog for the locked-agent picker. If the session's
  // current model isn't present in the agent's model list (e.g. a model that
  // was removed from the config after the session was spawned), we prepend a
  // synthetic entry so the picker can still display and switch away from it.
  const effectiveCatalog = $derived.by((): HarnessInfo[] => {
    const base = harnessesConfig.harnesses;
    if (!pickerModel) return base;
    const agentInfo = base.find((a) => a.name === pickerAgent);
    if (!agentInfo) return base;
    const inCatalog = agentInfo.models.some((m) => m.id === pickerModel);
    if (inCatalog) return base;
    // Prepend synthetic stale-model entry to the matched agent's model list.
    const syntheticModels = [
      { id: pickerModel, label: `${pickerModel} (not in config)`, default: false },
      ...agentInfo.models,
    ];
    return base.map((a) =>
      a.name === pickerAgent ? { ...a, models: syntheticModels } : a,
    );
  });

  async function applyModelChange(m: string) {
    // Picker already wrote to `pickerModel` optimistically via `bind:`.
    // If the daemon rejects, roll the buffer back to whatever the daemon
    // still considers authoritative — otherwise the pill lies.
    const prev = app.currentModel ?? '';
    try {
      await app.setModel(m);
    } catch {
      pickerModel = prev;
    }
  }

  function trySend() {
    const text = draft.trim();
    if (!text || !app.currentSession) return;
    // The daemon journals the prompt as a `user_prompt` event, which feeds
    // back through `app.entries` → the `groups` switch above. No local
    // mirror needed — a refresh or second tab sees the same history.
    //
    // `submit` enqueues automatically when a turn is in flight; otherwise
    // it dispatches immediately. Drains on the next terminal Result.
    app.submit(text);
    draft = '';
  }
</script>

<div class="flex h-full flex-col bg-background">
  <ChatHeader onOpenSidebar={onOpenSidebar} />
  {#if app.lastTurnError}
    <LastErrorBanner error={app.lastTurnError} />
  {/if}
  <div class="relative flex-1 overflow-hidden">
    <div
      bind:this={scrollEl}
      onscroll={onScroll}
      class="h-full overflow-y-auto"
    >
      <MessageGroups />
    </div>

    {#if !shouldAutoScroll}
      <div class="pointer-events-none absolute inset-x-0 bottom-3 flex justify-center">
        <button
          type="button"
          aria-label="Scroll to latest"
          onclick={scrollToBottom}
          class="pointer-events-auto flex h-8 w-8 items-center justify-center rounded-full border border-border/60 bg-card text-foreground shadow-sm transition-colors hover:bg-muted"
        >
          <ArrowDown class="size-4" />
        </button>
      </div>
    {/if}
  </div>

  <div class="mx-auto w-full max-w-4xl px-4 pb-4">
    <!-- Archived session: composer is replaced by a centered Resume CTA.
         The session can be read freely above, but writing requires
         bringing the agent back live via `resumeAndOpen`. -->
    {#if app.currentSession && app.isArchived(app.currentSession)}
      {@const resuming = app.resumingSession === app.currentSession}
      <div class="flex flex-col items-center gap-2 py-6">
        <p class="text-xs text-muted-foreground">
          {resuming
            ? 'Waking the agent — this can take a few seconds…'
            : 'This session is archived — read-only.'}
        </p>
        <button
          type="button"
          onclick={() => app.resumeAndOpen(app.currentSession!)}
          disabled={resuming}
          aria-busy={resuming}
          class="inline-flex items-center gap-2 rounded-full bg-foreground px-4 py-2 text-sm font-medium text-background transition-opacity hover:opacity-90 disabled:cursor-default disabled:opacity-70"
        >
          {#if resuming}
            <Loader2 class="size-4 animate-spin" />
            Resuming…
          {:else}
            <ArrowDown class="size-4 rotate-180" />
            Resume session
          {/if}
        </button>
      </div>
    {:else}
      <QueuePanel />

      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
      <form
        onsubmit={onSubmit}
        onclick={(e) => {
          // Click on any non-interactive area of the composer card focuses
          // the textarea — feels like the whole pill is the input.
          if ((e.target as HTMLElement).closest(INTERACTIVE_SEL)) return;
          textareaEl?.focus();
        }}
        class="relative flex w-full cursor-text flex-col gap-2 rounded-3xl border border-border/60 bg-card px-4 pb-2 pt-3 shadow-sm transition-colors focus-within:border-ring/60"
      >
        <ComposerTextarea
          bind:value={draft}
          bind:ref={textareaEl}
          {slash}
          cap={240}
          disabled={!app.inputAcquired || app.loadingSession}
          placeholder={app.loadingSession
            ? 'Opening session…'
            : app.inputAcquired
              ? app.awaitingTurn
                ? 'Type to queue — sends when the agent finishes'
                : 'Ask anything — Enter to send, Shift+Enter for newline. / for commands.'
              : 'Input lease not held — open a live session to write'}
        />
        <div class="flex items-center justify-between gap-2">
          <div class="flex items-center gap-1.5">
            <ComposerActions
              disabled={!app.inputAcquired || app.loadingSession}
              slashSupported={supportsSlashCommands(app.currentAgent)}
              onAttach={(path) => slash.insertAtCaret(`@${path} `)}
              onPickSkill={slash.openSlashFromMenu}
              onError={(msg) => (app.lastError = msg)}
            />
            {#if app.currentAgent && pickerModel}
              <ModelPicker
                agent={pickerAgent}
                bind:model={pickerModel}
                catalog={effectiveCatalog}
                disabled={!app.inputAcquired}
                lockAgent
                onChange={applyModelChange}
              />
            {/if}
          </div>
          <div class="flex items-center gap-2">
            {#if app.awaitingTurn}
              <button
                type="button"
                aria-label="Stop generating"
                onclick={() => app.cancelTurn()}
                title="cancel turn (ACP session/cancel)"
                class="flex h-9 w-9 items-center justify-center rounded-full border border-border/60 bg-card text-foreground transition-opacity hover:opacity-90"
              >
                <Square class="h-3.5 w-3.5 animate-pulse" fill="currentColor" />
              </button>
            {/if}
            <button
              type="submit"
              aria-label={app.awaitingTurn ? 'Queue message' : 'Send message'}
              title={app.awaitingTurn ? 'Queue message (sends when agent finishes)' : 'Send message'}
              disabled={!app.inputAcquired || !draft.trim()}
              class="flex h-9 w-9 items-center justify-center rounded-full bg-foreground text-background transition-opacity hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-30"
            >
              <ArrowUp class="h-4 w-4" strokeWidth={2.5} />
            </button>
          </div>
        </div>
      </form>
    {/if}
  </div>
</div>
