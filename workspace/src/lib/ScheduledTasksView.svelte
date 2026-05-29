<script lang="ts">
  import { onMount } from 'svelte';
  import { Button } from '$lib/components/ui/button';
  import { CalendarClock, PanelLeft, RefreshCw, Bot, Pause, Play } from '@lucide/svelte';
  import {
    HttpError,
    scheduler,
    type SchedulerAgent,
    type SchedulerTrigger,
    type SchedulerFire,
  } from './management-client';
  import { errMsg } from './utils';

  const FIRES_LIMIT = 50;

  let { onOpenSidebar }: { onOpenSidebar?: () => void } = $props();

  type LoadStatus =
    | { kind: 'ok' }
    | { kind: 'error'; message: string }
    | { kind: 'unavailable' };

  let agents = $state<SchedulerAgent[]>([]);
  let triggers = $state<SchedulerTrigger[]>([]);
  let fires = $state<SchedulerFire[]>([]);
  let loading = $state(false);
  let status = $state<LoadStatus>({ kind: 'ok' });

  const agentNameById = $derived(new Map(agents.map((a) => [a.id, a.name])));
  const activeTriggers = $derived(triggers.filter((t) => t.paused === 0));
  const pausedTriggers = $derived(triggers.filter((t) => t.paused !== 0));

  async function refresh() {
    if (loading) return;
    loading = true;
    try {
      const [a, t, f] = await Promise.all([
        scheduler.agents(),
        scheduler.triggers(),
        scheduler.fires({ limit: FIRES_LIMIT }),
      ]);
      agents = a;
      triggers = t;
      fires = f;
      status = { kind: 'ok' };
    } catch (e) {
      status =
        e instanceof HttpError && e.status === 503
          ? { kind: 'unavailable' }
          : { kind: 'error', message: errMsg(e) };
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    void refresh();
  });

  function formatRelative(iso: string | null): string {
    if (!iso) return '—';
    const diffMs = new Date(iso).getTime() - Date.now();
    const abs = Math.abs(diffMs);
    const past = diffMs < 0;
    if (abs < 60_000) return past ? 'just now' : '<1m';
    const minutes = Math.floor(abs / 60_000);
    if (minutes < 60) return past ? `${minutes}m ago` : `in ${minutes}m`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return past ? `${hours}h ago` : `in ${hours}h`;
    const days = Math.floor(hours / 24);
    return past ? `${days}d ago` : `in ${days}d`;
  }

  function fmtAbsolute(iso: string | null): string {
    return iso ? new Date(iso).toLocaleString() : '';
  }

  function describeTrigger(t: SchedulerTrigger): string {
    if (t.kind === 'oneshot') {
      return `fires once at ${fmtAbsolute(t.next_fire_at)}`;
    }
    if (t.cron_expr) {
      return `cron ${t.cron_expr} (${t.timezone})`;
    }
    return t.kind;
  }
</script>

<div class="flex h-full min-h-0 flex-col">
  <header class="flex items-center gap-2 border-b border-border px-4 py-3">
    {#if onOpenSidebar}
      <button
        type="button"
        onclick={onOpenSidebar}
        aria-label="Show sidebar"
        title="Show sidebar"
        class="inline-flex size-9 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground md:hidden"
      >
        <PanelLeft class="size-4" />
      </button>
    {/if}
    <h1 class="flex flex-1 items-center gap-2 truncate text-base font-semibold">
      <CalendarClock class="size-4 shrink-0 text-muted-foreground" />
      Scheduled
    </h1>
    <Button variant="ghost" size="sm" onclick={() => void refresh()} disabled={loading}>
      <RefreshCw class={['size-4', loading && 'animate-spin']} />
      Refresh
    </Button>
  </header>

  <div class="min-h-0 flex-1 overflow-y-auto px-4 py-4">
    {#if status.kind === 'error'}
      <div
        role="alert"
        class="mb-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
      >
        {status.message}
      </div>
    {/if}

    {#if status.kind === 'unavailable'}
      <div class="rounded-lg border border-dashed border-border p-8 text-center">
        <p class="mb-2 text-sm font-medium">Scheduler not initialized</p>
        <p class="text-sm text-muted-foreground">
          Start <code class="rounded bg-muted px-1.5 py-0.5 font-mono text-xs">roy scheduler serve</code>
          at least once to create
          <code class="rounded bg-muted px-1.5 py-0.5 font-mono text-xs">~/.local/state/roy-scheduler/state.db</code>,
          then refresh.
        </p>
      </div>
    {:else if loading && triggers.length === 0 && fires.length === 0}
      <p class="text-sm text-muted-foreground">Loading…</p>
    {:else if triggers.length === 0 && fires.length === 0}
      <div class="rounded-lg border border-dashed border-border p-8 text-center">
        <p class="mb-2 text-sm font-medium">No scheduled tasks yet</p>
        <p class="text-sm text-muted-foreground">
          Register an agent and add a trigger via
          <code class="rounded bg-muted px-1.5 py-0.5 font-mono text-xs">roy scheduler triggers add</code>.
        </p>
      </div>
    {:else}
      <div class="flex flex-col gap-6">
        {#if activeTriggers.length > 0}
          <section>
            <h2
              class="mb-2 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground"
            >
              Active ({activeTriggers.length})
            </h2>
            <ul class="flex flex-col gap-2">
              {#each activeTriggers as t (t.id)}
                <li class="rounded-lg border border-border bg-card p-3">
                  <div class="flex flex-wrap items-baseline gap-x-2 gap-y-1">
                    <span class="inline-flex items-center gap-1 text-sm font-semibold">
                      <Bot class="size-3.5 text-muted-foreground" />
                      {agentNameById.get(t.agent_id) ?? t.agent_id}
                    </span>
                    {#if t.kind === 'oneshot'}
                      <span
                        class="rounded bg-muted px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-muted-foreground"
                      >
                        one-shot
                      </span>
                    {:else}
                      <span
                        class="rounded bg-muted px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-muted-foreground"
                      >
                        cron
                      </span>
                    {/if}
                    <span class="text-xs text-muted-foreground">{describeTrigger(t)}</span>
                  </div>
                  <div
                    class="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-[11px] text-muted-foreground"
                  >
                    <span title={fmtAbsolute(t.next_fire_at)}>
                      <Play class="mr-0.5 inline size-3" />
                      Next: {formatRelative(t.next_fire_at)}
                    </span>
                    {#if t.last_fire_at}
                      <span title={fmtAbsolute(t.last_fire_at)}>
                        Last: {formatRelative(t.last_fire_at)}
                      </span>
                    {/if}
                    {#if t.last_error}
                      <span class="text-destructive" title={t.last_error}>
                        Last error: {t.last_error.slice(0, 60)}
                      </span>
                    {/if}
                  </div>
                </li>
              {/each}
            </ul>
          </section>
        {/if}

        {#if pausedTriggers.length > 0}
          <section>
            <h2
              class="mb-2 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground"
            >
              Paused ({pausedTriggers.length})
            </h2>
            <ul class="flex flex-col gap-2">
              {#each pausedTriggers as t (t.id)}
                <li class="rounded-lg border border-border bg-card p-3 opacity-70">
                  <div class="flex flex-wrap items-baseline gap-x-2 gap-y-1">
                    <span class="inline-flex items-center gap-1 text-sm font-semibold">
                      <Bot class="size-3.5 text-muted-foreground" />
                      {agentNameById.get(t.agent_id) ?? t.agent_id}
                    </span>
                    <span
                      class="inline-flex items-center gap-1 rounded bg-muted px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-muted-foreground"
                    >
                      <Pause class="size-3" />
                      paused
                    </span>
                    <span class="text-xs text-muted-foreground">{describeTrigger(t)}</span>
                  </div>
                  {#if t.last_error}
                    <p class="mt-1 text-[11px] text-destructive">Last error: {t.last_error}</p>
                  {/if}
                </li>
              {/each}
            </ul>
          </section>
        {/if}

        {#if fires.length > 0}
          <section>
            <h2
              class="mb-2 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground"
            >
              Recent fires (last {fires.length})
            </h2>
            <ul class="flex flex-col gap-1.5">
              {#each fires as f (f.id)}
                <li
                  class="flex flex-wrap items-baseline gap-x-2 gap-y-0.5 rounded-md border border-border/60 bg-background px-3 py-2 text-xs"
                >
                  <span
                    class={[
                      'rounded px-1.5 py-0.5 text-[10px] font-mono uppercase tracking-wide',
                      f.status === 'ok'
                        ? 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-300'
                        : f.status === 'error' || f.status === 'timeout'
                          ? 'bg-destructive/15 text-destructive'
                          : 'bg-muted text-muted-foreground',
                    ]}
                  >
                    {f.status}
                  </span>
                  <span class="font-medium text-foreground">
                    {agentNameById.get(f.agent_id) ?? f.agent_id}
                  </span>
                  <span class="text-muted-foreground" title={fmtAbsolute(f.started_at)}>
                    {formatRelative(f.started_at)}
                  </span>
                  {#if f.cost_usd !== null}
                    <span class="text-muted-foreground">${f.cost_usd.toFixed(4)}</span>
                  {/if}
                  {#if f.error_message}
                    <span class="basis-full text-destructive" title={f.error_message}>
                      {f.error_message.slice(0, 120)}
                    </span>
                  {:else if f.assistant_text}
                    <span class="basis-full truncate text-muted-foreground">
                      {f.assistant_text.slice(0, 200)}
                    </span>
                  {/if}
                </li>
              {/each}
            </ul>
          </section>
        {/if}
      </div>
    {/if}
  </div>
</div>
