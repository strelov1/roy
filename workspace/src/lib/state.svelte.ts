// Reactive Svelte 5 state for the chat UI. Single global `app` instance —
// fine for a single-window dev tool; revisit if we ever need multi-pane.

import type { ConnectionStatus } from './client';
import { royClient } from './client';
import type { Harness, JournalEntry, SessionInfo } from './wire';
import {
  projects as mgmtProjects,
  sessions as mgmtSessions,
  type Project,
} from './management-client';
import { harnessesConfig } from './harnesses-config.svelte';
import { errMsg, formatTitle, LS, lsGetJSON, lsSetJSON } from './utils';
import { BgAttach } from './bg-attach';
import { Transcript, type Group } from './transcript.svelte';

// MessageGroups.svelte imports `Group` from here — keep that path working.
export type { Group };

class AppState {
  status = $state<ConnectionStatus>('idle');
  live = $state<SessionInfo[]>([]);
  archived = $state<SessionInfo[]>([]);
  currentSession = $state<string | null>(null);
  currentAgent = $state<Harness | null>(null);
  currentModel = $state<string | null>(null);
  /// True while `openSession` is fetching the journal snapshot + attaching.
  /// Drives the skeleton placeholder in ChatView so the empty interval
  /// doesn't show as "no entries" before we know there really are none.
  loadingSession = $state(false);
  /// Session id currently being resurrected by `resumeAndOpen`. The
  /// `resume` RPC can take several seconds (daemon respawns the agent
  /// process), and the row-level + chat-level CTAs key off this so the
  /// click has visible feedback before `loadingSession` takes over.
  resumingSession = $state<string | null>(null);
  /// In-flight `spawn` RPC — set just before the call goes out, cleared
  /// the moment the daemon returns a session id (or errors). The sidebar
  /// renders a ghost row keyed by this, and composers spin their send
  /// button. The daemon emits a `spawning` progress ack but it rides
  /// FIFO between request/reply, so we can't observe it from `call()`;
  /// flipping local state at the call-site is equivalent for the UI.
  spawningSession = $state<{
    agent: Harness;
    projectId?: string;
    firstPrompt?: string;
  } | null>(null);
  /// Journal entries + their folded message groups. A standalone reactive
  /// store composed onto AppState (same pattern as `bg = new BgAttach(...)`);
  /// component-facing reads still go through the passthrough getters below so
  /// the `app.entries` / `app.groups` / `app.lastTurnError` /
  /// `app.currentAgentStatus` surface is unchanged.
  transcript = new Transcript();
  get entries() {
    return this.transcript.entries;
  }
  get groups() {
    return this.transcript.groups;
  }
  get lastTurnError() {
    return this.transcript.lastTurnError;
  }
  get currentAgentStatus() {
    return this.transcript.currentAgentStatus;
  }
  inputAcquired = $state(false);
  awaitingTurn = $state(false);
  /// Running totals for the current turn, reset on send. Updated whenever a
  /// `usage` frame lands. Cleared when the turn ends (Result).
  currentUsage = $state<{
    input_tokens: number;
    output_tokens: number;
    cost_usd: number;
  } | null>(null);
  lastError = $state<string | null>(null);
  /// Last error from `connect()` — distinct from `lastError` because the
  /// connection screen renders it inline (with a Retry button) rather than
  /// as a toast. Cleared whenever a fresh `connect()` attempt starts.
  connectionError = $state<string | null>(null);
  /// Per-session "turn in flight" flag, populated by background attaches
  /// to every live session. Sidebar pulses any row whose entry here is
  /// `true`. Survives switching between sessions — the current session's
  /// `awaitingTurn` is kept in sync via `onFrame`.
  activeSessions = $state<Record<string, boolean>>({});
  /// Titles derived from the first `user_prompt` of each session. Populated
  /// lazily — see `loadTitles` after `refreshSessions`, plus the `onFrame`
  /// path that fills in the current session as soon as the user sends their
  /// first message. `null` means "we tried but the journal has no prompt
  /// yet"; missing key means "not loaded".
  titles = $state<Record<string, string | null>>({});
  /// First prompt for a freshly spawned session — rendered optimistically
  /// by ChatView while `refreshSessions` + `openSession` + `acquire_input`
  /// round-trip. Cleared on the real `user_prompt` frame or on spawn-flow
  /// failure. `$state.raw` — replaced atomically, never mutated in place.
  pendingFirstPrompt = $state.raw<{ session: string; text: string } | null>(null);
  /// Monotonic counter — bumped to request focus into the active composer
  /// even when no route change happened (e.g. clicking "New chat" while
  /// already on `/`). Composers `$effect` on this and call `.focus()`.
  composerFocusTick = $state(0);
  /// One-shot text the next Composer mount (or focus tick) should splice
  /// into the draft. Cleared by the composer the moment it consumes the
  /// value, so the same text never gets injected twice. Used by the `+`
  /// buttons on /agents and /skills to drop a builder-skill body into a
  /// fresh draft on /.
  composerPrefill = $state<string | null>(null);
  projects = $state<Project[]>([]);
  expandedProjects = $state<Record<string, boolean>>(
    lsGetJSON<Record<string, boolean>>(LS.expandedProjects, {}),
  );
  /// Sessions with an in-flight pin/unpin PUT. Sidebar pin buttons disable
  /// themselves while their session id is in here so a double-click can't
  /// fire two overlapping RMWs.
  pinningSessions = $state<Record<string, boolean>>({});
  /// Sessions with an in-flight title PUT. The inline editor disables itself
  /// while its session id is in here so a double-click on the row can't fire
  /// two overlapping RMWs against `tags.title`.
  renamingSessions = $state<Record<string, boolean>>({});
  /// Projects with an in-flight rename PUT. Same role as `renamingSessions`
  /// but for project headers.
  renamingProjects = $state<Record<string, boolean>>({});
  /// Sessions with an in-flight `close` (archive) RPC. The three-dots menu
  /// disables its Archive item while a session id sits here so a double
  /// trigger can't fire two overlapping closes.
  closingSessions = $state<Record<string, boolean>>({});

  focusComposer() {
    this.composerFocusTick++;
  }

  /// Queue `text` for the next composer mount or focus tick to splice into
  /// its draft. Pairs with a focus bump so the composer notices even when
  /// it's already mounted on `/`.
  prefillComposer(text: string) {
    this.composerPrefill = text;
    this.composerFocusTick++;
  }

  /// Atomic take — returns and clears the pending prefill in one step.
  /// Centralizing the read/clear here keeps the one-shot contract from
  /// drifting if a second consumer appears, and avoids the read/clear gap
  /// where two effects could see the same non-null value (e.g. HMR-driven
  /// transient double-mounts).
  takeComposerPrefill(): string | null {
    const v = this.composerPrefill;
    if (v !== null) this.composerPrefill = null;
    return v;
  }

  private unsubscribeFrames: (() => void) | null = null;
  /// Background attach lifecycle controller — keeps a frame sub on every
  /// live session that isn't currently focused, so the sidebar pulse stays
  /// accurate regardless of which session the user is looking at.
  private bg = new BgAttach({
    currentSession: () => this.currentSession,
    markActive: (s, a) => this.markActive(s, a),
  });
  /// Monotonic counter bumped at the top of every `openSession` call.
  /// In-flight opens compare their captured value after each `await`;
  /// if it no longer matches `openEpoch`, the call bails out before
  /// touching reactive state. Prevents A→B rapid switches from letting
  /// A's late responses clobber B's display.
  private openEpoch = 0;

  constructor() {
    const unsub = royClient.onStatus((s) => (this.status = s));
    // Vite re-evaluates this module on HMR; without dispose, each pass leaks
    // a status listener (and a stale `this` reference) into RoyClient.
    if (import.meta.hot) import.meta.hot.dispose(unsub);
  }

  private persistExpanded() {
    lsSetJSON(LS.expandedProjects, this.expandedProjects);
  }

  private markActive(session: string, active: boolean) {
    if (!!this.activeSessions[session] === active) return;
    if (active) this.activeSessions[session] = true;
    else delete this.activeSessions[session];
  }

  /** Public surface for components that want to set/clear a session title
   *  without poking at `this.titles` directly. */
  setTitle(session: string, text: string) {
    if (this.titles[session] === text) return;
    this.titles[session] = text;
  }

  dismissError() {
    this.lastError = null;
  }

  toggleExpand(projectId: string) {
    this.expandedProjects[projectId] = !this.expandedProjects[projectId];
    this.persistExpanded();
  }

  /** True iff the session carries `tags.pinned == "true"`. The pin marker
   *  itself lives in the management-side `session_meta.tags` JSON map. */
  isPinned(s: SessionInfo): boolean {
    return s.tags?.pinned === 'true';
  }

  /** Toggle `tags.pinned` for `session`. RMW against the cached session row
   *  (no extra GET — the cached tags came from the same `/sessions` list the
   *  sidebar is rendering). The whole-map PUT is the wire op management
   *  exposes; we merge locally so other tags survive. */
  async setPinned(session: string, pinned: boolean): Promise<void> {
    const target = this.sessionById.get(session);
    if (!target) return;
    if (this.isPinned(target) === pinned) return;
    await this.updateSessionTags(session, this.pinningSessions, (tags) => {
      const next = { ...tags };
      if (pinned) next.pinned = 'true';
      else delete next.pinned;
      return next;
    });
  }

  /** Shared optimistic-RMW for whole-map tag PUTs. Snapshot the target
   *  session's tags, apply the caller's `buildTags`, fire the PUT, roll back
   *  by re-mapping the *current* `live`/`archived` arrays on failure.
   *  `inFlightMap` is the per-operation guard against double-clicks
   *  (`pinningSessions` for pin, `renamingSessions` for title rename, …).
   *  Re-reads `this.live`/`this.archived` at every mutation step (not captured
   *  snapshots) so a concurrent `refreshSessions()` doesn't get clobbered on
   *  rollback — we only ever restore the target row's tags, leaving the rest
   *  of whatever refresh produced intact. Mutates both `live` and `archived`
   *  because a session can briefly straddle both lists during a close/refresh;
   *  the row-id check inside the `map` callback is a no-op on rows that don't
   *  match. Errors are surfaced via `this.lastError`; we don't re-throw — the
   *  fire-and-forget call sites (`void app.setPinned(...)`) would turn a throw
   *  into an unhandled promise rejection. */
  private async updateSessionTags(
    session: string,
    inFlightMap: Record<string, boolean>,
    buildTags: (current: Record<string, string>) => Record<string, string>,
  ): Promise<void> {
    if (inFlightMap[session]) return;
    const target = this.sessionById.get(session);
    if (!target) return;
    const prevTags = target.tags ?? {};
    const nextTags = buildTags(prevTags);
    inFlightMap[session] = true;
    const setTags = (tags: Record<string, string>) => {
      const apply = (s: SessionInfo) => (s.session === session ? { ...s, tags } : s);
      this.live = this.live.map(apply);
      this.archived = this.archived.map(apply);
    };
    setTags(nextTags);
    try {
      await mgmtSessions.putTags(session, nextTags);
    } catch (e) {
      // Rollback against the *fresh* arrays — a concurrent refreshSessions()
      // may have replaced them with new server data while the PUT was in
      // flight, and captured snapshots would silently discard that refresh.
      setTags(prevTags);
      this.lastError = errMsg(e);
    } finally {
      delete inFlightMap[session];
    }
  }

  /** In-flight guard against double-clicks: no-op if `key` is already busy in
   *  `map`, otherwise mark it busy, run `fn`, and clear the flag in a finally.
   *  Error handling stays in `fn` so each caller keeps its own throw/swallow
   *  semantics — `withBusy` only owns the set/clear of the guard. */
  private async withBusy(
    map: Record<string, boolean>,
    key: string,
    fn: () => Promise<void>,
  ): Promise<void> {
    if (map[key]) return;
    map[key] = true;
    try {
      await fn();
    } finally {
      delete map[key];
    }
  }

  async connect(url: string, token: string) {
    this.lastError = null;
    this.connectionError = null;
    try {
      await royClient.connect(url, token);
      await this.refreshSessions();
      void harnessesConfig.refresh();
    } catch (e) {
      const msg = errMsg(e);
      this.connectionError = msg;
      this.lastError = msg;
    }
  }

  async refreshSessions() {
    // Daemon and management are independent backends — a flaky management
    // shouldn't blank out the daemon's session list. Daemon calls first
    // (failure here is fatal — sidebar is gone either way); management
    // enrichment runs as a separate best-effort step that degrades to
    // empty meta when /management/* is unreachable.
    let live: { sessions: SessionInfo[] };
    let archived: { sessions: SessionInfo[] };
    try {
      [live, archived] = await Promise.all([
        royClient.call({ op: 'list' }, 'listed'),
        royClient.call({ op: 'list_archived' }, 'listed_archived'),
      ]);
    } catch (e) {
      this.lastError = errMsg(e);
      return;
    }

    let meta = new Map<string, { project_id: string | null; tags: Record<string, string> }>();
    let projectList: Project[] = this.projects;
    try {
      const [projects, sessionRows] = await Promise.all([
        mgmtProjects.list(),
        mgmtSessions.list(),
      ]);
      projectList = projects;
      meta = new Map(sessionRows.map((r) => [r.session_id, r]));
    } catch (e) {
      // Daemon view stays usable — surface the error but keep going with
      // empty meta so existing components still render sessions.
      this.lastError = errMsg(e);
    }

    // Splice management-owned meta (project_id, tags) onto the daemon's
    // SessionInfo so existing components keep reading `s.project_id` and
    // `s.tags` directly. Sessions absent from management get nullish meta.
    const enrich = (s: SessionInfo): SessionInfo => {
      const m = meta.get(s.session);
      return {
        ...s,
        project_id: m?.project_id ?? undefined,
        tags: m?.tags ?? {},
      };
    };
    this.live = live.sessions.map(enrich);
    this.archived = archived.sessions.map(enrich);
    this.projects = projectList;
    // Fire-and-forget — titles trickle in over the next few ticks; sidebar
    // shows fallback hashes meanwhile.
    void this.loadTitles([
      ...live.sessions.map((s) => s.session),
      ...archived.sessions.map((s) => s.session),
    ]);
    // Wire up background attaches so the sidebar pulse stays accurate
    // for sessions the user isn't currently looking at. Drops attaches
    // for sessions that disappeared (closed / archived).
    void this.bg.reconcile(live.sessions.map((s) => s.session));
  }

  /** Lazily fetch the first `user_prompt` of each session's journal to use
   *  as the row's display title. Cached in `titles` — re-runs are no-ops.
   *  Concurrency capped so a startup with N sessions doesn't issue N
   *  parallel `read_journal`s and starve the foreground `acquire_input`. */
  async loadTitles(sessions: string[]) {
    const pending = sessions.filter((s) => !(s in this.titles));
    const CONCURRENCY = 4;
    // The first user_prompt is almost always within the first handful of
    // entries (system header, then prompt). Capping at 8 trims a multi-MB
    // journal pull down to a few KB on startup for long sessions.
    const HEAD_ENTRIES = 8;
    let cursor = 0;
    const worker = async () => {
      while (cursor < pending.length) {
        const s = pending[cursor++];
        try {
          const snap = await royClient.call(
            { op: 'read_journal', session: s, max_entries: HEAD_ENTRIES },
            'journal_read',
          );
          const prompt = snap.entries.find((e) => e.event.type === 'user_prompt');
          this.titles[s] =
            prompt && prompt.event.type === 'user_prompt' ? prompt.event.text : null;
        } catch {
          // Leave unset — sidebar falls back to short hash.
        }
      }
    };
    await Promise.all(Array.from({ length: Math.min(CONCURRENCY, pending.length) }, worker));
  }

  /** Pretty title for a session. Resolution order:
   *  1. Explicit user override at `tags.title` (set via the inline rename).
   *  2. First `user_prompt` lazily cached in `this.titles`.
   *  3. Short-hash fallback (`…abc123`) when neither is available.
   *  Returns the trimmed override verbatim so the user's exact text stands;
   *  the inferred title still goes through `formatTitle` to squash whitespace. */
  titleFor(session: string): string {
    const override = this.titleOverride(session);
    if (override) return override;
    const t = this.titles[session];
    if (!t) return `…${session.slice(-6)}`;
    return formatTitle(t);
  }

  /** Raw `tags.title` for `session`, trimmed and non-empty, or `null`.
   *  Helper for callers that want to distinguish override from inferred
   *  (e.g. the inline editor pre-fills with the override when present). */
  titleOverride(session: string): string | null {
    const t = this.sessionById.get(session)?.tags?.title;
    if (typeof t !== 'string') return null;
    const trimmed = t.trim();
    return trimmed.length === 0 ? null : trimmed;
  }

  /** Set / clear the user-overridable session title at `tags.title`. Same
   *  optimistic-RMW + rollback pattern as `setPinned`: snapshot the cached
   *  rows, patch them in place so the sidebar re-renders immediately, fire
   *  the whole-map PUT, restore on failure. An empty/whitespace `title`
   *  removes the override (display falls back to inferred title). */
  async setSessionTitle(session: string, title: string): Promise<void> {
    const target = this.sessionById.get(session);
    if (!target) return;
    const next = title.trim();
    const prev = (target.tags?.title ?? '').trim();
    if (next === prev) return; // no-op
    await this.updateSessionTags(session, this.renamingSessions, (tags) => {
      const out = { ...tags };
      if (next.length === 0) delete out.title;
      else out.title = next;
      return out;
    });
  }

  /** Open the given session: snapshot its journal, subscribe to live
   *  frames, acquire the input lease. Latest-wins: every call bumps
   *  `openEpoch`; in-flight calls bail before touching reactive state
   *  whenever they discover a newer call superseded them. */
  async openSession(session: string) {
    // Skip only when we're already *fully* attached to this session. The
    // archived → resume upgrade keeps `currentSession` stable but flips
    // it from read-only (no attach, no lease) to live; we need to re-run
    // the setup in that case.
    if (this.currentSession === session && this.inputAcquired) return;

    const epoch = ++this.openEpoch;
    const isStale = () => this.openEpoch !== epoch;

    // Release the prior session's input lease before grabbing a new one —
    // the daemon caps live leases per connection, so a stale hold on the
    // session we're leaving would block `acquire_input` on the new one.
    // Unconditional: even if our local `inputAcquired` says false, the
    // daemon may still associate this connection with the old session
    // (failed prior acquire, race, etc.). A no-op release is cheaper
    // than a wrong-session lease.
    const prevSession = this.currentSession;
    // Re-entry on the same session (e.g. archived → resume upgrade): keep
    // the loaded journal so the chat doesn't flash-empty between calls.
    const reuseJournal = prevSession === session && this.transcript.entries.length > 0;
    this.detachFrames();
    if (prevSession && prevSession !== session) {
      try {
        await royClient.call(
          { op: 'release_input', session: prevSession },
          'input_released',
        );
      } catch {
        // Best-effort: if release fails, acquire below will surface the
        // real problem via `lease.acquired === false`.
      }
    }
    if (isStale()) return;

    // Stepping into a session: drop its background-only attach so the
    // upcoming foreground attach is the sole frame consumer for it.
    this.bg.drop(session);
    this.currentSession = session;
    this.inputAcquired = false;
    this.lastError = null;
    if (!reuseJournal) {
      this.currentAgent = null;
      this.currentModel = null;
      this.transcript.clear();
      // Queue is per-session — typing into one session's composer shouldn't
      // leak into the next session if we switch away.
      this.queue = [];
    }

    this.loadingSession = true;
    try {
      let nextSeq: number;
      if (reuseJournal) {
        const last = this.transcript.entries[this.transcript.entries.length - 1];
        nextSeq = (last?.seq ?? -1) + 1;
      } else {
        // Snapshot first — works for both live and archived sessions.
        const snap = await royClient.call(
          { op: 'read_journal', session },
          'journal_read',
        );
        if (isStale()) return;
        this.transcript.replace(snap.entries);
        nextSeq = snap.next_seq;
      }

      // If the session is live, attach + acquire input.
      if (this.live.some((s) => s.session === session)) {
        const attached = await royClient.call(
          { op: 'attach', session, from_seq: nextSeq },
          'attached',
        );
        if (isStale()) {
          // Our attach landed but a newer open already moved on. Detach
          // so the daemon doesn't keep a zombie attach for us.
          void royClient.call({ op: 'detach', session }, 'detached').catch(() => {});
          return;
        }
        this.currentAgent = (attached.harness as Harness) || null;
        this.currentModel = attached.model || null;
        // Tear down any leftover sub before installing a new one so a
        // stale handle from a previous open can't overwrite ours.
        this.detachFrames();
        this.unsubscribeFrames = royClient.subscribeFrames(session, (e) =>
          this.onFrame(session, e),
        );
        const lease = await royClient.call(
          { op: 'acquire_input', session },
          'input_acquired',
        );
        if (isStale()) return;
        this.inputAcquired = lease.acquired;
        if (!lease.acquired) {
          this.lastError =
            'Input lease held by another connection — composer is read-only until it releases.';
        }
      }

      // The previous session (if any) is still live but lost its
      // foreground sub when we switched away. Pick it up via the
      // background path so its sidebar pulse keeps updating.
      if (prevSession && prevSession !== session && this.live.some((s) => s.session === prevSession)) {
        void this.bg.reconcile(this.live.map((s) => s.session));
      }
    } catch (e) {
      if (!isStale()) this.lastError = errMsg(e);
    } finally {
      if (!isStale()) this.loadingSession = false;
    }
  }

  /** Resurrect an archived session into a live one, then open it. */
  async resumeAndOpen(session: string) {
    if (this.resumingSession) return;
    this.resumingSession = session;
    try {
      // Pre-load history in read-only mode so the user sees the chat while
      // the multi-second `resume` RPC is in flight.
      if (this.currentSession !== session) {
        await this.openSession(session);
      }
      await royClient.call({ op: 'resume', session }, 'resumed');
      await this.refreshSessions();
      await this.openSession(session);
    } catch (e) {
      this.lastError = errMsg(e);
    } finally {
      if (this.resumingSession === session) this.resumingSession = null;
    }
  }

  /** Spawn a new session, optionally with a pending first prompt. Returns
   *  the session id as soon as the daemon hands it back; the rest of the
   *  open flow (refresh + snapshot + attach + acquire) finishes in the
   *  background and auto-submits the pending prompt once the input lease
   *  is held. ChatView reads `pendingFirstPrompt` to render an optimistic
   *  bubble until then. */
  async createSession(opts: {
    agent: Harness;
    project_id?: string;
    scope?: 'personal' | 'team';
    team_id?: string;
    model?: string;
    permission?: 'allow' | 'deny';
    firstPrompt?: string;
    /** Saved Agent persona to inject — forwarded to mgmtSessions.create
     *  as `system_prompt` / `agent_name`. Omit for a plain harness spawn. */
    persona?: { prompt: string; name: string };
    /** MCP connections to attach. Forwarded as `connection_ids` to POST
     *  /sessions. Daemon-side rejects non-claude presets when non-empty. */
    connection_ids?: string[];
  }) {
    let sessionId: string;
    this.spawningSession = {
      agent: opts.agent,
      projectId: opts.project_id || undefined,
      firstPrompt: opts.firstPrompt,
    };
    try {
      // Spawn-with-meta is a management-coordinator concern now: the daemon
      // wire only takes `cwd`, but project_id needs to resolve to a path
      // and tags/agent_name must be persisted alongside the spawn. POST
      // /sessions does that atomically (including the rollback Close on
      // meta-write failure).
      const created = await mgmtSessions.create({
        harness: opts.agent,
        project_id: opts.project_id || undefined,
        scope: opts.scope,
        team_id: opts.team_id || undefined,
        model: opts.model || undefined,
        permission: opts.permission || undefined,
        system_prompt: opts.persona?.prompt,
        agent_name: opts.persona?.name,
        connection_ids:
          opts.connection_ids && opts.connection_ids.length > 0
            ? opts.connection_ids
            : undefined,
      });
      sessionId = created.session_id;
    } catch (e) {
      this.lastError = errMsg(e);
      throw e;
    } finally {
      // Pending-prompt + sidebar refresh take over the optimistic UX from
      // here, so the ghost row needn't linger across the post-spawn boot.
      this.spawningSession = null;
    }
    if (opts.firstPrompt) {
      this.pendingFirstPrompt = { session: sessionId, text: opts.firstPrompt };
      // Seed the title eagerly so the sidebar row shows the prompt text
      // before the daemon journals it.
      this.setTitle(sessionId, opts.firstPrompt);
    }
    void (async () => {
      try {
        await this.refreshSessions();
        await this.openSession(sessionId);
        const pending = this.pendingFirstPrompt;
        // Identity check (not just session id) defeats a race where
        // onFrame's user_prompt landed first and already cleared pending.
        if (
          pending &&
          pending === this.pendingFirstPrompt &&
          pending.session === sessionId &&
          this.inputAcquired
        ) {
          this.pendingFirstPrompt = null;
          this.submit(pending.text);
        } else if (
          this.pendingFirstPrompt?.session === sessionId &&
          !this.inputAcquired
        ) {
          // Lease wasn't acquired (e.g. another connection holds it).
          // Drop pending + surface the loss instead of leaving a forever
          // "Spinning up session…" pulse with the user's text in limbo.
          this.pendingFirstPrompt = null;
          this.lastError =
            'Could not deliver first prompt — no input lease on the new session.';
        }
      } catch (e) {
        this.lastError = errMsg(e);
        if (this.pendingFirstPrompt?.session === sessionId) {
          this.pendingFirstPrompt = null;
        }
      }
    })();
    return sessionId;
  }

  /** FIFO of messages typed while a turn was in flight. Drained one-by-one
   *  in `onFrame` when a terminal Result lands. */
  queue = $state<{ id: string; text: string }[]>([]);
  private queueCounter = 0;

  /** Single submit entry-point for the composer. If a turn is already in
   *  flight, the message lands in `queue` and is sent automatically when
   *  the agent finishes. Without a lease we refuse outright — queuing
   *  would silently lose the message (the daemon never echoes a
   *  matching `result` to trigger the drain). */
  submit(text: string) {
    const trimmed = text.trim();
    if (!trimmed || !this.currentSession) return;
    if (!this.inputAcquired) {
      this.lastError = 'No input lease — cannot send message.';
      return;
    }
    if (this.awaitingTurn || this.queue.length > 0) {
      this.queue = [...this.queue, { id: `q${++this.queueCounter}`, text: trimmed }];
      return;
    }
    this.dispatch(trimmed);
  }

  private dispatch(text: string) {
    if (!this.currentSession) return;
    this.awaitingTurn = true;
    this.markActive(this.currentSession, true);
    this.currentUsage = null;
    try {
      royClient.fire({ op: 'send', session: this.currentSession, text });
    } catch (e) {
      this.lastError = errMsg(e);
      this.awaitingTurn = false;
      if (this.currentSession) this.markActive(this.currentSession, false);
    }
  }

  removeQueued(id: string) {
    this.queue = this.queue.filter((m) => m.id !== id);
  }

  updateQueued(id: string, text: string) {
    const trimmed = text.trim();
    if (!trimmed) {
      this.removeQueued(id);
      return;
    }
    this.queue = this.queue.map((m) => (m.id === id ? { ...m, text: trimmed } : m));
  }

  /** Move a queued item to the front so it dispatches next. */
  promoteQueued(id: string) {
    const idx = this.queue.findIndex((m) => m.id === id);
    if (idx <= 0) return;
    const item = this.queue[idx];
    this.queue = [item, ...this.queue.filter((_, i) => i !== idx)];
  }

  /** Swap the LLM label associated with the current session. The daemon
   *  rewrites SessionMetadata.model on disk and echoes ModelChanged so we
   *  reflect it in `currentModel` immediately. Other tabs see the new
   *  value on their next attach — there's no cross-connection broadcast
   *  on the daemon side yet. */
  async setModel(model: string) {
    if (!this.currentSession) return;
    if (model === this.currentModel) return;
    try {
      const ev = await royClient.call(
        { op: 'set_model', session: this.currentSession, model },
        'model_changed',
      );
      this.currentModel = ev.model;
    } catch (e) {
      // Re-throw so the caller (e.g. ChatView's picker) can roll back
      // its optimistic UI state. We still publish to `lastError` for
      // the global toast.
      this.lastError = errMsg(e);
      throw e;
    }
  }

  /** Stop the in-flight turn. The agent receives ACP `session/cancel`; the
   * journal lands a terminal Result with stop_reason: cancelled. */
  cancelTurn() {
    if (!this.currentSession || !this.awaitingTurn) return;
    try {
      royClient.fire({ op: 'cancel_turn', session: this.currentSession });
    } catch (e) {
      this.lastError = errMsg(e);
    }
  }

  async closeSession(session: string) {
    await this.withBusy(this.closingSessions, session, async () => {
      try {
        await royClient.call({ op: 'close', session }, 'closed');
        if (this.currentSession === session) this.resetFocusedSession();
        await this.refreshSessions();
      } catch (e) {
        this.lastError = errMsg(e);
      }
    });
  }

  /** Archive a live session — alias for `closeSession`. "Archive" is the
   *  product-side verb users see in the UI (close + keep the journal on
   *  disk so the session can be resumed later). */
  archiveSession(session: string): Promise<void> {
    return this.closeSession(session);
  }

  /** Permanently delete an archived session's journal + metadata from disk. */
  async deleteArchive(session: string): Promise<void> {
    await royClient.call({ op: 'delete_archive', session }, 'deleted');
    if (this.currentSession === session) this.resetFocusedSession();
    await this.refreshSessions();
  }

  async createProject(name: string, team_id?: string): Promise<Project> {
    const project = await mgmtProjects.create(name, team_id);
    this.projects = [...this.projects, project];
    this.expandedProjects[project.id] = true;
    this.persistExpanded();
    return project;
  }

  /** Optimistic read-modify-write for a single project field set. Snapshot
   *  `this.projects`, apply `patch` to the matching row, fire the management
   *  PUT, and roll back (restoring the snapshot + surfacing `lastError`) on
   *  failure. Re-throws so callers can decide whether to propagate the error
   *  (moveProject re-throws so the drag UI can revert) or swallow it
   *  (renameProject). */
  private async patchProject(id: string, patch: Partial<Project>): Promise<void> {
    const prev = this.projects;
    this.projects = prev.map((p) => (p.id === id ? { ...p, ...patch } : p));
    try {
      await mgmtProjects.update(id, patch);
    } catch (e) {
      this.projects = prev;
      this.lastError = errMsg(e);
      throw e;
    }
  }

  /** Move a project between scopes. Optimistic with rollback on failure;
   *  re-throws so the caller can roll back its optimistic drag UI. */
  async moveProject(id: string, team_id: string | null): Promise<void> {
    const target = this.projects.find((p) => p.id === id);
    if (!target) return;
    if ((target.team_id ?? null) === team_id) return;
    await this.patchProject(id, { team_id });
  }

  /** Rename a project. Caller is responsible for trimming + length validation
   *  before calling; this method no-ops when the trimmed name matches the
   *  current one. Optimistic update with rollback, same shape as `setPinned`. */
  async renameProject(id: string, name: string): Promise<void> {
    const trimmed = name.trim();
    if (trimmed.length === 0) return;
    const target = this.projects.find((p) => p.id === id);
    if (!target) return;
    if (target.name === trimmed) return;
    await this.withBusy(this.renamingProjects, id, async () => {
      // patchProject re-throws on failure (already rolled back + set
      // lastError); rename is fire-and-forget from the UI so we swallow.
      try {
        await this.patchProject(id, { name: trimmed });
      } catch {
        // already handled in patchProject
      }
    });
  }

  /** O(1) lookup index of every session by id (live ∪ archived). Built once
   *  per dependency change rather than re-scanning on every `titleOverride` /
   *  `setPinned` / `setSessionTitle` call. The archived entry wins on the
   *  brief window where a row straddles both lists during a close/refresh —
   *  matches the daemon's view-of-truth ordering. */
  sessionById = $derived.by(() => {
    const m = new Map<string, SessionInfo>();
    for (const s of this.live) m.set(s.session, s);
    for (const s of this.archived) m.set(s.session, s);
    return m;
  });

  /** Walk `live` then `archived` collecting rows matching `pred`, de-duped by
   *  session id (a row can briefly straddle both lists during a close/refresh).
   *  Live-before-archived order is the daemon-natural convention every sidebar
   *  getter uses since SessionInfo carries no activity timestamp on the wire. */
  private mergeDedup(pred: (s: SessionInfo) => boolean): SessionInfo[] {
    const seen = new Set<string>();
    const out: SessionInfo[] = [];
    for (const s of [...this.live, ...this.archived]) {
      if (!pred(s) || seen.has(s.session)) continue;
      seen.add(s.session);
      out.push(s);
    }
    return out;
  }

  /** All pinned sessions across live + archived + background, in a single
   *  flat list rendered at the very top of the sidebar. SessionInfo carries
   *  no activity timestamp on the wire, so we preserve daemon order — live
   *  rows first (daemon-natural), then archived rows. Other sidebar getters
   *  use the same convention. De-duped by session id since a row can briefly
   *  straddle both lists during a close/refresh. */
  pinnedSessions = $derived.by(() => this.mergeDedup((s) => this.isPinned(s)));

  orphanLive = $derived(
    this.live.filter((s) => s.project_id == null && !this.isBg(s) && !this.isPinned(s)),
  );

  orphanArchived = $derived(
    this.archived.filter((s) => s.project_id == null && !this.isBg(s) && !this.isPinned(s)),
  );

  /** Live sessions excluding scheduler-spawned ones AND pinned ones (the
   *  latter render in the dedicated Pinned group at the top so each session
   *  appears in exactly one place). ProjectGroup reads this so a background
   *  fire never double-renders in its project AND in the Background section. */
  regularLive = $derived(this.live.filter((s) => !this.isBg(s) && !this.isPinned(s)));

  /** Archived sessions excluding scheduler-spawned ones and pinned ones. */
  regularArchived = $derived(this.archived.filter((s) => !this.isBg(s) && !this.isPinned(s)));

  /** Sessions tagged by roy-scheduler (Plan B): any tag key starting with
   *  `roy-scheduler:`. Live first, then archived; de-duped by session id
   *  since a row can briefly straddle both lists during a close/refresh.
   *  Pinned background sessions surface in the dedicated Pinned group only. */
  backgroundSessions = $derived.by(() =>
    this.mergeDedup((s) => this.isBg(s) && !this.isPinned(s)),
  );

  /** True iff this session was spawned by roy-scheduler (Plan B). Membership
   *  is decided by tag-key prefix so any new `roy-scheduler:*` tag added in
   *  the future is captured without a code change here. */
  isBg(s: SessionInfo): boolean {
    return !!s.tags && Object.keys(s.tags).some((k) => k.startsWith('roy-scheduler:'));
  }

  /** True iff this session is currently archived (present in `archived`, not
   *  `live`). Used by Background rows to route the row-action to either
   *  `close` (live) or `delete_archive` (archived). */
  isArchived(session: string): boolean {
    return this.archived.some((s) => s.session === session);
  }

  async deleteProject(id: string): Promise<void> {
    // Management drops the project row only; the FK on session_meta is
    // ON DELETE SET NULL, so existing sessions detach (project_id becomes
    // null) rather than getting cascade-killed. To reflect this without a
    // full refresh, null out project_id on every cached session that
    // belonged to this project — both live and archived.
    await mgmtProjects.remove(id);
    const detach = (s: SessionInfo): SessionInfo =>
      s.project_id === id ? { ...s, project_id: undefined } : s;
    this.live = this.live.map(detach);
    this.archived = this.archived.map(detach);
    this.projects = this.projects.filter((p) => p.id !== id);
    delete this.expandedProjects[id];
    this.persistExpanded();
  }

  private detachFrames() {
    this.unsubscribeFrames?.();
    this.unsubscribeFrames = null;
  }

  /** Full teardown of whatever session is currently focused: detach frames,
   *  null out current session/agent/model, clear the transcript, and reset
   *  the per-turn flags (inputAcquired/awaitingTurn/currentUsage/queue).
   *  Shared by closeSession, deleteArchive, and clearCurrent — the latter
   *  additionally kicks off a bg.reconcile after calling this. */
  private resetFocusedSession() {
    this.detachFrames();
    this.currentSession = null;
    this.currentAgent = null;
    this.currentModel = null;
    this.transcript.clear();
    this.inputAcquired = false;
    this.awaitingTurn = false;
    this.currentUsage = null;
    this.queue = [];
  }

  /** Clear whatever session is currently focused. Used by URL routing when
   *  the user navigates back to `/` (no session in the path). */
  clearCurrent() {
    this.resetFocusedSession();
    // The session we just left has no foreground subscriber now — pick
    // up its activity via the background attach path so the sidebar
    // pulse keeps reflecting reality.
    void this.bg.reconcile(this.live.map((s) => s.session));
  }

  private onFrame(session: string, entry: JournalEntry) {
    this.transcript.append(entry);
    if (entry.event.type === 'user_prompt') {
      if (!this.titles[session]) this.setTitle(session, entry.event.text);
      // Clear pending against the frame's *own* session, not currentSession —
      // a rapid spawn-A → switch-B leaves A's foreground sub alive briefly,
      // and A's user_prompt should still cancel A's optimistic bubble.
      if (this.pendingFirstPrompt?.session === session) {
        this.pendingFirstPrompt = null;
      }
    }
    if (entry.event.type === 'usage') {
      // Accumulate: opencode streams usage_update repeatedly, only the latest
      // numbers are interesting. Treat any non-null field as the new truth.
      const u = entry.event;
      const prev = this.currentUsage ?? {
        input_tokens: 0,
        output_tokens: 0,
        cost_usd: 0,
      };
      const next = {
        input_tokens: u.input_tokens ?? prev.input_tokens,
        output_tokens: u.output_tokens ?? prev.output_tokens,
        cost_usd: u.cost_usd ?? prev.cost_usd,
      };
      if (
        !this.currentUsage ||
        next.input_tokens !== this.currentUsage.input_tokens ||
        next.output_tokens !== this.currentUsage.output_tokens ||
        next.cost_usd !== this.currentUsage.cost_usd
      ) {
        this.currentUsage = next;
      }
    }
    // Sidebar pulse follows the frame's own session — stays correct even
    // mid-switch when the previous foreground sub is still draining.
    if (entry.event.type === 'user_prompt') this.markActive(session, true);
    else if (entry.event.type === 'result') this.markActive(session, false);
    // awaitingTurn + queue drain are about the *currently focused*
    // composer — ignore tail frames from a session the user just left.
    if (entry.event.type === 'result' && session === this.currentSession) {
      this.awaitingTurn = false;
      if (this.queue.length > 0 && this.inputAcquired) {
        const [next, ...rest] = this.queue;
        this.queue = rest;
        this.dispatch(next.text);
      }
    }
  }
}

export const app = new AppState();
