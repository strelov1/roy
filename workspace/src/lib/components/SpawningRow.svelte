<script lang="ts">
  // Ephemeral sidebar row shown while `app.spawningSession` is set —
  // between `op:spawn` going out and the daemon's `spawned` reply
  // (several seconds for a fresh agent boot). Disappears the instant
  // `createSession` clears the state; the real row arrives via the
  // subsequent `refreshSessions`. Parents gate visibility (project vs
  // orphan) via `app.spawningSession?.projectId` and only mount this
  // when it matches their scope.
  import { Loader2 } from '@lucide/svelte';
  import { app } from '../state.svelte';
  import { formatTitle } from '../utils';

  let label = $derived(
    formatTitle(app.spawningSession?.firstPrompt ?? '') || 'Spawning session…',
  );
</script>

<div
  aria-busy="true"
  class="flex items-center gap-2 rounded-md px-2 py-1.5 text-sm italic text-muted-foreground"
  title="Spawning session — this can take a few seconds"
>
  <Loader2 class="size-3.5 shrink-0 animate-spin text-foreground/80" />
  <span class="min-w-0 flex-1 truncate">{label}</span>
</div>
