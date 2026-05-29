<script lang="ts">
  import { Loader2, Paperclip, Plug, Plus, Zap } from '@lucide/svelte';
  import * as DropdownMenu from '$lib/components/ui/dropdown-menu';
  import { HttpError, MAX_UPLOAD_MB, uploads } from './management-client';

  let {
    disabled = false,
    slashSupported = true,
    mcpEligible = false,
    mcpCount = 0,
    onAttach,
    onPickSkill,
    onPickConnections,
    onError,
  }: {
    disabled?: boolean;
    /** Disables the "Use skill" entry when the active agent doesn't honor slash commands. */
    slashSupported?: boolean;
    /** When false, hides the "MCP connections" entry (preset doesn't support MCP injection). */
    mcpEligible?: boolean;
    /** Shown as a count next to the menu entry; 0 means no selection yet. */
    mcpCount?: number;
    /** Called after a successful upload — composer splices `@${path}` into the draft. */
    onAttach: (path: string, name: string) => void;
    /** Opens the slash-command popover at the current caret position. */
    onPickSkill: () => void;
    /** Opens the MCP connection picker dialog (composer owns the actual dialog). */
    onPickConnections?: () => void;
    /** Surface upload failures through the composer's existing error rail. */
    onError?: (message: string) => void;
  } = $props();

  let fileInput: HTMLInputElement | undefined = $state();
  let uploading = $state(false);

  function openFilePicker() {
    fileInput?.click();
  }

  async function onFileChange(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    // Always reset so picking the same file twice re-fires `change`.
    input.value = '';
    if (!file) return;

    uploading = true;
    try {
      const res = await uploads.send(file);
      onAttach(res.path, res.name);
    } catch (err) {
      if (err instanceof HttpError && err.status === 413) {
        onError?.(`File too large (max ${MAX_UPLOAD_MB} MB)`);
      } else {
        onError?.(err instanceof Error ? err.message : 'Upload failed');
      }
    } finally {
      uploading = false;
    }
  }
</script>

<input
  bind:this={fileInput}
  type="file"
  class="hidden"
  onchange={onFileChange}
/>

<DropdownMenu.Root>
  <DropdownMenu.Trigger
    disabled={disabled || uploading}
    class="flex h-8 w-8 items-center justify-center rounded-full border border-border bg-background text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-50"
    aria-label="Attach or insert skill"
    title="Attach file or insert a skill"
  >
    {#if uploading}
      <Loader2 class="size-3.5 animate-spin" />
    {:else}
      <Plus class="size-4" />
    {/if}
  </DropdownMenu.Trigger>
  <DropdownMenu.Content class="w-48" align="start">
    <DropdownMenu.Item onSelect={openFilePicker}>
      <Paperclip class="size-4" />
      <span>Attach file</span>
    </DropdownMenu.Item>
    <DropdownMenu.Item
      onSelect={onPickSkill}
      disabled={!slashSupported}
    >
      <Zap class="size-4" />
      <span>Use skill</span>
    </DropdownMenu.Item>
    {#if mcpEligible && onPickConnections}
      <DropdownMenu.Item onSelect={onPickConnections}>
        <Plug class="size-4" />
        <span>Connections</span>
        {#if mcpCount > 0}
          <span class="ml-auto text-[10px] text-muted-foreground">{mcpCount}</span>
        {/if}
      </DropdownMenu.Item>
    {/if}
  </DropdownMenu.Content>
</DropdownMenu.Root>
