<script lang="ts">
  let {
    error,
  }: {
    error: { stopReason: string; detail: string | null };
  } = $props();
</script>

<div class="border-b border-destructive/30 bg-destructive/10 px-4 py-2 text-xs text-destructive">
  <div class="mx-auto flex max-w-4xl items-start gap-2">
    <span class="mt-0.5 inline-flex h-1.5 w-1.5 shrink-0 rounded-full bg-destructive"></span>
    <div class="min-w-0 flex-1">
      <div class="font-semibold uppercase tracking-wider text-[0.65rem] opacity-80">
        Turn ended with error · {error.stopReason}
      </div>
      {#if error.detail}
        <div class="mt-0.5 font-mono text-[0.75rem] leading-relaxed break-words">
          {error.detail}
        </div>
        {#if error.detail.toLowerCase().includes('authentication')}
          <div class="mt-1 text-[0.7rem] opacity-80">
            Run <code class="rounded bg-destructive/20 px-1">claude /login</code> in a regular terminal to re-authenticate.
          </div>
        {/if}
      {/if}
    </div>
  </div>
</div>
