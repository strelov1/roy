<script lang="ts">
  // New-project form, built on the project's shadcn Dialog (overlay +
  // focus-trap + Escape come for free). Mount-on-demand: the Composer renders
  // it behind `{#if ...}` and clears that flag in `onclose`.
  import { app } from '../state.svelte';
  import { authState } from '../auth.svelte';
  import { errMsg } from '../utils';
  import * as Dialog from '$lib/components/ui/dialog';
  import { Button } from '$lib/components/ui/button';
  import * as Select from '$lib/components/ui/select';

  let {
    onclose,
    defaultTeamId = '',
  }: {
    onclose: () => void;
    /** Pre-select a team for the new project. Empty string = personal. The
     *  Composer passes its current scope so "+ New project" lands in the
     *  same team the user is about to spawn a session under. */
    defaultTeamId?: string;
  } = $props();

  let name = $state('');
  // Captures the initial prop. The dialog is re-mounted on each open, so
  // a later prop change can't desync — silences the static warning.
  // svelte-ignore state_referenced_locally
  let teamId = $state<string>(defaultTeamId);
  let submitting = $state(false);
  let error = $state<string | null>(null);

  const userTeams = $derived(authState.user?.teams ?? []);
  const teamLabel = $derived(
    teamId ? (userTeams.find((t) => t.id === teamId)?.name ?? 'Team') : 'Personal',
  );

  // Mounted already-open; closing always unmounts via onclose (the Composer
  // drops its `{#if}`). Block close while the create RPC is in flight.
  function onOpenChange(next: boolean) {
    if (!next && !submitting) onclose();
  }

  async function submit() {
    const trimmed = name.trim();
    if (!trimmed) return;
    submitting = true;
    error = null;
    try {
      await app.createProject(trimmed, teamId || undefined);
      onclose();
    } catch (e) {
      error = errMsg(e);
      submitting = false;
    }
  }
</script>

<Dialog.Root open={true} {onOpenChange}>
  <Dialog.Content class="max-w-md">
    <Dialog.Header>
      <Dialog.Title>New project</Dialog.Title>
    </Dialog.Header>
    <div class="flex flex-col gap-3">
      <label class="flex flex-col gap-1 text-sm">
        <span class="text-muted-foreground">Name</span>
        <input
          type="text"
          bind:value={name}
          placeholder="letters, digits, dash, underscore"
          required
          autofocus
          class="rounded-md border border-input bg-background px-3 py-2 text-sm outline-none focus:ring-2 focus:ring-ring/40"
        />
      </label>

      {#if userTeams.length > 0}
        <label class="flex flex-col gap-1 text-sm">
          <span class="text-muted-foreground">Owner</span>
          <Select.Root
            type="single"
            value={teamId}
            onValueChange={(v) => (teamId = v ?? '')}
            disabled={submitting}
          >
            <Select.Trigger class="h-9 rounded-md border-border px-3 text-sm font-medium">
              {teamLabel}
            </Select.Trigger>
            <Select.Content>
              <Select.Item value="">Personal</Select.Item>
              {#each userTeams as t (t.id)}
                <Select.Item value={t.id}>Team · {t.name}</Select.Item>
              {/each}
            </Select.Content>
          </Select.Root>
        </label>
      {/if}

      {#if error}
        <p class="text-sm text-destructive">{error}</p>
      {/if}
    </div>
    <Dialog.Footer>
      <Button variant="ghost" onclick={onclose} disabled={submitting}>Cancel</Button>
      <Button onclick={submit} disabled={submitting || !name.trim()}>Create</Button>
    </Dialog.Footer>
  </Dialog.Content>
</Dialog.Root>
