<script lang="ts">
  import { app } from './state.svelte';
  import { ArrowUp, Check, ChevronDown, ChevronUp, Pencil, Trash2, X } from '@lucide/svelte';
  import { autosize } from './utils';

  let expanded = $state(true);
  let editingId = $state<string | null>(null);
  let draft = $state('');
  let editEl: HTMLTextAreaElement | undefined = $state();

  function startEdit(id: string, text: string) {
    editingId = id;
    draft = text;
    queueMicrotask(() => {
      editEl?.focus();
      editEl?.setSelectionRange(text.length, text.length);
    });
  }

  function commitEdit() {
    if (!editingId) return;
    app.updateQueued(editingId, draft);
    editingId = null;
    draft = '';
  }

  function cancelEdit() {
    editingId = null;
    draft = '';
  }

  function onEditKey(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      commitEdit();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      cancelEdit();
    }
  }

  // Auto-grow the inline edit textarea.
  $effect(() => {
    void draft;
    if (editEl) autosize(editEl);
  });
</script>

{#if app.queue.length > 0}
  <div class="mb-2 overflow-hidden rounded-3xl border border-border/60 bg-card">
    <button
      type="button"
      class="flex w-full items-center justify-between gap-2 px-4 py-3 text-sm"
      onclick={() => (expanded = !expanded)}
      aria-expanded={expanded}
      aria-controls="queue-panel-list"
    >
      <span class="flex items-center gap-2 text-violet-400">
        <span class="font-medium">{app.queue.length} queued</span>
      </span>
      {#if expanded}
        <ChevronUp class="h-4 w-4 text-muted-foreground" aria-hidden="true" />
      {:else}
        <ChevronDown class="h-4 w-4 text-muted-foreground" aria-hidden="true" />
      {/if}
    </button>

    {#if expanded}
      <ul id="queue-panel-list" class="divide-y divide-border/40 border-t border-border/40">
        {#each app.queue as item (item.id)}
          {@const editing = editingId === item.id}
          <li class="group flex items-start gap-3 px-4 py-2.5">
            <div class="flex min-w-0 flex-1 flex-col gap-1.5">
              {#if editing}
                <textarea
                  bind:this={editEl}
                  bind:value={draft}
                  onkeydown={onEditKey}
                  rows="1"
                  class="block w-full resize-none bg-transparent text-sm text-foreground focus:outline-none"
                ></textarea>
              {:else}
                <span class="whitespace-pre-wrap break-words text-sm text-foreground">
                  {item.text}
                </span>
              {/if}
            </div>

            <div
              class={[
                'ml-auto flex shrink-0 items-center gap-0.5 pt-0.5 text-muted-foreground',
                editing
                  ? ''
                  : 'opacity-60 transition-opacity group-hover:opacity-100 focus-within:opacity-100',
              ]}
            >
              {#if editing}
                <button
                  type="button"
                  aria-label="Save"
                  title="Save"
                  onclick={commitEdit}
                  class="flex h-7 w-7 items-center justify-center rounded-md transition-colors hover:bg-muted hover:text-foreground"
                >
                  <Check class="h-4 w-4" />
                </button>
                <button
                  type="button"
                  aria-label="Cancel"
                  title="Cancel"
                  onclick={cancelEdit}
                  class="flex h-7 w-7 items-center justify-center rounded-md transition-colors hover:bg-muted hover:text-foreground"
                >
                  <X class="h-4 w-4" />
                </button>
              {:else}
                <button
                  type="button"
                  aria-label="Edit"
                  title="Edit"
                  onclick={() => startEdit(item.id, item.text)}
                  class="flex h-7 w-7 items-center justify-center rounded-md transition-colors hover:bg-muted hover:text-foreground"
                >
                  <Pencil class="h-4 w-4" />
                </button>
                <button
                  type="button"
                  aria-label="Send next"
                  title="Send next"
                  onclick={() => app.promoteQueued(item.id)}
                  class="flex h-7 w-7 items-center justify-center rounded-md transition-colors hover:bg-muted hover:text-foreground"
                >
                  <ArrowUp class="h-4 w-4" />
                </button>
                <button
                  type="button"
                  aria-label="Remove"
                  title="Remove"
                  onclick={() => app.removeQueued(item.id)}
                  class="flex h-7 w-7 items-center justify-center rounded-md transition-colors hover:bg-muted hover:text-foreground"
                >
                  <Trash2 class="h-4 w-4" />
                </button>
              {/if}
            </div>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
{/if}
