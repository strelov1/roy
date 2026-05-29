<script lang="ts">
  import { Marked } from 'marked';
  import { app, type Group } from '../state.svelte';
  import { pickVerb } from '../spinnerVerbs';
  import { ChevronRight, Terminal } from '@lucide/svelte';
  import AppMark from '../AppMark.svelte';
  import Skeleton from '../Skeleton.svelte';
  import {
    bashCommand,
    callLine,
    groupTitle,
    isExpandable,
    nonEmptyInput,
    previewToolInput,
  } from '../tool-formatters';

  const md = new Marked({ gfm: true, breaks: true });
  function renderMarkdown(text: string): string {
    return md.parse(text, { async: false }) as string;
  }

  // Shared style for centered pill chips (date separators, system markers).
  const CENTER_CHIP =
    'self-center rounded-full border bg-muted/50 px-2.5 py-0.5 text-[0.7rem] text-muted-foreground';

  // Cached Intl formatters. Constructing one per call is ~10× slower than
  // reusing a module-scope instance, and chat re-renders on every streamed
  // chunk so the savings compound across hundreds of bubbles.
  const TIME_FMT = new Intl.DateTimeFormat(undefined, {
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  });
  const FULL_FMT = new Intl.DateTimeFormat(undefined, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  });
  const SAME_YEAR_DAY_FMT = new Intl.DateTimeFormat(undefined, {
    weekday: 'short',
    month: 'short',
    day: 'numeric',
  });
  const CROSS_YEAR_DAY_FMT = new Intl.DateTimeFormat(undefined, {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
  });

  // Groups are folded incrementally in `Transcript.appendToGroups` so a
  // streaming turn is O(N) end-to-end instead of O(N²) per chunk.
  const groups: Group[] = $derived(app.groups);

  // Guard for missing/invalid wire ts_ms (malformed or older entries).
  function isValidTs(ts: number | null | undefined): ts is number {
    return typeof ts === 'number' && Number.isFinite(ts) && ts > 0;
  }
  // Local-TZ day bucket so day boundaries match the user's wall clock.
  function dayKey(ts: number): string {
    const d = new Date(ts);
    return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
  }
  function formatTime(ts: number | undefined): string {
    return isValidTs(ts) ? TIME_FMT.format(ts) : '';
  }
  function formatFullDateTime(ts: number | undefined): string {
    return isValidTs(ts) ? FULL_FMT.format(ts) : '';
  }
  function formatDayLabel(
    ts: number,
    todayKey: string,
    yesterdayKey: string,
    currentYear: number,
  ): string {
    const key = dayKey(ts);
    if (key === todayKey) return 'Today';
    if (key === yesterdayKey) return 'Yesterday';
    const fmt = new Date(ts).getFullYear() === currentYear ? SAME_YEAR_DAY_FMT : CROSS_YEAR_DAY_FMT;
    return fmt.format(ts);
  }

  type RenderItem =
    | { type: 'date'; label: string; key: string }
    | { type: 'group'; group: Group; key: string };

  // Reactive "now" so Today/Yesterday labels self-correct after midnight on
  // a long-open session, instead of staying frozen at the moment `groups`
  // last changed. Timer fires once per day boundary, not per minute.
  let nowMs = $state(Date.now());
  $effect(() => {
    const d = new Date(nowMs);
    const nextMidnight = new Date(
      d.getFullYear(),
      d.getMonth(),
      d.getDate() + 1,
    ).getTime();
    const id = setTimeout(() => { nowMs = Date.now(); }, nextMidnight - nowMs + 1000);
    return () => clearTimeout(id);
  });

  const renderItems: RenderItem[] = $derived.by(() => {
    const todayKey = dayKey(nowMs);
    const yesterdayKey = dayKey(nowMs - 86_400_000);
    const currentYear = new Date(nowMs).getFullYear();
    const items: RenderItem[] = [];
    let lastDay: string | null = null;
    for (const g of groups) {
      if (isValidTs(g.ts_ms)) {
        const day = dayKey(g.ts_ms);
        if (day !== lastDay) {
          items.push({
            type: 'date',
            label: formatDayLabel(g.ts_ms, todayKey, yesterdayKey, currentYear),
            key: `d${day}`,
          });
          lastDay = day;
        }
      }
      items.push({ type: 'group', group: g, key: g.key });
    }
    return items;
  });

  const SPINNER_GLYPHS = ['·', '✢', '✳', '✶', '✻', '✽'] as const;
  let elapsedSec = $state(0);
  let spinnerIdx = $state(0);
  let turnStartedAt = $state<number | null>(null);
  let currentVerb = $state('Thinking');
  let thoughtStartMs = $state<number | null>(null);
  let thoughtElapsedMs = $state(0);

  $effect(() => {
    if (app.awaitingTurn && turnStartedAt === null) {
      turnStartedAt = Date.now();
      elapsedSec = 0;
      spinnerIdx = 0;
      currentVerb = pickVerb();
      thoughtStartMs = null;
      thoughtElapsedMs = 0;
    } else if (!app.awaitingTurn) {
      turnStartedAt = null;
      elapsedSec = 0;
    }
  });
  $effect(() => {
    if (!app.awaitingTurn) return;
    const last = app.entries[app.entries.length - 1];
    if (!last) return;
    if (last.event.type === 'assistant_thought' && thoughtStartMs === null) {
      thoughtStartMs = Date.now();
    } else if (
      last.event.type === 'assistant_text' &&
      thoughtStartMs !== null &&
      thoughtElapsedMs === 0
    ) {
      thoughtElapsedMs = Date.now() - thoughtStartMs;
    }
  });
  $effect(() => {
    if (!app.awaitingTurn || turnStartedAt === null) return;
    const id = setInterval(() => {
      elapsedSec = Math.floor((Date.now() - (turnStartedAt as number)) / 1000);
      spinnerIdx = (spinnerIdx + 1) % SPINNER_GLYPHS.length;
    }, 120);
    return () => clearInterval(id);
  });

  function fmtTokens(n: number): string {
    if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + 'M';
    if (n >= 1_000) return (n / 1_000).toFixed(1) + 'k';
    return String(n);
  }

  function thoughtTail(text: string): string {
    const stripped = text.replace(/\*\*/g, '').replace(/\s+/g, ' ').trim();
    if (stripped.length <= 80) return stripped;
    return '…' + stripped.slice(-80);
  }

  const lastGroupKey = $derived(groups.length > 0 ? groups[groups.length - 1].key : null);
</script>

<div class="mx-auto flex max-w-4xl flex-col gap-3 px-4 py-6">
  {#if app.pendingFirstPrompt && app.pendingFirstPrompt.session === app.currentSession}
    <!-- Optimistic first-prompt render: shows the user bubble +
         thinking pulse the instant `spawn` returns, while the rest
         of the wire dance (snapshot + attach + acquire) finishes
         in the background. Cleared in onFrame once the real
         user_prompt frame lands. -->
    <article class="self-end max-w-[80%] rounded-2xl rounded-br-md bg-secondary px-4 py-2.5 text-sm leading-relaxed text-secondary-foreground">
      <pre class="m-0 whitespace-pre-wrap break-words font-sans">{app.pendingFirstPrompt.text}</pre>
    </article>
    <div class="self-start inline-flex items-center gap-2 px-2 py-1 text-xs text-muted-foreground">
      <AppMark class="w-8 shrink-0" animate />
      <span>Spinning up session…</span>
    </div>
  {:else if app.loadingSession && app.entries.length === 0}
    <!-- Shape-preserving placeholders so the layout doesn't jump
         when the snapshot finally arrives. Three bubbles of
         alternating sides + sizes match the typical opening turn. -->
    <Skeleton class="self-end h-9 w-2/3 rounded-2xl rounded-br-md" />
    <div class="self-start flex w-[88%] flex-col gap-2">
      <Skeleton class="h-4 w-full rounded" />
      <Skeleton class="h-4 w-11/12 rounded" />
      <Skeleton class="h-4 w-3/4 rounded" />
    </div>
    <Skeleton class="self-end h-7 w-1/3 rounded-2xl rounded-br-md" />
    <div class="self-start flex w-3/4 flex-col gap-2">
      <Skeleton class="h-4 w-full rounded" />
      <Skeleton class="h-4 w-5/6 rounded" />
    </div>
  {/if}
  {#snippet timeBadge(ts: number, extraClass: string)}
    <time
      class={['text-[0.65rem] tabular-nums', extraClass]}
      datetime={isValidTs(ts) ? new Date(ts).toISOString() : undefined}
      title={formatFullDateTime(ts)}
    >{formatTime(ts)}</time>
  {/snippet}
  {#each renderItems as r (r.key)}
    {#if r.type === 'date'}
      <div class={[CENTER_CHIP, 'my-1 font-medium']}>{r.label}</div>
    {:else}
      {@const item = r.group}
      {#if item.kind === 'user'}
        <article class="self-end max-w-[80%] flex items-end gap-2 rounded-2xl rounded-br-md bg-secondary px-4 py-2.5 text-sm leading-relaxed text-secondary-foreground">
          <pre class="m-0 min-w-0 flex-1 whitespace-pre-wrap break-words font-sans">{item.text}</pre>
          {@render timeBadge(item.ts_ms, 'shrink-0 pb-0.5 leading-none text-muted-foreground')}
        </article>
      {:else if item.kind === 'assistant'}
        <article class="self-start max-w-[88%] px-1 py-1 text-sm leading-relaxed text-foreground">
          <div class="md">{@html renderMarkdown(item.text)}</div>
          {@render timeBadge(item.ts_ms, 'mt-1 block text-right text-muted-foreground')}
        </article>
      {:else if item.kind === 'thought'}
        {@const active = item.key === lastGroupKey && app.awaitingTurn}
        <details
          class="self-start max-w-[88%] text-xs text-muted-foreground"
          open={active}
        >
          <summary class="flex cursor-pointer items-center gap-2 rounded-md px-2 py-1 hover:bg-muted/50 [&::-webkit-details-marker]:hidden [&::marker]:hidden">
            <span
              class={[
                'font-mono text-[0.85rem] font-semibold',
                active ? 'star-glow' : 'text-muted-foreground/60',
              ]}
            >
              {active ? SPINNER_GLYPHS[spinnerIdx] : '✶'}
            </span>
            <span class={['font-medium', active && 'shimmer']}>Thinking</span>
            {#if active}
              <span class="font-mono text-[0.7rem] text-muted-foreground/70">
                ({elapsedSec}s)
              </span>
              <span class="ml-1 max-w-[24ch] truncate italic opacity-80">
                {thoughtTail(item.text)}
              </span>
            {/if}
            {@render timeBadge(item.ts_ms, 'ml-auto text-muted-foreground/70')}
          </summary>
          <div class="md mt-1 max-h-56 overflow-y-auto border-l-2 border-border pl-3 py-1 text-muted-foreground">
            {@html renderMarkdown(item.text)}
          </div>
        </details>
      {:else if item.kind === 'tools'}
        {@const title = groupTitle(item.family, item.calls)}
        {#if !isExpandable(item.family, item.calls)}
          <div class="self-start flex items-center gap-2 rounded-lg border bg-muted/50 px-3 py-2 text-sm text-muted-foreground">
            <Terminal class="size-4 shrink-0" />
            <span>{title}</span>
            {@render timeBadge(item.ts_ms, 'ml-2 text-muted-foreground/70')}
          </div>
        {:else}
          <details class="self-start max-w-[90%]">
            <summary class="flex cursor-pointer items-center gap-2 rounded-lg border bg-muted/50 px-3 py-2 text-sm text-muted-foreground hover:bg-muted/70 [&::-webkit-details-marker]:hidden [&::marker]:hidden [&[open]>span>svg.chev]:rotate-90">
              <Terminal class="size-4 shrink-0" />
              <span class="flex items-center gap-1.5">
                {title}
                <ChevronRight class="chev size-3.5 shrink-0 transition-transform" />
              </span>
              {@render timeBadge(item.ts_ms, 'ml-auto text-muted-foreground/70')}
            </summary>
            {#if item.family === 'bash'}
              <div class="mt-2 overflow-hidden rounded-md border bg-background">
                <div class="border-b bg-muted/40 px-3 py-1.5 text-[0.65rem] font-medium uppercase tracking-wider text-muted-foreground/80">
                  Shell
                </div>
                {#each item.calls as c, i (i)}
                  <pre class={['m-0 whitespace-pre-wrap break-words px-3 py-2 font-mono text-xs', i > 0 && 'border-t']}>$ {bashCommand(c.input) ?? ''}</pre>
                {/each}
              </div>
            {:else if item.family === 'fs'}
              <ul class="mt-2 ml-6 space-y-1 text-xs text-muted-foreground">
                {#each item.calls as c, i (i)}
                  <li>{callLine(c)}</li>
                {/each}
              </ul>
            {:else}
              <ul class="mt-2 ml-6 space-y-1 text-xs text-muted-foreground">
                {#each item.calls as c, i (i)}
                  <li class="flex flex-wrap items-baseline gap-1.5">
                    <span class="font-medium text-foreground/80">{c.name}</span>
                    {#if nonEmptyInput(c.input)}
                      <code class="rounded bg-muted px-1.5 py-0.5 font-mono">{previewToolInput(c.input)}</code>
                    {/if}
                  </li>
                {/each}
              </ul>
            {/if}
          </details>
        {/if}
      {:else if item.kind === 'system'}
        <div class={CENTER_CHIP} title={formatFullDateTime(item.ts_ms)}>
          system · {item.subtype} · {formatTime(item.ts_ms)}
        </div>
      {:else if item.kind === 'note'}
        {@const isAsk = item.text.startsWith('[ask]')}
        <article
          class={[
            'self-stretch rounded-lg px-4 py-2.5 text-sm',
            isAsk
              ? 'border border-amber-400/40 bg-amber-400/5'
              : 'border border-primary/30 bg-primary/5',
          ]}
        >
          <div
            class={[
              'mb-1 flex items-center gap-1 text-[0.65rem] font-semibold uppercase tracking-wider',
              isAsk ? 'text-amber-700 dark:text-amber-300' : 'text-primary/80',
            ]}
          >
            <span>{isAsk ? 'awaiting reply' : 'background'}{#if item.sourceSession}
              {@const src = item.sourceSession}
              ·
              <a
                class="underline"
                href={`/s/${src}`}
                onclick={(e) => {
                  if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey) return;
                  e.preventDefault();
                  void app.openSession(src);
                  window.history.pushState(null, '', `/s/${src}`);
                }}
              >{src.slice(0, 8)}</a>
            {/if}</span>
            {@render timeBadge(item.ts_ms, 'ml-auto font-normal normal-case tracking-normal text-muted-foreground')}
          </div>
          <pre class="m-0 whitespace-pre-wrap break-words font-sans">{item.text}</pre>
        </article>
      {:else if item.kind === 'error'}
        <article class="self-stretch rounded-lg border border-destructive/40 bg-destructive/10 px-4 py-2.5 text-sm text-destructive">
          <div class="mb-1 flex items-center gap-1 text-[0.65rem] font-semibold uppercase tracking-wider opacity-80">
            <span>error</span>
            {@render timeBadge(item.ts_ms, 'ml-auto font-normal normal-case tracking-normal opacity-90')}
          </div>
          <pre class="m-0 whitespace-pre-wrap break-words font-mono text-xs">turn ended with stop_reason={item.stopReason}</pre>
        </article>
      {:else}
        <article class="self-stretch rounded-lg border bg-muted/30 px-4 py-2.5 text-xs text-muted-foreground">
          <div class="mb-1 flex items-center gap-1 text-[0.65rem] font-semibold uppercase tracking-wider">
            <span>raw</span>
            {@render timeBadge(item.ts_ms, 'ml-auto font-normal normal-case tracking-normal opacity-80')}
          </div>
          <pre class="m-0 overflow-x-auto whitespace-pre-wrap break-words font-mono">{JSON.stringify(item.value, null, 2)}</pre>
        </article>
      {/if}
    {/if}
  {/each}

  {#if app.awaitingTurn}
    {@const thinking = thoughtStartMs !== null && thoughtElapsedMs === 0}
    <div class="self-start inline-flex items-baseline gap-2 px-2 py-1 text-xs text-muted-foreground">
      <span class="star-glow font-mono text-[0.85rem] font-semibold">
        {SPINNER_GLYPHS[spinnerIdx]}
      </span>
      <span class="shimmer font-medium">{currentVerb}…</span>
      <span class="font-mono text-[0.7rem] text-muted-foreground/70">
        ({elapsedSec}s
        {#if app.currentUsage && app.currentUsage.output_tokens > 0}
          · ↑ {fmtTokens(app.currentUsage.output_tokens)} tokens
        {/if}
        {#if thinking}
          · still thinking
        {:else if thoughtElapsedMs > 0}
          · thought for {Math.round(thoughtElapsedMs / 1000)}s
        {/if})
      </span>
    </div>
  {/if}
</div>

<style>
  /* Shimmer over the spinner verb. background-clip masks the text shape onto
     a moving gradient — no JS, no repaints beyond the GPU-composited layer. */
  .shimmer {
    background: linear-gradient(
      90deg,
      var(--color-muted-foreground) 0%,
      var(--color-muted-foreground) 35%,
      var(--color-foreground) 50%,
      var(--color-muted-foreground) 65%,
      var(--color-muted-foreground) 100%
    );
    background-size: 200% 100%;
    background-clip: text;
    -webkit-background-clip: text;
    color: transparent;
    -webkit-text-fill-color: transparent;
    animation: shimmer-pan 2.4s linear infinite;
  }
  @keyframes shimmer-pan {
    0%   { background-position: 200% 0; }
    100% { background-position: -200% 0; }
  }

  .star-glow {
    color: var(--color-foreground);
    animation: star-pulse 1.8s ease-in-out infinite;
  }
  @keyframes star-pulse {
    0%, 100% { opacity: 0.7; }
    50%      { opacity: 1; }
  }

  @media (prefers-reduced-motion: reduce) {
    .star-glow { animation: none; opacity: 1; }
    .shimmer {
      animation: none;
      background: none;
      color: var(--color-foreground);
      -webkit-text-fill-color: currentColor;
    }
  }

  /* Markdown rendering — scoped, applied with :global to reach @html output. */
  .md :global(*:first-child) { margin-top: 0; }
  .md :global(*:last-child)  { margin-bottom: 0; }
  .md :global(p) { margin: 0 0 0.5rem; line-height: 1.55; }
  .md :global(h1),
  .md :global(h2),
  .md :global(h3),
  .md :global(h4) {
    margin: 0.8rem 0 0.35rem;
    line-height: 1.3;
    font-weight: 600;
  }
  .md :global(h1) { font-size: 1.1rem; }
  .md :global(h2) { font-size: 1.0rem; }
  .md :global(h3),
  .md :global(h4) { font-size: 0.95rem; }
  .md :global(ul),
  .md :global(ol) { margin: 0 0 0.5rem; padding-left: 1.25rem; }
  .md :global(li) { margin: 0.1rem 0; }
  .md :global(li > p) { margin: 0; }
  .md :global(a) {
    color: oklch(0.6 0.18 280);
    text-decoration: underline;
    text-underline-offset: 2px;
  }
  .md :global(strong) { font-weight: 600; }
  .md :global(em) { font-style: italic; }
  .md :global(code) {
    font-family: var(--font-mono);
    font-size: 0.85em;
    padding: 0.05rem 0.3rem;
    border-radius: 4px;
    background: color-mix(in oklab, currentColor 10%, transparent);
  }
  .md :global(pre) {
    background: color-mix(in oklab, currentColor 12%, transparent);
    padding: 0.65rem 0.8rem;
    border-radius: 6px;
    overflow-x: auto;
    margin: 0.4rem 0;
    font-size: 0.85em;
  }
  .md :global(pre code) {
    background: transparent;
    padding: 0;
    font-size: 1em;
  }
  .md :global(blockquote) {
    margin: 0.4rem 0;
    padding: 0.2rem 0.7rem;
    border-left: 3px solid var(--color-border);
    color: var(--color-muted-foreground);
  }
  .md :global(table) {
    border-collapse: collapse;
    margin: 0.4rem 0;
    font-size: 0.85em;
  }
  .md :global(th),
  .md :global(td) {
    border: 1px solid var(--color-border);
    padding: 0.25rem 0.55rem;
    text-align: left;
  }
  .md :global(hr) {
    border: none;
    border-top: 1px solid var(--color-border);
    margin: 0.6rem 0;
  }
</style>
