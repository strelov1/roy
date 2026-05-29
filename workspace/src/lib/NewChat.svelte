<script lang="ts">
  import Composer from './Composer.svelte';

  let {
    onCreated,
    onOpenConnections,
  }: {
    onCreated: (sessionId: string) => void;
    onOpenConnections?: () => void;
  } = $props();
</script>

<div class="flex h-full flex-col bg-background">
  <div class="flex flex-1 flex-col items-center justify-center px-6 pb-[18vh]">
    <div class="w-full max-w-2xl">
      <Composer onCreated={onCreated} {onOpenConnections} />
    </div>
  </div>
</div>
