<script lang="ts">
  import { commandsStore, type CommandInfo } from './commands.svelte';

  let {
    query,
    onpick,
    onclose,
  }: {
    /** The text after the triggering `/`. Empty string means "user just typed
     *  the slash and hasn't filtered yet" — show the full catalog. */
    query: string;
    /** Called with the chosen command name (no leading slash). */
    onpick: (name: string) => void;
    onclose: () => void;
  } = $props();

  // Lazy-load on first mount; the store dedups concurrent loads itself.
  $effect(() => {
    void commandsStore.load();
  });

  const filtered = $derived.by<CommandInfo[]>(() => {
    const q = query.trim().toLowerCase();
    if (!q) return commandsStore.list;
    return commandsStore.list.filter((c) => c.name.toLowerCase().startsWith(q));
  });

  // Active index follows filtered list, clamped on every re-derivation so
  // a shrinking list doesn't strand the highlight off-end.
  let active = $state(0);
  $effect(() => {
    if (active >= filtered.length) active = Math.max(0, filtered.length - 1);
  });

  export function onKeydown(e: KeyboardEvent): boolean {
    // Returns true when the popover consumed the event so the textarea
    // doesn't also see it (e.g. ArrowDown shouldn't move the caret).
    if (filtered.length === 0 && e.key !== 'Escape') return false;
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      active = (active + 1) % filtered.length;
      return true;
    }
    if (e.key === 'ArrowUp') {
      e.preventDefault();
      active = (active - 1 + filtered.length) % filtered.length;
      return true;
    }
    if (e.key === 'Enter' || e.key === 'Tab') {
      const choice = filtered[active];
      if (choice) {
        e.preventDefault();
        onpick(choice.name);
        return true;
      }
    }
    if (e.key === 'Escape') {
      e.preventDefault();
      onclose();
      return true;
    }
    return false;
  }
</script>

<div
  class="absolute bottom-full left-0 z-20 mb-2 max-h-72 w-full max-w-md overflow-y-auto rounded-lg border border-border bg-popover text-popover-foreground shadow-md"
  role="listbox"
  aria-label="Slash commands"
>
  {#if commandsStore.loading && commandsStore.list.length === 0}
    <div class="px-3 py-2 text-xs text-muted-foreground">Loading commands…</div>
  {:else if commandsStore.error}
    <div class="px-3 py-2 text-xs text-destructive">
      Couldn't load commands: {commandsStore.error}
    </div>
  {:else if filtered.length === 0}
    <div class="px-3 py-2 text-xs text-muted-foreground">No matching commands</div>
  {:else}
    {#each filtered as cmd, i (cmd.name + cmd.source)}
      <button
        type="button"
        role="option"
        aria-selected={i === active}
        onmousedown={(e) => {
          // mousedown beats focus-loss on the textarea so the click lands
          // before the popover's blur handler closes it.
          e.preventDefault();
          onpick(cmd.name);
        }}
        onmouseenter={() => (active = i)}
        class={[
          'flex w-full flex-col gap-0.5 px-3 py-2 text-left text-sm transition-colors',
          i === active ? 'bg-accent text-accent-foreground' : 'hover:bg-accent/50',
        ]}
      >
        <div class="flex items-baseline justify-between gap-3">
          <span class="font-mono text-foreground">/{cmd.name}</span>
          <span class="text-[0.7rem] text-muted-foreground">{cmd.source}</span>
        </div>
        {#if cmd.description}
          <span class="line-clamp-2 text-xs text-muted-foreground">{cmd.description}</span>
        {/if}
      </button>
    {/each}
  {/if}
</div>
