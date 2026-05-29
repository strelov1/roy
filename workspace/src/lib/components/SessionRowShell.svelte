<script lang="ts">
  // Real <button> as the click target + sibling action buttons in an
  // absolute overlay — avoids nesting <button> in <button>.
  import type { Snippet } from 'svelte';
  import { app } from '../state.svelte';
  import { cn } from '../utils';
  import InlineEditable from './InlineEditable.svelte';

  let {
    session,
    onPick,
    active = false,
    italic = false,
    title,
    class: cls = '',
    overlayPadding = 'pr-8',
    /** Controlled inline-edit state. Parent flips this true from a menu
     *  item (e.g. "Rename" in the three-dots dropdown); the row calls
     *  `onEditingChange(false)` on commit/cancel so the parent can clear
     *  its own per-row editing flag. */
    editing = false,
    onEditingChange,
    icon,
    actions,
  }: {
    session: string;
    onPick: (id: string) => void;
    active?: boolean;
    italic?: boolean;
    title?: string;
    class?: string;
    overlayPadding?: string;
    editing?: boolean;
    onEditingChange?: (next: boolean) => void;
    icon: Snippet;
    actions?: Snippet;
  } = $props();

  let displayed = $derived(app.titleFor(session));
  let editValue = $derived(app.titleOverride(session) ?? displayed);
  let renaming = $derived(app.renamingSessions[session] === true);

  function submitTitle(next: string) {
    onEditingChange?.(false);
    // Trim + RMW lives in app.setSessionTitle; an empty string clears the
    // override. Errors are surfaced via app.lastError.
    void app.setSessionTitle(session, next).catch(() => {});
  }

  function cancelEditing() {
    onEditingChange?.(false);
  }
</script>

<!-- Two row layouts: the normal button (single-click → open) and the edit
     form. Switching between them avoids nesting <input> inside <button>,
     which would be invalid HTML and break form semantics. -->
<div
  class={cn(
    'group/row relative flex items-center rounded-md transition-colors',
    active
      ? 'bg-sidebar-accent text-sidebar-accent-foreground'
      : 'hover:bg-sidebar-accent/60 hover:text-sidebar-accent-foreground',
    italic && !active && 'italic text-muted-foreground',
    cls,
  )}
>
  {#if editing}
    <div
      class={cn(
        'flex min-w-0 flex-1 items-center gap-2 rounded-md px-2 py-1.5 text-sm',
        actions && overlayPadding,
      )}
    >
      {@render icon()}
      <InlineEditable
        value={editValue}
        busy={renaming}
        ariaLabel="Rename session"
        placeholder="Session title — Enter to save, Esc to cancel"
        onSubmit={submitTitle}
        onCancel={cancelEditing}
      />
    </div>
  {:else}
    <button
      type="button"
      onclick={() => onPick(session)}
      class={cn(
        'flex min-w-0 flex-1 select-none items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm',
        actions && overlayPadding,
      )}
    >
      {@render icon()}
      <span class="min-w-0 flex-1 truncate" title={title ?? session}>
        {displayed}
      </span>
    </button>
  {/if}
  {#if actions}
    <div class="absolute right-0.5 top-1/2 flex -translate-y-1/2 items-center gap-0.5">
      {@render actions()}
    </div>
  {/if}
</div>
