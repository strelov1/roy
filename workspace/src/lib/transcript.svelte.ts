// Transcript folding for the chat UI — the journal entries plus their
// rendered, incrementally-folded message groups. Extracted from AppState so
// the folding logic lives next to the `Group` type it produces.

import { classifyFamily, type ToolCall, type ToolFamily } from './tool-formatters';
import { isPiAcpStatusEvent, parsePiAcpStatus, type PiAcpStatus } from './pi-acp';
import type { JournalEntry } from './wire';

/** Rendered message group consumed by `MessageGroups.svelte`. */
export type Group =
  | { kind: 'user'; text: string; key: string; ts_ms: number }
  | { kind: 'assistant'; text: string; key: string; ts_ms: number }
  | { kind: 'thought'; text: string; key: string; ts_ms: number }
  | {
      kind: 'tools';
      family: ToolFamily;
      calls: ToolCall[];
      key: string;
      ts_ms: number;
    }
  | { kind: 'system'; subtype: string; key: string; ts_ms: number }
  | { kind: 'note'; text: string; sourceSession: string | null; key: string; ts_ms: number }
  | { kind: 'error'; stopReason: string; key: string; ts_ms: number }
  | { kind: 'raw'; value: unknown; key: string; ts_ms: number };

export class Transcript {
  entries = $state<JournalEntry[]>([]);
  /// Rendered groups, folded incrementally in `appendToGroups` on every
  /// frame (O(1) per chunk). A `$derived.by` here would re-fold all entries
  /// on each push and balloon a long streaming turn to O(N²).
  groups = $state<Group[]>([]);

  /// Most recent error-result + the `agent_error:` system event explaining
  /// it (typically the real cause, e.g. "Authentication required"). `null`
  /// when the latest result was clean or there's no result yet. Derived —
  /// the scan stops at the first result, so it's O(turn-length) not O(N).
  readonly lastTurnError: { stopReason: string; detail: string | null } | null =
    $derived.by(() => {
      const list = this.entries;
      for (let i = list.length - 1; i >= 0; i--) {
        const e = list[i].event;
        if (e.type !== 'result') continue;
        if (!e.is_error) return null;
        let detail: string | null = null;
        for (let j = i - 1; j >= 0; j--) {
          const prev = list[j].event;
          if (prev.type === 'result') break;
          if (prev.type === 'system' && prev.subtype.startsWith('agent_error:')) {
            detail = prev.subtype.slice('agent_error:'.length).trim();
            break;
          }
        }
        return { stopReason: e.stop_reason, detail };
      }
      return null;
    });

  /** Latest pi-acp status ping for the open session, or null when none has
   *  arrived (which is the steady state for non-pi agents). `$derived.by` so
   *  the tail-scan only fires when `entries` actually changes — matching the
   *  same pattern as `lastTurnError` above. */
  readonly currentAgentStatus: PiAcpStatus | null = $derived.by(() => {
    const list = this.entries;
    for (let i = list.length - 1; i >= 0; i--) {
      const ev = list[i].event;
      if (ev.type !== 'raw') continue;
      const status = parsePiAcpStatus(ev.value);
      if (status) return status;
    }
    return null;
  });

  /** Drop the in-memory transcript for the foreground session. The two
   *  buffers must stay in lockstep — clearing only one leaves a half-
   *  rendered chat. */
  clear() {
    this.entries = [];
    this.groups = [];
  }

  /** Replace the transcript wholesale (snapshot load) and re-fold groups
   *  end-to-end. Frame appends go through `append` instead. */
  replace(entries: JournalEntry[]) {
    this.entries = entries;
    this.rebuild();
  }

  /** Append one live frame: push the entry and fold it into the groups. */
  append(e: JournalEntry) {
    this.entries.push(e);
    this.appendToGroups(e);
  }

  /** Merge one journal entry into `this.groups`. Streaming text/tool frames
   *  extend the trailing group when its kind matches; everything else pushes
   *  a fresh group. */
  private appendToGroups(e: JournalEntry) {
    const list = this.groups;
    const last = list[list.length - 1];
    const ev = e.event;
    switch (ev.type) {
      case 'user_prompt':
        list.push({ kind: 'user', text: ev.text, key: `u${e.seq}`, ts_ms: e.ts_ms });
        return;
      case 'note':
        list.push({
          kind: 'note',
          text: ev.text,
          sourceSession: ev.source_session,
          key: `n${e.seq}`,
          ts_ms: e.ts_ms,
        });
        return;
      case 'assistant_text':
        // Mutate the proxied field in place; spread-replacing the slot would
        // re-allocate the whole group object per streamed chunk.
        if (last?.kind === 'assistant') last.text += ev.text;
        else list.push({ kind: 'assistant', text: ev.text, key: `a${e.seq}`, ts_ms: e.ts_ms });
        return;
      case 'assistant_thought':
        if (last?.kind === 'thought') last.text += ev.text;
        else list.push({ kind: 'thought', text: ev.text, key: `h${e.seq}`, ts_ms: e.ts_ms });
        return;
      case 'tool_use': {
        // Group by tool family so a Read/Grep/Glob/LS run collapses into one
        // "Explored N files" affordance; for 'other' tools we still only merge
        // identical adjacent names so we don't conflate Edit and Write.
        // Push through the proxy — spreading `last.calls` would balloon to
        // O(N²) on tool-heavy turns by copying the whole array per call.
        const family = classifyFamily(ev.name);
        const call: ToolCall = { name: ev.name, input: ev.input };
        if (last?.kind === 'tools' && last.family === family) {
          const sameName = last.calls[last.calls.length - 1]?.name === ev.name;
          if (family !== 'other' || sameName) {
            last.calls.push(call);
            return;
          }
        }
        list.push({
          kind: 'tools',
          family,
          calls: [call],
          key: `t${e.seq}`,
          ts_ms: e.ts_ms,
        });
        return;
      }
      case 'system':
        list.push({ kind: 'system', subtype: ev.subtype, key: `s${e.seq}`, ts_ms: e.ts_ms });
        return;
      case 'usage':
        return;
      case 'result':
        if (ev.is_error) {
          list.push({
            kind: 'error',
            stopReason: ev.stop_reason,
            key: `r${e.seq}`,
            ts_ms: e.ts_ms,
          });
        }
        return;
      case 'raw':
        // pi-acp status pings are header-spinner fuel; never go into the
        // visible transcript. Match the pre-refactor MessageGroups filter.
        if (isPiAcpStatusEvent(ev.value)) return;
        list.push({ kind: 'raw', value: ev.value, key: `x${e.seq}`, ts_ms: e.ts_ms });
        return;
    }
  }

  /** Rebuild `groups` from `entries` end-to-end. Called whenever entries
   *  are replaced wholesale (snapshot load) — frame appends go through
   *  `appendToGroups` instead. */
  private rebuild() {
    this.groups = [];
    for (const e of this.entries) this.appendToGroups(e);
  }
}
