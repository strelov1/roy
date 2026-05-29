<script lang="ts">
  import { app } from '../state.svelte';
  import ConfirmDeleteDialog from './ConfirmDeleteDialog.svelte';
  import type { Project } from '../management-client';

  let {
    project,
    onclose,
  }: {
    project: Project;
    onclose: () => void;
  } = $props();

  let sessionCount = $derived(
    [...app.live, ...app.archived].filter((s) => s.project_id === project.id)
      .length,
  );
</script>

<ConfirmDeleteDialog
  title={`Delete "${project.name}"?`}
  body={`This permanently deletes the project and ${sessionCount} session${sessionCount === 1 ? '' : 's'} (journals and metadata). This cannot be undone.`}
  onConfirm={() => app.deleteProject(project.id)}
  {onclose}
/>
