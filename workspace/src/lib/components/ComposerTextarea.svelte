<script lang="ts">
  // The relative-positioned <textarea> + slash-command popover shared by
  // Composer.svelte and ChatView.svelte. The slash machinery lives in the
  // parent's `useSlashCommands` hook (passed in as `slash`); this component
  // owns only the markup, the autosize effect, and the keydown deferral that
  // lets the popover consume nav keys before the form sees Enter.
  import CommandPopover from '../CommandPopover.svelte';
  import { autosize } from '../utils';
  import type { useSlashCommands } from '../useSlashCommands.svelte';

  let {
    value = $bindable(),
    ref = $bindable(),
    slash,
    cap,
    disabled = false,
    placeholder,
  }: {
    /** The draft text — two-way bound to the parent. */
    value: string;
    /** The textarea element — exposed so the parent's hook can reach it. */
    ref?: HTMLTextAreaElement | undefined;
    /** The shared slash-command unit from `useSlashCommands`. */
    slash: ReturnType<typeof useSlashCommands>;
    /** Autosize cap in px (Composer 280, ChatView 240). */
    cap: number;
    disabled?: boolean;
    placeholder: string;
  } = $props();

  let popover: CommandPopover | null = $state(null);

  // Auto-grow the textarea to fit content (capped at `cap`).
  $effect(() => {
    void value;
    if (ref) autosize(ref, cap);
  });

  function onKeydown(e: KeyboardEvent) {
    // When the popover is open let it consume nav keys first; only Submit
    // (Enter without active popover) bubbles up to the form.
    if (slash.slash && popover?.onKeydown(e)) return;
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      (e.currentTarget as HTMLTextAreaElement).form?.requestSubmit();
    }
  }
</script>

<div class="relative">
  <textarea
    bind:this={ref}
    bind:value
    onkeydown={onKeydown}
    oninput={() => slash.refreshSlash()}
    onclick={() => slash.refreshSlash()}
    onblur={() => slash.closeSlash()}
    rows="2"
    {disabled}
    {placeholder}
    class="z-10 block min-h-[3rem] w-full resize-none cursor-text bg-transparent px-1 py-0 text-sm leading-6 text-foreground placeholder:text-muted-foreground/70 focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-50"
  ></textarea>
  {#if slash.slash}
    <CommandPopover
      bind:this={popover}
      query={slash.slash.query}
      onpick={slash.pickCommand}
      onclose={() => slash.closeSlash()}
    />
  {/if}
</div>
