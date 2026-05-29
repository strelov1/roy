<script lang="ts">
  // Three-dots dropdown that lives in a session row's hover-actions area.
  // Reused by SessionList (orphan rows) and ProjectGroup (project-scoped
  // rows) so the menu is shaped identically wherever a session surfaces.
  //
  // The Rename action just calls `onRename`; the parent owns the
  // `editingId` state and flips the row's `editing` bindable on. Pin and
  // Archive go through `app.setPinned` / `app.archiveSession`, both of
  // which use the optimistic-RMW + rollback pattern.

  import { app } from '../state.svelte';
  import * as DropdownMenu from '$lib/components/ui/dropdown-menu';
  import {
    Archive,
    MoreHorizontal,
    Pencil,
    Pin,
    PinOff,
  } from '@lucide/svelte';

  let {
    session,
    pinned,
    archived = false,
    onRename,
  }: {
    session: string;
    pinned: boolean;
    /** True iff the session is already archived. Disables the Archive item —
     *  closing an archived session would have the daemon reject with no
     *  visible feedback (toast yes, menu stays open). The row's Resume /
     *  Delete actions live on `ArchivedSessionRow` instead. */
    archived?: boolean;
    /** Called when the user picks "Rename". Parent should flip its
     *  per-session editing flag so the row swaps in <InlineEditable>. */
    onRename: () => void;
  } = $props();

  let pinning = $derived(app.pinningSessions[session] === true);
  let closing = $derived(app.closingSessions[session] === true);

  function togglePin() {
    void app.setPinned(session, !pinned);
  }

  function archive() {
    void app.archiveSession(session);
  }
</script>

<DropdownMenu.Root>
  <DropdownMenu.Trigger
    aria-label="Session actions"
    title="Session actions"
    class="flex size-6 shrink-0 items-center justify-center rounded text-muted-foreground opacity-0 transition-opacity hover:bg-foreground/10 hover:text-foreground group-hover/row:opacity-100 focus-visible:opacity-100 data-[state=open]:opacity-100"
  >
    <MoreHorizontal class="size-3.5" />
  </DropdownMenu.Trigger>
  <DropdownMenu.Content align="end" class="min-w-[10rem]">
    <DropdownMenu.Item onSelect={onRename}>
      <Pencil class="size-4" />
      Rename
    </DropdownMenu.Item>
    <DropdownMenu.Item onSelect={togglePin} disabled={pinning}>
      {#if pinned}
        <PinOff class="size-4" />
        Unpin
      {:else}
        <Pin class="size-4" />
        Pin
      {/if}
    </DropdownMenu.Item>
    <DropdownMenu.Separator />
    <DropdownMenu.Item onSelect={archive} disabled={closing || archived}>
      <Archive class="size-4" />
      Archive
    </DropdownMenu.Item>
  </DropdownMenu.Content>
</DropdownMenu.Root>
