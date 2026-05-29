<script lang="ts">
  import { Button } from '$lib/components/ui/button';
  import { Input } from '$lib/components/ui/input';
  import { Label } from '$lib/components/ui/label';
  import * as Dialog from '$lib/components/ui/dialog';
  import * as Select from '$lib/components/ui/select';
  import { channelsStore, type ChannelType } from './channels.svelte';
  import { agents as agentsApi, type WireAgent, type SessionStrategy } from './management-client';
  import { errMsg } from './utils';

  let {
    open = $bindable(false),
    onAdded,
  }: {
    open?: boolean;
    onAdded?: () => void;
  } = $props();

  // Channel types the picker offers. Only Telegram has a backend today; new
  // kinds become a row here plus a credential block below — nothing else moves.
  const CHANNEL_TYPES: { value: ChannelType; label: string }[] = [
    { value: 'telegram', label: 'Telegram' },
  ];

  let channelType = $state<ChannelType>('telegram');
  let name = $state('');
  let botToken = $state('');
  // Encoded "agentScope::slug" so the value carries both the slug and the
  // scope the binding needs (two agents could share a slug across scopes).
  let agentValue = $state('');
  let strategy = $state<SessionStrategy>('per_sender_sticky');
  let idleMinutes = $state(60);
  let allowlistRaw = $state('');
  let agentList = $state<WireAgent[]>([]);
  let submitting = $state(false);
  let error = $state<string | null>(null);

  function scopeString(a: WireAgent): string {
    return a.scope.kind === 'team' && a.scope.team_id ? `team:${a.scope.team_id}` : 'user';
  }
  function encode(a: WireAgent): string {
    return `${scopeString(a)}::${a.slug}`;
  }

  const STRATEGIES: { value: SessionStrategy; label: string }[] = [
    { value: 'per_sender_sticky', label: 'Per sender (sticky)' },
    { value: 'persistent_one', label: 'One shared session' },
    { value: 'ephemeral', label: 'Ephemeral (fresh each message)' },
  ];

  const channelTypeLabel = $derived(
    CHANNEL_TYPES.find((c) => c.value === channelType)?.label ?? '',
  );
  const selectedAgentLabel = $derived(
    agentList.find((a) => encode(a) === agentValue)?.name ?? 'Select an agent',
  );
  const strategyLabel = $derived(
    STRATEGIES.find((s) => s.value === strategy)?.label ?? '',
  );

  // Fresh form + agent fetch on each open.
  $effect(() => {
    if (open) {
      channelType = 'telegram';
      name = '';
      botToken = '';
      agentValue = '';
      strategy = 'per_sender_sticky';
      idleMinutes = 60;
      allowlistRaw = '';
      error = null;
      void agentsApi.list().then((a) => (agentList = a)).catch((e) => (error = errMsg(e)));
    }
  });

  /// Parse "111, 222 333" → [111, 222, 333]. Returns null on any non-numeric
  /// token so we can reject the form instead of silently dropping it.
  function parseAllowlist(raw: string): number[] | null {
    const out: number[] = [];
    for (const tok of raw.split(/[\s,]+/).filter(Boolean)) {
      const n = Number(tok);
      if (!Number.isInteger(n) || n <= 0) return null;
      out.push(n);
    }
    return out;
  }

  async function submit() {
    if (submitting) return;
    if (!name.trim()) return (error = 'Name is required');
    if (channelType === 'telegram' && !botToken.trim()) return (error = 'Bot token is required');
    if (!agentValue) return (error = 'Pick an agent');
    if (strategy === 'per_sender_sticky' && (!idleMinutes || idleMinutes <= 0)) {
      return (error = 'Idle timeout must be a positive number of minutes');
    }
    const allowed = parseAllowlist(allowlistRaw);
    if (allowed === null) return (error = 'Allowlist must be space/comma-separated numeric sender IDs');

    const sep = agentValue.indexOf('::');
    const agentScope = agentValue.slice(0, sep);
    const agentSlug = agentValue.slice(sep + 2);

    submitting = true;
    error = null;
    try {
      await channelsStore.addChannel({
        channelType,
        name: name.trim(),
        agentSlug,
        agentScope,
        sessionStrategy: strategy,
        idleTimeoutSecs: strategy === 'per_sender_sticky' ? idleMinutes * 60 : undefined,
        allowedSenderIds: allowed,
        telegram: channelType === 'telegram' ? { botToken: botToken.trim() } : undefined,
      });
      open = false;
      onAdded?.();
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
      <Dialog.Title>Add channel</Dialog.Title>
      <Dialog.Description>
        Connect a channel to an agent. The agent answers messages that arrive on it.
      </Dialog.Description>
    </Dialog.Header>

    <div class="space-y-4 py-2">
      <div class="space-y-1.5">
        <Label>Channel type</Label>
        <Select.Root type="single" bind:value={channelType}>
          <Select.Trigger class="w-full">{channelTypeLabel}</Select.Trigger>
          <Select.Content>
            {#each CHANNEL_TYPES as c (c.value)}
              <Select.Item value={c.value}>{c.label}</Select.Item>
            {/each}
          </Select.Content>
        </Select.Root>
      </div>

      <div class="space-y-1.5">
        <Label for="channel-name">Name</Label>
        <Input id="channel-name" bind:value={name} placeholder="Support bot" autocomplete="off" />
        <p class="text-xs text-muted-foreground">A label to recognise this channel.</p>
      </div>

      {#if channelType === 'telegram'}
        <div class="space-y-1.5">
          <Label for="bot-token">Bot token</Label>
          <Input
            id="bot-token"
            type="password"
            bind:value={botToken}
            placeholder="123456:ABC-DEF…"
            autocomplete="off"
          />
          <p class="text-xs text-muted-foreground">From @BotFather. Stored as a secret.</p>
        </div>
      {/if}

      <div class="space-y-1.5">
        <Label>Agent</Label>
        <Select.Root type="single" bind:value={agentValue}>
          <Select.Trigger class="w-full">{selectedAgentLabel}</Select.Trigger>
          <Select.Content>
            {#each agentList as a (encode(a))}
              <Select.Item value={encode(a)}>{a.name} ({a.harness})</Select.Item>
            {/each}
          </Select.Content>
        </Select.Root>
      </div>

      <div class="space-y-1.5">
        <Label>Session strategy</Label>
        <Select.Root type="single" bind:value={strategy}>
          <Select.Trigger class="w-full">{strategyLabel}</Select.Trigger>
          <Select.Content>
            {#each STRATEGIES as s (s.value)}
              <Select.Item value={s.value}>{s.label}</Select.Item>
            {/each}
          </Select.Content>
        </Select.Root>
      </div>

      {#if strategy === 'per_sender_sticky'}
        <div class="space-y-1.5">
          <Label for="idle">Idle timeout (minutes)</Label>
          <Input id="idle" type="number" min="1" bind:value={idleMinutes} />
          <p class="text-xs text-muted-foreground">
            A sender's session closes after this much inactivity.
          </p>
        </div>
      {/if}

      <div class="space-y-1.5">
        <Label for="allowlist">Allowlist (optional)</Label>
        <Input
          id="allowlist"
          bind:value={allowlistRaw}
          placeholder="e.g. 12345678 98765432"
          autocomplete="off"
        />
        <p class="text-xs text-muted-foreground">
          Sender IDs allowed to use the channel. Empty = open to everyone.
        </p>
      </div>

      {#if error}
        <p class="text-sm text-destructive">{error}</p>
      {/if}
    </div>

    <Dialog.Footer>
      <Button variant="ghost" onclick={() => (open = false)}>Cancel</Button>
      <Button onclick={submit} disabled={submitting}>
        {submitting ? 'Adding…' : 'Add channel'}
      </Button>
    </Dialog.Footer>
  </Dialog.Content>
</Dialog.Root>
