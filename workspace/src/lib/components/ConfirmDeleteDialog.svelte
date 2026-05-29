<script lang="ts">
  // Shared confirm-and-delete modal, built on the project's shadcn Dialog so
  // it gets the overlay, focus-trap, and Escape handling for free. The delete
  // dialogs (project / session / team) differ only in their copy and which
  // delete runs, so they delegate here. Mount-on-demand: the parent renders
  // this behind `{#if target}` and clears the target in `onclose`.
  import type { Snippet } from 'svelte';
  import * as Dialog from '$lib/components/ui/dialog';
  import { Button } from '$lib/components/ui/button';
  import { errMsg } from '../utils';

  let {
    title,
    body,
    confirmLabel = 'Delete',
    onConfirm,
    onclose,
  }: {
    title: string;
    /** Plain text, or a snippet when the body needs markup. */
    body: string | Snippet;
    confirmLabel?: string;
    /** Runs on confirm; rejecting keeps the dialog open and shows the error. */
    onConfirm: () => Promise<void>;
    onclose: () => void;
  } = $props();

  let submitting = $state(false);
  let error = $state<string | null>(null);

  // Mounted already-open; closing always unmounts via onclose (the parent
  // drops its `{#if target}`), so `open` stays a constant. Block close
  // (Escape / overlay / X) while the delete RPC is in flight.
  function onOpenChange(next: boolean) {
    if (!next && !submitting) onclose();
  }

  async function confirm() {
    submitting = true;
    error = null;
    try {
      await onConfirm();
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
      <Dialog.Title>{title}</Dialog.Title>
      <Dialog.Description>
        {#if typeof body === 'string'}{body}{:else}{@render body()}{/if}
      </Dialog.Description>
    </Dialog.Header>
    {#if error}
      <p class="text-sm text-destructive">{error}</p>
    {/if}
    <Dialog.Footer>
      <Button variant="ghost" onclick={onclose} disabled={submitting}>Cancel</Button>
      <Button variant="destructive" onclick={confirm} disabled={submitting}>
        {submitting ? 'Deleting…' : confirmLabel}
      </Button>
    </Dialog.Footer>
  </Dialog.Content>
</Dialog.Root>
