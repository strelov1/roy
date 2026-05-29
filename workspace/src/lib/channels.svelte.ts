// Client-side store for the user's Telegram bot channel bindings.
// A "bot" is two backend objects: a telegram_bot connection (token) and a
// channel binding (agent + session strategy). addBot orchestrates both and
// rolls back the connection if the bind fails so no orphan token is left.

import {
  channelBindings as api,
  connections as connApi,
  type ChannelBinding,
  type SessionStrategy,
} from './management-client';
import { LoadableStore } from './list-store.svelte';

export type NewBotInput = {
  botName: string;
  botToken: string;
  agentSlug: string;
  agentScope: string;
  sessionStrategy: SessionStrategy;
  idleTimeoutSecs?: number;
  allowedUserIds: number[];
};

class ChannelsState extends LoadableStore<ChannelBinding> {
  async load(force = false): Promise<void> {
    await this.run(() => api.list(), force);
  }

  /// Create the telegram_bot connection, then bind it to the agent. If the
  /// bind fails, delete the just-created connection so a failed attempt
  /// doesn't strand a bot token in the DB.
  async addBot(input: NewBotInput): Promise<ChannelBinding> {
    const conn = await connApi.create({
      name: input.botName,
      kind: 'telegram_bot',
      config: {},
      secrets: { bot_token: input.botToken },
    });
    try {
      const binding = await api.create({
        connection_id: conn.id,
        agent_slug: input.agentSlug,
        agent_scope: input.agentScope,
        session_strategy: input.sessionStrategy,
        idle_timeout_secs: input.idleTimeoutSecs,
        allowed_user_ids: input.allowedUserIds,
      });
      this.list = [binding, ...this.list];
      return binding;
    } catch (e) {
      // Best-effort rollback. Swallow its error so the user sees the real
      // bind failure, not a secondary cleanup error.
      try {
        await connApi.remove(conn.id);
      } catch {
        /* leave the orphan; surface the original error below */
      }
      throw e;
    }
  }

  async setEnabled(id: string, enabled: boolean): Promise<void> {
    const updated = await api.setEnabled(id, enabled);
    this.list = this.list.map((b) => (b.id === id ? updated : b));
  }

  /// Delete the binding, then its connection (so the bot token is gone too).
  async removeBot(binding: ChannelBinding): Promise<void> {
    await api.remove(binding.id);
    try {
      await connApi.remove(binding.connection_id);
    } catch {
      /* binding already gone; an orphan connection is harmless and re-deletable */
    }
    this.list = this.list.filter((b) => b.id !== binding.id);
  }
}

export const channelsStore = new ChannelsState();
export type { ChannelBinding } from './management-client';
