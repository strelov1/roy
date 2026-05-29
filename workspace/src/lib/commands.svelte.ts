// Slash-command catalog: lazy-fetched from /management/commands, which
// scans ~/.claude/skills/**/SKILL.md plus enabled plugin marketplaces.
// The server already caches results for 30s, so we just load once on first
// use and refresh on demand.

import {
  commands as api,
  HttpError,
  type CommandInfo,
  type CreateCommandReq,
} from './management-client';
import { LoadableStore } from './list-store.svelte';

export type { CommandInfo, CommandBody, CreateCommandReq } from './management-client';

/// Fetch the markdown body for a single skill. `null` when the server can't
/// find it (404) or the request fails — callers fall back to leaving the
/// literal `/<name>` in the textarea so the user isn't stranded mid-edit.
export async function fetchCommandBody(name: string): Promise<string | null> {
  try {
    const data = await api.body(name);
    return data.body;
  } catch (e) {
    // 404 → skill not found; any other failure also falls back to null so the
    // caller leaves the literal `/<name>` in place.
    if (e instanceof HttpError && e.status === 404) return null;
    return null;
  }
}

/// Create a new skill under ~/.roy/skills/. Returns true on success, false
/// when the server rejects (4xx). Caller surfaces an error toast.
export async function createCommand(req: CreateCommandReq): Promise<boolean> {
  try {
    await api.create(req);
    return true;
  } catch {
    return false;
  }
}

class CommandsState extends LoadableStore<CommandInfo> {
  /// Fire-and-forget; safe to call repeatedly. Reads the cached server
  /// response after the first hit; pass `force=true` to bust both layers.
  async load(force = false): Promise<void> {
    await this.run(() => api.list(), force);
  }
}

export const commandsStore = new CommandsState();
