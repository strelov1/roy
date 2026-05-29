<script lang="ts">
  import { Button } from '$lib/components/ui/button';
  import { Input } from '$lib/components/ui/input';
  import { Label } from '$lib/components/ui/label';
  import * as Dialog from '$lib/components/ui/dialog';
  import { connectionsStore } from './connections.svelte';
  import type { Provider } from './providers.svelte';
  import { errMsg } from './utils';

  let {
    provider,
    open = $bindable(false),
    onConnected,
  }: {
    provider: Provider;
    open?: boolean;
    onConnected?: () => void;
  } = $props();

  let label = $state('default');
  let secretValues = $state<Record<string, string>>({});
  let submitting = $state(false);
  let error = $state<string | null>(null);

  // Reset when dialog opens (fresh form per open).
  $effect(() => {
    if (open) {
      label = 'default';
      secretValues = Object.fromEntries(provider.secrets.map((s) => [s.key, '']));
      error = null;
    }
  });

  async function submit() {
    if (submitting) return;
    if (!label.trim()) {
      error = 'Label is required';
      return;
    }
    for (const s of provider.secrets) {
      if (!secretValues[s.key]?.trim()) {
        error = `${s.label} is required`;
        return;
      }
    }
    submitting = true;
    error = null;
    try {
      await connectionsStore.create({
        provider_id: provider.id,
        name: label.trim(),
        secrets: secretValues,
      });
      open = false;
      onConnected?.();
    } catch (e) {
      error = errMsg(e);
    } finally {
      submitting = false;
    }
  }
</script>

<Dialog.Root bind:open>
  <Dialog.Content class="max-w-md">
    <Dialog.Header>
      <Dialog.Title>Connect {provider.name}</Dialog.Title>
      {#if provider.description}
        <Dialog.Description>{provider.description}</Dialog.Description>
      {/if}
    </Dialog.Header>

    <div class="space-y-4 py-2">
      <div class="space-y-1.5">
        <Label for="conn-label">Label</Label>
        <Input
          id="conn-label"
          bind:value={label}
          placeholder="work, personal, …"
          autocomplete="off"
        />
        <p class="text-xs text-muted-foreground">
          Distinguishes this instance from other {provider.name} connections.
        </p>
      </div>

      {#each provider.secrets as secret (secret.key)}
        <div class="space-y-1.5">
          <Label for={`secret-${secret.key}`}>{secret.label}</Label>
          <Input
            id={`secret-${secret.key}`}
            type="password"
            bind:value={secretValues[secret.key]}
            autocomplete="off"
          />
          {#if secret.help}
            <p class="text-xs text-muted-foreground">{secret.help}</p>
          {/if}
        </div>
      {/each}

      {#if error}
        <p class="text-sm text-destructive">{error}</p>
      {/if}
    </div>

    <Dialog.Footer>
      <Button variant="ghost" onclick={() => (open = false)}>Cancel</Button>
      <Button onclick={submit} disabled={submitting}>
        {submitting ? 'Connecting…' : 'Connect'}
      </Button>
    </Dialog.Footer>
  </Dialog.Content>
</Dialog.Root>
