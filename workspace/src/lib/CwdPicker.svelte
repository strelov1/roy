<script lang="ts">
  import { onMount } from 'svelte';
  import * as Popover from '$lib/components/ui/popover';
  import { Input } from '$lib/components/ui/input';
  import { Button } from '$lib/components/ui/button';
  import { Folder, Check, X, ChevronDown } from '@lucide/svelte';
  import { LS, lsGetJSON, lsSetJSON } from './utils';

  let {
    value = $bindable(),
    disabled = false,
  }: {
    value: string;
    disabled?: boolean;
  } = $props();

  const MAX_RECENT = 8;

  let open = $state(false);
  let draft = $state(value);
  let recent = $state<string[]>([]);
  let inputEl: HTMLInputElement | undefined = $state();

  // Pull recent cwds from localStorage once at mount. `onMount` (vs
  // `$effect`) makes "load once" explicit and immune to future edits
  // accidentally introducing a reactive dependency.
  onMount(() => {
    const parsed = lsGetJSON<unknown[]>(LS.recentCwds, []);
    if (Array.isArray(parsed)) {
      recent = parsed.filter((s): s is string => typeof s === 'string' && s.length > 0);
    }
  });

  // Reset draft to current value when the popover opens, then auto-focus the
  // input so the user can paste a path right away.
  $effect(() => {
    if (open) {
      draft = value;
      queueMicrotask(() => inputEl?.focus());
    }
  });

  function pushRecent(path: string) {
    if (!path) return;
    const next = [path, ...recent.filter((p) => p !== path)].slice(0, MAX_RECENT);
    recent = next;
    lsSetJSON(LS.recentCwds, next);
  }

  function commit(path: string) {
    const trimmed = path.trim();
    value = trimmed;
    if (trimmed) pushRecent(trimmed);
    open = false;
  }

  function clear() {
    value = '';
    open = false;
  }

  function removeRecent(path: string) {
    const next = recent.filter((p) => p !== path);
    recent = next;
    lsSetJSON(LS.recentCwds, next);
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      e.preventDefault();
      commit(draft);
    } else if (e.key === 'Escape') {
      e.preventDefault();
      open = false;
    }
  }

  // Compact display for the trigger pill — collapse the home prefix and
  // keep only the last two path segments so long paths don't bloat the row.
  function shortPath(path: string): string {
    if (!path) return 'default cwd';
    const trimmed = path.replace(/\/+$/, '');
    const parts = trimmed.split('/').filter(Boolean);
    if (parts.length <= 2) return trimmed;
    return '…/' + parts.slice(-2).join('/');
  }

  const display = $derived(shortPath(value));
</script>

<Popover.Root bind:open>
  <Popover.Trigger
    {disabled}
    title={value || 'Uses daemon default (ROY_CWD / current_dir)'}
    class="inline-flex h-8 max-w-[18rem] items-center gap-1.5 truncate rounded-full border border-border bg-background px-3 text-xs font-medium hover:bg-muted disabled:cursor-not-allowed disabled:opacity-50"
  >
    <Folder class="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
    <span class={['truncate font-mono', !value && 'text-muted-foreground']}>{display}</span>
    <ChevronDown class="size-3.5 shrink-0 text-muted-foreground" aria-hidden="true" />
  </Popover.Trigger>

  <Popover.Content
    side="top"
    align="start"
    sideOffset={8}
    class="w-[22rem] rounded-2xl border border-border/60 bg-popover p-3 text-popover-foreground shadow-xl"
  >
    <label for="cwd-input" class="mb-1 block text-[0.7rem] font-medium uppercase tracking-wider text-muted-foreground">
      Working directory
    </label>
    <div class="flex items-center gap-1.5">
      <Input
        id="cwd-input"
        bind:ref={inputEl}
        type="text"
        bind:value={draft}
        onkeydown={onKeydown}
        placeholder="/Users/me/projects/something"
        spellcheck="false"
        class="h-8 flex-1 rounded-md font-mono text-xs"
      />
      <Button
        type="button"
        size="icon-sm"
        title="Apply (Enter)"
        aria-label="Apply"
        onclick={() => commit(draft)}
        disabled={draft.trim() === value.trim()}
      >
        <Check class="size-3.5" />
      </Button>
    </div>
    <p class="mt-1.5 text-[0.7rem] text-muted-foreground">
      Empty → daemon's <code class="rounded bg-muted px-1 font-mono">ROY_CWD</code> / current_dir.
    </p>

    {#if value}
      <button
        type="button"
        onclick={clear}
        class="mt-2 inline-flex items-center gap-1 rounded-md px-1.5 py-1 text-[0.7rem] text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
      >
        <X class="size-3" />
        Use daemon default
      </button>
    {/if}

    {#if recent.length > 0}
      <div class="mt-3 border-t border-border/60 pt-2">
        <div class="mb-1 px-1 text-[0.65rem] font-semibold uppercase tracking-wider text-muted-foreground">
          Recent
        </div>
        <ul class="flex max-h-56 flex-col overflow-y-auto">
          {#each recent as path (path)}
            {@const active = path === value}
            <li class="group/recent flex items-center gap-0.5">
              <button
                type="button"
                onclick={() => commit(path)}
                title={path}
                class={[
                  'flex min-w-0 flex-1 items-center gap-2 rounded-md px-1.5 py-1.5 text-left transition-colors',
                  active
                    ? 'bg-muted text-foreground'
                    : 'text-muted-foreground hover:bg-muted hover:text-foreground',
                ]}
              >
                <Folder class="size-3.5 shrink-0 opacity-70" aria-hidden="true" />
                <span class="truncate font-mono text-xs">{path}</span>
              </button>
              <button
                type="button"
                aria-label="Remove from recents"
                title="Remove"
                onclick={() => removeRecent(path)}
                class="flex size-6 shrink-0 items-center justify-center rounded text-muted-foreground opacity-0 transition-opacity hover:bg-foreground/10 hover:text-foreground group-hover/recent:opacity-100 focus-visible:opacity-100"
              >
                <X class="size-3" />
              </button>
            </li>
          {/each}
        </ul>
      </div>
    {/if}
  </Popover.Content>
</Popover.Root>
