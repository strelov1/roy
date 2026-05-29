<script lang="ts">
  import { app } from '../state.svelte';
  import { Button } from '$lib/components/ui/button';
  import { Input } from '$lib/components/ui/input';
  import * as Dialog from '$lib/components/ui/dialog';
  import ConfirmDeleteDialog from './ConfirmDeleteDialog.svelte';
  import { UserPlus, Users, Mail, Trash2, Copy, Check } from '@lucide/svelte';
  import { authState, type TeamMembership } from '../auth.svelte';
  import {
    teams as teamsApi,
    HttpError,
  } from '../management-client';
  import { errMsg } from '../utils';

  // Teams tab: mirrors the projects inline-create + list pattern. Membership
  // is owned by authState.user.teams; we patch it locally on CRUD success.
  const teamRows = $derived<TeamMembership[]>(authState.user?.teams ?? []);

  let newTeamName = $state<string | null>(null);
  let newTeamSubmitting = $state(false);
  let newTeamInputEl: HTMLInputElement | undefined = $state();

  $effect(() => {
    if (newTeamName !== null) {
      queueMicrotask(() => newTeamInputEl?.focus());
    }
  });

  async function submitNewTeam() {
    const trimmed = (newTeamName ?? '').trim();
    if (!trimmed) {
      newTeamName = null;
      return;
    }
    newTeamSubmitting = true;
    try {
      const team = await teamsApi.create(trimmed);
      authState.patchTeams([
        ...teamRows,
        { id: team.id, name: team.name, role: 'owner' },
      ]);
      newTeamName = null;
    } catch (e) {
      app.lastError = errMsg(e);
    } finally {
      newTeamSubmitting = false;
    }
  }

  function onNewTeamKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      e.preventDefault();
      void submitNewTeam();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      newTeamName = null;
    }
  }

  let deleteTeamTarget = $state<TeamMembership | null>(null);

  // Delegates the confirm UX to ConfirmDeleteDialog; it tracks submitting +
  // surfaces a thrown message, so we only do the RPC + local patch here and
  // rethrow the 403 with team-specific copy.
  async function deleteTeam(target: TeamMembership) {
    try {
      await teamsApi.remove(target.id);
    } catch (e) {
      throw new Error(
        e instanceof HttpError && e.status === 403
          ? 'Only the owner can delete this team.'
          : errMsg(e),
      );
    }
    authState.patchTeams(teamRows.filter((t) => t.id !== target.id));
  }

  let inviteTarget = $state<TeamMembership | null>(null);
  let inviteToken = $state<string | null>(null);
  let inviteError = $state<string | null>(null);
  let inviteSubmitting = $state(false);
  let inviteCopied = $state(false);

  function inviteUrl(token: string): string {
    return `${window.location.origin}/accept-invite?token=${encodeURIComponent(token)}`;
  }

  async function generateInvite() {
    if (!inviteTarget) return;
    inviteSubmitting = true;
    inviteError = null;
    inviteToken = null;
    try {
      const res = await teamsApi.createInvite(inviteTarget.id);
      inviteToken = res.token;
    } catch (e) {
      inviteError = errMsg(e);
    } finally {
      inviteSubmitting = false;
    }
  }

  async function copyInvite() {
    if (!inviteToken) return;
    try {
      await navigator.clipboard.writeText(inviteUrl(inviteToken));
      inviteCopied = true;
      setTimeout(() => (inviteCopied = false), 1500);
    } catch (e) {
      inviteError = errMsg(e);
    }
  }

  function closeInvite() {
    inviteTarget = null;
    inviteToken = null;
    inviteError = null;
    inviteCopied = false;
  }
</script>

<div class="flex flex-col gap-0.5">
  {#if newTeamName === null}
    <button
      type="button"
      onclick={() => (newTeamName = '')}
      class="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-sm text-foreground transition-colors hover:bg-sidebar-accent/50"
    >
      <UserPlus class="size-4 text-muted-foreground" />
      <span>New team</span>
    </button>
  {:else}
    <div class="flex w-full items-center gap-2 rounded-md px-2 py-1 text-sm">
      <UserPlus class="size-4 shrink-0 text-muted-foreground" />
      <input
        bind:this={newTeamInputEl}
        bind:value={newTeamName}
        onkeydown={onNewTeamKeydown}
        onblur={() => { if (!(newTeamName ?? '').trim()) newTeamName = null; }}
        disabled={newTeamSubmitting}
        placeholder="Team name — Enter to create"
        class="min-w-0 flex-1 bg-transparent text-sm text-foreground placeholder:text-muted-foreground/60 focus:outline-none disabled:opacity-50"
      />
    </div>
  {/if}

  {#if teamRows.length === 0}
    <p class="px-2 py-1 text-xs text-muted-foreground/70">No teams yet</p>
  {:else}
    <ul class="flex flex-col">
      {#each teamRows as t (t.id)}
        <li class="group flex items-center gap-2 rounded-md px-2 py-1.5 text-sm hover:bg-sidebar-accent/40">
          <Users class="size-3.5 shrink-0 text-muted-foreground" />
          <span class="min-w-0 flex-1 truncate text-foreground">{t.name}</span>
          <span
            class={[
              'shrink-0 rounded-full px-1.5 py-0.5 text-[9px] uppercase tracking-wider',
              t.role === 'owner'
                ? 'bg-primary/15 text-primary'
                : 'bg-muted text-muted-foreground',
            ]}
          >
            {t.role}
          </span>
          {#if t.role === 'owner'}
            <button
              type="button"
              class="opacity-0 transition-opacity group-hover:opacity-100 hover:text-foreground"
              title="Invite a member"
              aria-label="Invite a member"
              onclick={() => {
                inviteTarget = t;
                inviteToken = null;
                inviteError = null;
                inviteCopied = false;
              }}
            >
              <Mail class="size-3.5 text-muted-foreground" />
            </button>
            <button
              type="button"
              class="opacity-0 transition-opacity group-hover:opacity-100 hover:text-destructive"
              title="Delete team"
              aria-label="Delete team"
              onclick={() => (deleteTeamTarget = t)}
            >
              <Trash2 class="size-3.5 text-muted-foreground" />
            </button>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</div>

{#snippet teamDeleteBody()}
  Members lose access immediately. The team workspace on disk
  (<code class="rounded bg-muted px-1 font-mono text-xs">teams/{deleteTeamTarget?.id}/</code>)
  is preserved — only the database row is removed.
{/snippet}

{#if deleteTeamTarget}
  <ConfirmDeleteDialog
    title={`Delete team "${deleteTeamTarget.name}"?`}
    body={teamDeleteBody}
    onConfirm={() => deleteTeam(deleteTeamTarget!)}
    onclose={() => (deleteTeamTarget = null)}
  />
{/if}

<Dialog.Root open={inviteTarget !== null} onOpenChange={(o) => { if (!o) closeInvite(); }}>
  <Dialog.Content class="max-w-lg">
    {#if inviteTarget}
      <Dialog.Header>
        <Dialog.Title>Invite to “{inviteTarget.name}”</Dialog.Title>
        <Dialog.Description>
          One-time invite token. Anyone with the link, while logged into roy,
          becomes a member after opening it.
        </Dialog.Description>
      </Dialog.Header>

      {#if !inviteToken}
        {#if inviteError}
          <p class="text-sm text-destructive">{inviteError}</p>
        {/if}
        <div class="flex justify-end gap-2 pt-2">
          <Button variant="ghost" onclick={closeInvite} disabled={inviteSubmitting}>
            Cancel
          </Button>
          <Button onclick={() => void generateInvite()} disabled={inviteSubmitting}>
            {inviteSubmitting ? 'Generating…' : 'Generate invite link'}
          </Button>
        </div>
      {:else}
        <div class="flex flex-col gap-3 pt-2">
          <div class="flex items-center gap-2">
            <Input
              readonly
              value={inviteUrl(inviteToken)}
              class="font-mono text-xs"
              onclick={(e) => (e.currentTarget as HTMLInputElement).select()}
            />
            <Button
              variant="outline"
              size="icon"
              title={inviteCopied ? 'Copied' : 'Copy link'}
              aria-label="Copy invite link"
              onclick={() => void copyInvite()}
            >
              {#if inviteCopied}
                <Check class="size-4 text-emerald-500" />
              {:else}
                <Copy class="size-4" />
              {/if}
            </Button>
          </div>
          <p class="text-xs text-muted-foreground">
            Single-use. The recipient must be signed in; if not, the link routes
            them through the login screen first.
          </p>
          {#if inviteError}
            <p class="text-sm text-destructive">{inviteError}</p>
          {/if}
          <div class="flex justify-end">
            <Button variant="ghost" onclick={closeInvite}>Done</Button>
          </div>
        </div>
      {/if}
    {/if}
  </Dialog.Content>
</Dialog.Root>
