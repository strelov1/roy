// Background attach controller: keeps a frame subscription open on every
// live session that isn't the foreground one, so the sidebar's per-session
// "working" pulse stays accurate regardless of which session the user is
// looking at. The foreground session is owned by `openSession` directly —
// we explicitly skip it here to avoid double-counting frames.

import { royClient } from './client';
import type { JournalEntry } from './wire';

export type BgAttachHost = {
  /** Read the currently focused session id (or null) — bg attach skips it. */
  currentSession(): string | null;
  /** Toggle the per-session "working" flag the sidebar pulses on. */
  markActive(session: string, active: boolean): void;
};

export class BgAttach {
  /** Per-session unsubscribe handles. */
  private subs = new Map<string, () => void>();

  constructor(private host: BgAttachHost) {}

  /** Tear down a single session's bg attach + tell the daemon. No-op
   *  if no sub exists, so callers don't need a pre-check. */
  drop(session: string) {
    const unsub = this.subs.get(session);
    if (!unsub) return;
    unsub();
    this.subs.delete(session);
    // `call` (not `fire`): daemon answers Detach with a `detached` event,
    // so the response must consume a pending slot — otherwise it lands
    // at the head of the queue and trips the next `call`'s kind check.
    void royClient.call({ op: 'detach', session }, 'detached').catch(() => {});
  }

  /** Reconcile bg attaches against the current live session set. Drops
   *  attaches for vanished sessions; opens new attaches in parallel for
   *  the ones we haven't seen yet (skipping the foreground session). */
  async reconcile(liveIds: string[]): Promise<void> {
    const liveSet = new Set(liveIds);
    // Snapshot keys before iteration — `drop` mutates the map.
    for (const id of [...this.subs.keys()]) {
      if (!liveSet.has(id)) {
        this.drop(id);
        this.host.markActive(id, false);
      }
    }
    // Attach to fresh ones in parallel — sequential awaits made a 50-session
    // reconnect take 50 round-trips end-to-end. We skip the open session;
    // its foreground path pumps onFrame. No `from_seq`: the bg sub only
    // watches for new turn-start/turn-end frames.
    const current = this.host.currentSession();
    const fresh = liveIds.filter((id) => id !== current && !this.subs.has(id));
    await Promise.all(
      fresh.map(async (id) => {
        try {
          await royClient.call({ op: 'attach', session: id }, 'attached');
          const unsub = royClient.subscribeFrames(id, (entry) =>
            this.onFrame(id, entry),
          );
          this.subs.set(id, unsub);
        } catch {
          // Best-effort: a transient failure shouldn't break the sidebar.
        }
      }),
    );
  }

  /** Lightweight handler for background frames: just toggle the per-session
   *  turn flag. We don't fan the frame into entries because that buffer
   *  belongs to the currently-open session. */
  private onFrame(session: string, entry: JournalEntry) {
    const t = entry.event.type;
    if (t === 'user_prompt') this.host.markActive(session, true);
    else if (t === 'result') this.host.markActive(session, false);
  }
}
