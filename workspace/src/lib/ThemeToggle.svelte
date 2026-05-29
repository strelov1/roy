<script lang="ts">
  import { themeStore } from './theme.svelte';
  import { Sun, Moon } from '@lucide/svelte';

  // Show the icon of the *effective* theme. `system` resolves via
  // `prefers-color-scheme` so the user always sees a concrete state.
  const isDarkEffective = $derived(themeStore.isDark);
</script>

<button
  type="button"
  onclick={() => themeStore.cycle()}
  title={isDarkEffective ? 'Dark · click for light' : 'Light · click for dark'}
  aria-label="Toggle theme"
  class="inline-flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground"
>
  {#if isDarkEffective}
    <Moon class="size-4" />
  {:else}
    <Sun class="size-4" />
  {/if}
</button>
