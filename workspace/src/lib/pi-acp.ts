// Adapter for pi-acp's non-standard ACP extension. pi-acp publishes status
// pings as `session/update` notifications whose `sessionUpdate` discriminator
// roy doesn't recognise, so the daemon bubbles them up as `TurnEvent::Raw`.
// All knowledge of that wire shape — the discriminator string, the
// `_meta.piAcp` payload — lives here so the rest of the UI stays
// agent-agnostic.

const STATUS_UPDATE = 'session_info_update';

export interface PiAcpStatus {
  running: boolean;
  queueDepth: number;
}

/** True iff this Raw value is a pi-acp status ping. Callers use this to
 *  drop these frames from the visible chat transcript. */
export function isPiAcpStatusEvent(value: unknown): boolean {
  const v = value as { sessionUpdate?: string } | null;
  return v?.sessionUpdate === STATUS_UPDATE;
}

/** Extract `{running, queueDepth}` from a pi-acp status ping, or `null` if
 *  the value isn't one or doesn't carry the expected `_meta.piAcp` payload. */
export function parsePiAcpStatus(value: unknown): PiAcpStatus | null {
  if (!isPiAcpStatusEvent(value)) return null;
  const meta = (value as { _meta?: { piAcp?: unknown } })._meta?.piAcp as
    | { running?: boolean; queueDepth?: number }
    | undefined;
  if (!meta) return null;
  return {
    running: meta.running === true,
    queueDepth: typeof meta.queueDepth === 'number' ? meta.queueDepth : 0,
  };
}
