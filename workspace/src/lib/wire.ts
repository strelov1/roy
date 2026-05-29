// TypeScript mirror of `crates/roy/src/control.rs` and the `TurnEvent` enum
// from `crates/roy/src/event.rs`. The canonical reference is
// `docs/wire-protocol.md` in the roy repo.
//
// Project- and tag-aware operations (list_projects, create_project,
// delete_project, set_tags, Spawn.project_id, listed.project_id, …) live in
// roy-management's HTTP API, not on this wire. See `management-client.ts`.

export type Harness = 'claude' | 'gemini' | 'opencode' | 'codex' | 'pi';

/** Runtime allowlist of harness names. Use for narrowing strings of unknown
 *  origin (e.g. server-returned `Agent.harness`) into the `Harness` union. */
export const KNOWN_HARNESSES: ReadonlySet<Harness> = new Set<Harness>([
  'claude',
  'gemini',
  'opencode',
  'codex',
  'pi',
]);

/// Slash-commands are sourced from `~/.roy/skills/**` (harness-agnostic)
/// + `~/.claude/skills/**` (legacy). roy-web expands the skill body inline
/// into the textarea on pick, so any harness receives the unfolded prompt —
/// returns `true` regardless of the active harness.
export function supportsSlashCommands(_harness: Harness | '' | null | undefined): boolean {
  return true;
}

/// The WebSocket subprotocol marker the roy-gateway expects alongside the
/// JWT. Mirrors `roy_auth::cookie::COOKIE_NAME` (Rust) — rename in both.
export const WS_SUBPROTOCOL_MARKER = 'roy-jwt';

export interface ModelInfo {
  id: string;
  label: string;
  default: boolean;
}

export interface HarnessInfo {
  name: Harness;
  models: ModelInfo[];
}

export type HarnessesConfigStatus =
  | { kind: 'ok' }
  | { kind: 'created' }
  | { kind: 'invalid'; reason: string };

/** Shape returned by `listed` / `listed_archived`. The daemon owns
 *  `session, harness, model, cwd`; `project_id` and `tags` are spliced in by
 *  `AppState.refreshSessions` from management's `GET /sessions` so existing
 *  components keep reading them as if they came over the wire. */
export interface SessionInfo {
  session: string;
  harness: string;
  cwd: string;
  model?: string;
  project_id?: string;
  tags?: Record<string, string>;
}

export type Seq = number;

// ---- TurnEvent (tag: "type") ---------------------------------------------

export type StopReason =
  | 'end_turn'
  | 'max_tokens'
  | 'max_turn_requests'
  | 'refusal'
  | 'cancelled'
  | 'error'
  | (string & {});

export type TurnEvent =
  | { type: 'system'; subtype: string }
  | { type: 'user_prompt'; text: string }
  | { type: 'note'; text: string; source_session: string | null }
  | { type: 'assistant_text'; text: string }
  | { type: 'assistant_thought'; text: string }
  | { type: 'tool_use'; name: string; input: unknown }
  | {
      type: 'usage';
      input_tokens: number | null;
      output_tokens: number | null;
      cost_usd: number | null;
    }
  | {
      type: 'result';
      cost_usd: number | null;
      stop_reason: StopReason;
      is_error: boolean;
    }
  | { type: 'raw'; value: unknown };

export interface JournalEntry {
  seq: Seq;
  /// Wall-clock millis since epoch, stamped by the daemon when the entry hit
  /// the journal. UIs render this as the send/receive time of the message.
  ts_ms: number;
  event: TurnEvent;
}

// ---- ClientCommand (tag: "op") -------------------------------------------

export type ClientCommand =
  | { op: 'attach'; session: string; from_seq?: Seq }
  | { op: 'acquire_input'; session: string }
  | { op: 'send'; session: string; text: string }
  | { op: 'cancel_turn'; session: string }
  | { op: 'set_model'; session: string; model: string }
  | { op: 'release_input'; session: string }
  | { op: 'detach'; session: string }
  | { op: 'close'; session: string }
  | { op: 'delete_archive'; session: string }
  | { op: 'list' }
  | { op: 'list_archived' }
  | { op: 'list_harnesses' }
  | { op: 'resume'; session: string }
  | {
      op: 'read_journal';
      session: string;
      from_seq?: Seq;
      max_entries?: number;
    };

// ---- ServerEvent (tag: "kind") -------------------------------------------

export type ErrorCode =
  | 'bad_request'
  | 'spawn_failed'
  | 'no_session'
  | 'attach_failed'
  | 'archive_read_failed'
  | 'no_lease'
  | 'send_failed'
  | 'close_failed'
  | 'list_archived_failed'
  | 'resume_failed'
  | 'read_journal_failed'
  | 'delete_failed'
  | 'cancel_failed'
  | 'set_model_failed'
  | (string & {});

export type ServerEvent =
  | { kind: 'resuming'; session: string }
  | {
      kind: 'attached';
      session: string;
      seq_at_attach: Seq;
      harness: string;
      model?: string;
    }
  | { kind: 'frame'; session: string; entry: JournalEntry }
  | { kind: 'input_acquired'; session: string; acquired: boolean }
  | { kind: 'input_released'; session: string }
  | { kind: 'detached'; session: string }
  | { kind: 'model_changed'; session: string; model: string }
  | { kind: 'closed'; session: string }
  | { kind: 'deleted'; session: string }
  | { kind: 'listed'; sessions: SessionInfo[] }
  | { kind: 'listed_archived'; sessions: SessionInfo[] }
  | { kind: 'resumed'; session: string; resume_cursor?: string }
  | {
      kind: 'journal_read';
      session: string;
      entries: JournalEntry[];
      next_seq: Seq;
      has_more: boolean;
    }
  | { kind: 'error'; session?: string; code: ErrorCode; message: string }
  | {
      kind: 'harnesses_list';
      harnesses: HarnessInfo[];
      config_path: string;
      status: HarnessesConfigStatus;
    };

export type ServerEventKind = ServerEvent['kind'];
