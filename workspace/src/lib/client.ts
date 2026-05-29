import {
  WS_SUBPROTOCOL_MARKER,
  type ClientCommand,
  type JournalEntry,
  type ServerEvent,
  type ServerEventKind,
} from './wire';

export type ConnectionStatus = 'idle' | 'connecting' | 'open' | 'closed' | 'error';

type Pending = {
  expected: ServerEventKind;
  resolve: (ev: ServerEvent) => void;
  reject: (err: Error) => void;
};

export class RoyClient {
  private ws: WebSocket | null = null;
  private queue: Pending[] = [];
  private frameSubs = new Map<string, Set<(entry: JournalEntry) => void>>();
  private statusSubs = new Set<(s: ConnectionStatus) => void>();
  private _status: ConnectionStatus = 'idle';

  get status(): ConnectionStatus {
    return this._status;
  }

  /**
   * Connect to the roy gateway's WebSocket relay. Resolves when the socket
   * reaches OPEN, rejects if it errors, closes before opening, or fails to
   * upgrade within `timeoutMs` (default 10s).
   *
   * The gateway authenticates via the WebSocket subprotocol slot — browsers
   * can't set arbitrary headers on `new WebSocket(url, [protocols])`, so the
   * JWT rides `Sec-WebSocket-Protocol` alongside the literal `roy-jwt` marker
   * (two subprotocol values: marker + token). The gateway echoes back only
   * the marker, never the token, so it doesn't leak into the upgrade response
   * headers.
   *
   * Without the timeout a half-open TCP socket (daemon crashed mid-handshake,
   * network stall, etc.) leaves `onopen`/`onerror` silent and the returned
   * Promise pends forever, hanging any awaiting caller (e.g. App.svelte's
   * onMount) with no diagnosis.
   */
  connect(url: string, token: string, timeoutMs = 10_000): Promise<void> {
    if (this.ws) this.close();
    this.setStatus('connecting');
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(url, [WS_SUBPROTOCOL_MARKER, token]);
      this.ws = ws;
      const timer = setTimeout(() => {
        this.setStatus('error');
        try {
          ws.close();
        } catch {
          // already closed
        }
        reject(new Error(`timed out connecting to ${url} after ${timeoutMs}ms`));
      }, timeoutMs);
      ws.onopen = () => {
        clearTimeout(timer);
        this.setStatus('open');
        resolve();
      };
      ws.onerror = () => {
        clearTimeout(timer);
        this.setStatus('error');
        if (ws.readyState !== WebSocket.OPEN) {
          reject(new Error(`failed to connect to ${url}`));
        }
        this.flushQueueWithError('websocket error');
      };
      ws.onclose = () => {
        clearTimeout(timer);
        this.setStatus('closed');
        this.flushQueueWithError('websocket closed');
      };
      ws.onmessage = (msg) => this.handleMessage(msg.data);
    });
  }

  close() {
    this.ws?.close();
    this.ws = null;
  }

  /**
   * Send a command and await the matching server reply. Frame events are not
   * counted as replies — they flow through `subscribeFrames` instead.
   *
   * Rejects with the Error event's message if the daemon answered with an
   * Error of any code.
   */
  call<K extends ServerEventKind>(
    cmd: ClientCommand,
    expected: K,
  ): Promise<Extract<ServerEvent, { kind: K }>> {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      return Promise.reject(new Error('not connected'));
    }
    return new Promise((resolve, reject) => {
      this.queue.push({
        expected,
        resolve: (ev) => resolve(ev as Extract<ServerEvent, { kind: K }>),
        reject,
      });
      this.ws!.send(JSON.stringify(cmd));
    });
  }

  /**
   * Fire-and-forget command. `Send` is the canonical case: the daemon emits
   * no ack on success — the observable effect is the stream of `Frame`
   * events that follows, terminated by a `Result`. Only an `Error` event
   * can come back, and that would resolve whatever command is at the head
   * of the queue (so don't interleave `fire` with pending `call`s).
   */
  fire(cmd: ClientCommand) {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      throw new Error('not connected');
    }
    this.ws.send(JSON.stringify(cmd));
  }

  /**
   * Subscribe to live Frame events for a session. Returns an unsubscribe
   * function.
   */
  subscribeFrames(session: string, cb: (entry: JournalEntry) => void): () => void {
    let set = this.frameSubs.get(session);
    if (!set) {
      set = new Set();
      this.frameSubs.set(session, set);
    }
    set.add(cb);
    return () => {
      const s = this.frameSubs.get(session);
      s?.delete(cb);
      if (s && s.size === 0) this.frameSubs.delete(session);
    };
  }

  onStatus(cb: (s: ConnectionStatus) => void): () => void {
    this.statusSubs.add(cb);
    cb(this._status);
    return () => this.statusSubs.delete(cb);
  }

  private setStatus(s: ConnectionStatus) {
    this._status = s;
    for (const cb of this.statusSubs) cb(s);
  }

  private handleMessage(data: unknown) {
    if (typeof data !== 'string') return;
    let ev: ServerEvent;
    try {
      ev = JSON.parse(data) as ServerEvent;
    } catch (e) {
      console.error('roy-web: invalid JSON from daemon', e, data);
      return;
    }

    if (ev.kind === 'frame') {
      this.dispatchFrame(ev.session, ev.entry);
      return;
    }

    // Progress ack, not the awaited reply — returning here (instead of
    // dequeuing) keeps FIFO matching aligned for the real reply behind it.
    if (ev.kind === 'resuming') {
      return;
    }

    // Every other event resolves the head of the pending queue. The daemon
    // processes commands serially per connection, so FIFO matching is sound.
    const pending = this.queue.shift();
    if (!pending) {
      console.warn('roy-web: unsolicited event', ev);
      return;
    }
    if (ev.kind === 'error') {
      pending.reject(new Error(`${ev.code}: ${ev.message}`));
      return;
    }
    if (ev.kind !== pending.expected) {
      pending.reject(
        new Error(`expected ${pending.expected}, got ${ev.kind}`),
      );
      return;
    }
    pending.resolve(ev);
  }

  private dispatchFrame(session: string, entry: JournalEntry) {
    const set = this.frameSubs.get(session);
    if (!set) return;
    for (const cb of set) cb(entry);
  }

  private flushQueueWithError(reason: string) {
    const q = this.queue;
    this.queue = [];
    for (const p of q) p.reject(new Error(reason));
  }
}

export const royClient = new RoyClient();

// Without this, an HMR-replaced module leaves its old WS alive in stale
// component closures — the orphan keeps holding the daemon input lease and
// starves the new instance's `acquire_input` with `acquired: false`.
if (import.meta.hot) {
  import.meta.hot.dispose(() => royClient.close());
}
