<script lang="ts">
  import { onMount } from 'svelte';
  import { Users, CheckCircle2, XCircle, Loader2 } from '@lucide/svelte';
  import * as Card from '$lib/components/ui/card';
  import { Button } from '$lib/components/ui/button';
  import { authState } from './auth.svelte';
  import { teams as teamsApi, HttpError } from './management-client';
  import { errMsg } from './utils';

  let {
    token,
    onDone,
  }: {
    token: string;
    onDone: () => void;
  } = $props();

  type Phase =
    | { kind: 'pending' }
    | { kind: 'ok' }
    | { kind: 'error'; message: string };

  let phase = $state<Phase>({ kind: 'pending' });

  async function run() {
    if (!token) {
      phase = { kind: 'error', message: 'Missing invite token in URL.' };
      return;
    }
    try {
      await teamsApi.acceptInvite(token);
      // Refresh /auth/me so `user.teams` includes the new membership; the
      // Teams page reads off that slice and would otherwise look unchanged.
      await authState.refresh();
      phase = { kind: 'ok' };
    } catch (e) {
      if (e instanceof HttpError) {
        // The server collapses every accept failure to a generic 400
        // ("invite invalid") — same/expired token, already accepted, or
        // wrong account. Show that as-is rather than guess.
        phase = {
          kind: 'error',
          message: e.status === 400 ? 'Invite invalid — it may be expired or already used.' : e.message,
        };
      } else {
        phase = { kind: 'error', message: errMsg(e) };
      }
    }
  }

  onMount(() => {
    void run();
  });
</script>

<div class="flex h-full w-full items-center justify-center bg-background p-6">
  <Card.Root class="w-full max-w-md">
    <Card.Header>
      <Card.Title class="flex items-center gap-2">
        <Users class="size-5 text-muted-foreground" />
        Team invite
      </Card.Title>
      <Card.Description>
        {#if phase.kind === 'pending'}
          Accepting the invite as
          <code class="rounded bg-muted px-1.5 py-0.5 font-mono text-xs">{authState.user?.username ?? '?'}</code>…
        {:else if phase.kind === 'ok'}
          You're in. Refreshed your membership list.
        {:else}
          We couldn't accept this invite.
        {/if}
      </Card.Description>
    </Card.Header>
    <Card.Content class="flex flex-col gap-4">
      <div class="flex items-center gap-2 text-sm">
        {#if phase.kind === 'pending'}
          <Loader2 class="size-4 animate-spin text-muted-foreground" />
          <span class="text-muted-foreground">Talking to the server…</span>
        {:else if phase.kind === 'ok'}
          <CheckCircle2 class="size-4 text-emerald-500" />
          <span>Membership added.</span>
        {:else}
          <XCircle class="size-4 text-destructive" />
          <span class="text-destructive">{phase.message}</span>
        {/if}
      </div>
      <div class="flex justify-end">
        <Button onclick={onDone} disabled={phase.kind === 'pending'}>
          {phase.kind === 'ok' ? 'Continue' : 'Back'}
        </Button>
      </div>
    </Card.Content>
  </Card.Root>
</div>
