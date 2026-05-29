<script lang="ts">
  import { app } from '../state.svelte';
  import ConfirmDeleteDialog from './ConfirmDeleteDialog.svelte';

  let {
    session,
    onclose,
  }: {
    session: string;
    onclose: () => void;
  } = $props();

  let title = $derived(app.titleFor(session));
</script>

<ConfirmDeleteDialog
  title={`Delete "${title}"?`}
  body="This permanently deletes the archived session's journal and metadata. This cannot be undone."
  onConfirm={() => app.deleteArchive(session)}
  {onclose}
/>
