<script lang="ts">
  import type { Snippet } from 'svelte';
  import { Bot } from '@lucide/svelte';
  import ProviderIcon from './ProviderIcon.svelte';
  import { agentIcon, modelProvider } from './provider-icons';
  import { cn } from './utils';

  // Composite agent mark: primary brand of the agent, plus a small badge
  // showing the *model's* provider when the agent is a router (opencode).
  // For dedicated agents (claude/gemini/codex) the model brand always
  // matches the agent — we render the single icon to avoid visual noise.
  // When `agent` is null or unknown, render `fallback` if provided, else a
  // neutral Bot glyph so the icon slot keeps its dimensions and the
  // surrounding flex row doesn't collapse.
  let {
    agent,
    model = null,
    class: cls = 'size-3.5',
    fallback,
  }: {
    agent: string | null | undefined;
    model?: string | null;
    class?: string;
    fallback?: Snippet;
  } = $props();

  const primary = $derived(agentIcon(agent));
  const secondary = $derived(
    primary === 'opencode' ? modelProvider(model) : null,
  );
</script>

{#if primary && secondary}
  <span class={cn('relative inline-block shrink-0', cls)}>
    <ProviderIcon name={primary} class="size-full" />
    <span
      class="absolute -right-1 -bottom-1 inline-flex size-[60%] items-center justify-center rounded-full bg-background"
    >
      <ProviderIcon name={secondary} class="size-full" />
    </span>
  </span>
{:else if primary}
  <ProviderIcon name={primary} class={cls} />
{:else if fallback}
  {@render fallback()}
{:else}
  <Bot class={cls} />
{/if}
