<script lang="ts">
  import { Button } from '$lib/components/ui/button';
  import { Input } from '$lib/components/ui/input';
  import { Label } from '$lib/components/ui/label';
  import { Textarea } from '$lib/components/ui/textarea';
  import * as Dialog from '$lib/components/ui/dialog';
  import { connectionsStore } from './connections.svelte';
  import { errMsg } from './utils';

  let {
    open = $bindable(false),
    onConnected,
  }: {
    open?: boolean;
    onConnected?: () => void;
  } = $props();

  let name = $state('');
  let command = $state('');
  let argsText = $state('');
  let envText = $state('');
  let secretsText = $state('');
  let submitting = $state(false);
  let error = $state<string | null>(null);

  $effect(() => {
    if (open) {
      name = '';
      command = '';
      argsText = '';
      envText = '';
      secretsText = '';
      error = null;
    }
  });

  function parseLines(text: string): string[] {
    return text
      .split('\n')
      .map((l) => l.trim())
      .filter((l) => l.length > 0);
  }

  function parseKv(text: string): Record<string, string> {
    const out: Record<string, string> = {};
    for (const line of parseLines(text)) {
      const eq = line.indexOf('=');
      if (eq < 0) continue;
      const key = line.slice(0, eq).trim();
      if (key) out[key] = line.slice(eq + 1);
    }
    return out;
  }

  async function submit() {
    if (submitting) return;
    if (!name.trim()) {
      error = 'Name is required';
      return;
    }
    if (!command.trim()) {
      error = 'Command is required';
      return;
    }
    submitting = true;
    error = null;
    try {
      const args = parseLines(argsText);
      const env = parseKv(envText);
      const secrets = parseKv(secretsText);
      await connectionsStore.create({
        name: name.trim(),
        kind: 'mcp_stdio',
        config: {
          command: command.trim(),
          ...(args.length > 0 ? { args } : {}),
          ...(Object.keys(env).length > 0 ? { env } : {}),
        },
        ...(Object.keys(secrets).length > 0 ? { secrets } : {}),
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
  <Dialog.Content class="max-w-lg">
    <Dialog.Header>
      <Dialog.Title>Add custom MCP server</Dialog.Title>
      <Dialog.Description>
        Configure any stdio MCP server by hand. For known providers, prefer the
        catalog entries (Connect button in the Available list).
      </Dialog.Description>
    </Dialog.Header>

    <div class="space-y-4 py-2">
      <div class="space-y-1.5">
        <Label for="custom-name">Name</Label>
        <Input
          id="custom-name"
          bind:value={name}
          placeholder="My MCP server"
          autocomplete="off"
        />
      </div>

      <div class="space-y-1.5">
        <Label for="custom-command">Command</Label>
        <Input
          id="custom-command"
          bind:value={command}
          placeholder="npx"
          autocomplete="off"
        />
      </div>

      <div class="space-y-1.5">
        <Label for="custom-args">Args (one per line)</Label>
        <Textarea
          id="custom-args"
          bind:value={argsText}
          placeholder={'-y\n@some-org/my-mcp-server'}
          rows={3}
        />
      </div>

      <div class="space-y-1.5">
        <Label for="custom-env">Env (KEY=value per line, optional)</Label>
        <Textarea
          id="custom-env"
          bind:value={envText}
          placeholder="LOG_LEVEL=debug"
          rows={2}
        />
      </div>

      <div class="space-y-1.5">
        <Label for="custom-secrets">Secrets (KEY=value per line, optional)</Label>
        <Textarea
          id="custom-secrets"
          bind:value={secretsText}
          placeholder="API_TOKEN=..."
          rows={2}
        />
        <p class="text-xs text-muted-foreground">
          Stored in the DB as plain JSON (file mode 0600). For one-off ad-hoc
          servers; for shareable providers prefer adding an entry to
          <code class="rounded bg-muted px-1 font-mono">~/.roy/connections.yaml</code>.
        </p>
      </div>

      {#if error}
        <p class="text-sm text-destructive">{error}</p>
      {/if}
    </div>

    <Dialog.Footer>
      <Button variant="ghost" onclick={() => (open = false)}>Cancel</Button>
      <Button onclick={submit} disabled={submitting}>
        {submitting ? 'Adding…' : 'Add'}
      </Button>
    </Dialog.Footer>
  </Dialog.Content>
</Dialog.Root>
