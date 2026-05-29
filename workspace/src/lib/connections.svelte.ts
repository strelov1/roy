// Client-side store for user-owned MCP connections.
// Talks to /management/connections via the typed wrapper in management-client.

import {
  connections as api,
  type Connection,
  type NewConnection,
  type ConnectionUpdate,
} from './management-client';
import { LoadableStore } from './list-store.svelte';

class ConnectionsState extends LoadableStore<Connection> {
  async load(force = false): Promise<void> {
    await this.run(() => api.list(), force);
  }

  async create(body: NewConnection): Promise<Connection> {
    const c = await api.create(body);
    this.list = [c, ...this.list];
    return c;
  }

  async update(id: string, body: ConnectionUpdate): Promise<Connection> {
    const c = await api.update(id, body);
    this.list = this.list.map((x) => (x.id === id ? c : x));
    return c;
  }

  async remove(id: string): Promise<void> {
    await api.remove(id);
    this.list = this.list.filter((x) => x.id !== id);
  }
}

export const connectionsStore = new ConnectionsState();
export type { Connection, NewConnection, ConnectionUpdate } from './management-client';
