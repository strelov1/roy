// Client-side store for the user's inbound channels. Telegram is the only
// channel with a backend today; email / WhatsApp are roadmap. A "channel" is
// two backend objects: a connection (the channel's credentials) and a channel
// binding (agent + session strategy). addChannel orchestrates both and rolls
// back the connection if the bind fails so no orphan credential is left.

import {
  channelBindings as api,
  connections as connApi,
  type ChannelBinding,
  type Connection,
  type SessionStrategy,
} from './management-client';
import { LoadableStore } from './list-store.svelte';

/// Channel types the UI knows about. Only `telegram` has a backend today;
/// the union grows as roy-inbound learns new channels (email, whatsapp, …).
export type ChannelType = 'telegram';

export type NewChannelInput = {
  channelType: ChannelType;
  name: string;
  agentSlug: string;
  agentScope: string;
  sessionStrategy: SessionStrategy;
  idleTimeoutSecs?: number;
  /// Sender ids allowed to use the channel (Telegram user ids today). Empty =
  /// open to everyone.
  allowedSenderIds: number[];
  /// Telegram credentials — required when channelType === 'telegram'.
  telegram?: { botToken: string };
};

class ChannelsState extends LoadableStore<ChannelBinding> {
  async load(force = false): Promise<void> {
    await this.run(() => api.list(), force);
  }

  /// Create the channel's connection (credentials), then bind it to the agent.
  /// If the bind fails, delete the just-created connection so a failed attempt
  /// doesn't strand a credential in the DB.
  async addChannel(input: NewChannelInput): Promise<ChannelBinding> {
    const conn = await this.createConnection(input);
    try {
      const binding = await api.create({
        connection_id: conn.id,
        agent_slug: input.agentSlug,
        agent_scope: input.agentScope,
        session_strategy: input.sessionStrategy,
        idle_timeout_secs: input.idleTimeoutSecs,
        allowed_user_ids: input.allowedSenderIds,
      });
      this.list = [binding, ...this.list];
      return binding;
    } catch (e) {
      // Roll back the just-created connection so a failed bind leaves no
      // orphan credential, then surface the original bind error.
      await this.tryRemoveConnection(conn.id);
      throw e;
    }
  }

  /// Best-effort connection delete. Swallows errors: a leftover connection is
  /// harmless and re-deletable, and we never want cleanup failure to mask the
  /// real error (rollback) or block the list update (delete).
  private async tryRemoveConnection(id: string): Promise<void> {
    try {
      await connApi.remove(id);
    } catch {
      /* leave the orphan */
    }
  }

  /// Create the per-type connection that carries the channel's credentials.
  /// Each new channel kind adds a branch here — the rest of the flow is shared.
  private createConnection(input: NewChannelInput): Promise<Connection> {
    if (input.channelType === 'telegram') {
      return connApi.create({
        name: input.name,
        kind: 'telegram_bot',
        config: {},
        secrets: { bot_token: input.telegram?.botToken ?? '' },
      });
    }
    // Unreachable while ChannelType is telegram-only; the throw is the guard
    // that makes adding a future kind without a branch a loud failure.
    throw new Error(`channel type "${input.channelType}" is not supported yet`);
  }

  async setEnabled(id: string, enabled: boolean): Promise<void> {
    const updated = await api.setEnabled(id, enabled);
    this.list = this.list.map((b) => (b.id === id ? updated : b));
  }

  /// Delete the binding, then its connection (so the credential is gone too).
  async removeChannel(binding: ChannelBinding): Promise<void> {
    await api.remove(binding.id);
    await this.tryRemoveConnection(binding.connection_id);
    this.list = this.list.filter((b) => b.id !== binding.id);
  }
}

export const channelsStore = new ChannelsState();
export type { ChannelBinding } from './management-client';
