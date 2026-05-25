# Wire protocol

roy has **one** JSON wire format. The same shapes appear on:

- CLI stdout (`roy run`, `roy attach`, etc.),
- the per-session JSONL journal (`<journal_dir>/<session_id>.jsonl`),
- frames sent across the Unix-socket trigger,
- text frames on the WebSocket trigger (provided by `roy-gateway`'s WS relay),
- result payloads inside MCP tool responses.

This document is the reference. The Rust types live in
`crates/roy/src/event.rs` and `crates/roy/src/control.rs`; serialisation
is implemented through the single mapping
`event_to_json` / `event_from_json` (for `TurnEvent`) and serde
`#[derive]` for everything else.

## TurnEvent — normalised agent output

```rust
enum TurnEvent {
    System { subtype: String },
    UserPrompt { text: String },
    AssistantText { text: String },
    AssistantThought { text: String },
    ToolUse { name: String, input: serde_json::Value },
    Usage { input_tokens: Option<u64>, output_tokens: Option<u64>, cost_usd: Option<f64> },
    Result { cost_usd: Option<f64>, stop_reason: StopReason },
    Note { text: String, source_session: Option<String> },
    Raw(serde_json::Value),
}
```

Wire form (one object per JSONL line, one object per `entry` in a
control frame):

| variant            | JSON shape                                                                                   |
|--------------------|-----------------------------------------------------------------------------------------------|
| `System`           | `{"type":"system","subtype":"…"}`                                                            |
| `UserPrompt`       | `{"type":"user_prompt","text":"…"}`                                                          |
| `AssistantText`    | `{"type":"assistant_text","text":"…"}`                                                       |
| `AssistantThought` | `{"type":"assistant_thought","text":"…"}`                                                    |
| `ToolUse`          | `{"type":"tool_use","name":"…","input":…}`                                                   |
| `Usage`            | `{"type":"usage","input_tokens":null|123,"output_tokens":null|456,"cost_usd":null|0.01}`     |
| `Result`           | `{"type":"result","cost_usd":null|0.42,"stop_reason":"end_turn","is_error":false}`           |
| `Note`             | `{"type":"note","text":"…","source_session":null|"<session_id>"}`                            |
| `Raw`              | `{"type":"raw","value":…}`                                                                   |

`UserPrompt` is journaled by the engine the moment a `send`/`Cmd::Prompt`
arrives, *before* the prompt is forwarded to the agent. Agents don't
echo user input over ACP, so without this entry a refresh, a late
attach, or a second observer would only see the agent side.

Notes:

- `stop_reason` is a snake_case string. `is_error` is computed
  (`is_error = stop_reason ∉ {end_turn, max_tokens}`); it is written for
  human-readability but the source of truth is `stop_reason`.
- `Note` is a message dropped into the session out-of-band — not produced
  by the agent and not a user turn. It is journaled and broadcast directly
  (no input lease, no transport round-trip), so it lands even while an
  interactive client holds the session's input lease. `source_session`
  (nullable) links back to the session that produced it (e.g. the child
  background-agent fire). Emitted in response to `inject` (see below).
- `Raw` carries any unmapped event payload verbatim. Unknown future
  event types from an upgraded agent SDK surface as `Raw` rather than
  being silently dropped.
- A turn's event stream **always** terminates with `Result`. If the
  transport dies mid-turn, the engine synthesises
  `Result { stop_reason: Error }`.

### StopReason

| wire value             | meaning                                              |
|------------------------|------------------------------------------------------|
| `end_turn`             | clean completion                                     |
| `max_tokens`           | hit the token budget cleanly                         |
| `max_turn_requests`    | hit the agent's per-turn request budget              |
| `refusal`              | agent refused                                        |
| `cancelled`            | client cancelled the turn (`session/cancel`)         |
| `error`                | catch-all transport/agent failure                    |
| `<other string>`       | forward-compat: any other agent-emitted reason       |

## JournalEntry — `seq` + `event`

Every entry in the journal AND every `Frame` event on the trigger
protocol uses:

```json
{"seq": 7, "event": {"type": "assistant_text", "text": "…"}}
```

`seq` is `u64`, monotonically increasing across all turns of a session.
Resumed sessions continue past the last persisted seq.

## Control protocol — ClientCommand / ServerEvent

The payload of every command/event on the Unix-socket trigger (and,
indirectly, on the WebSocket relay in `roy-gateway` and in every MCP
tool result body). The JSON shapes are identical across all transports.

### ClientCommand (client → server)

`{"op": "<name>", …}`. Operations:

| op                | fields                                                                                          |
|-------------------|-------------------------------------------------------------------------------------------------|
| `spawn`           | `agent`, optional `project_id`, `model`, `permission`, `resume`, `system_prompt`                |
| `attach`          | `session`, optional `from_seq`                                                                  |
| `acquire_input`   | `session`                                                                                       |
| `send`            | `session`, `text`                                                                               |
| `release_input`   | `session`                                                                                       |
| `detach`          | `session`                                                                                       |
| `close`           | `session`                                                                                       |
| `list`            | —                                                                                               |
| `list_archived`   | —                                                                                               |
| `resume`          | `session`                                                                                       |
| `read_journal`    | `session`, optional `from_seq`, optional `max_entries`                                          |
| `inject`          | `session`, `text`, optional `source_session`, optional `respond` (default `false`), optional `timeout_ms` |
| `list_projects`   | —                                                                                               |
| `create_project`  | `name`                                                                                          |
| `delete_project`  | `project_id`                                                                                    |
| `list_agents`     | —                                                                                               |

`permission` is `"allow"` or `"deny"`. `agent` is one of `claude`,
`gemini`, `opencode`, `codex` (with the default `TransportFactory`).

`spawn.system_prompt` (also accepted on a `fire` command's `spawn` target) is
an optional inline persona/system prompt. The daemon injects it via ACP
`_meta.systemPrompt = { append }` for presets that support it (`claude`,
`opencode`) and as a first journaled `System` turn otherwise (`gemini`,
`codex`), and snapshots it into `SessionMetadata` so it is re-applied on
`resume`.

`spawn.project_id` is a UUID string referencing an existing project; omit or
set to `null` for an orphan session. When `project_id` is given, the session's
`cwd` is the project directory (`workspace_dir/<name>/`). When absent, the
daemon creates `workspace_dir/<session_id>/` and uses that as `cwd`.

`create_project.name` must match `^[A-Za-z0-9_-]+$`. The daemon derives the
on-disk path as `workspace_dir/name` and creates the directory.

`delete_project` is a cascade: the project registry entry is removed and every
session belonging to that project has its `.jsonl` and `.meta.json` deleted.
The on-disk `workspace_dir/<name>/` directory is **not** removed.

`inject` drops a message into a **live** session (resume an archived one first;
an unknown/non-live session replies `error` with code `no_session`):

- `respond: false` (default) → appends a `note` event referencing
  `source_session`. **No input lease required**, so it lands even while an
  interactive client holds the lease. Reply:
  `{"kind":"injected","session":"<sid>","seq":N}`.
- `respond: true` → delivers `text` as a real user turn the agent answers;
  the daemon waits for any in-flight turn to finish first. Reply: the same
  `fire_done` / `fire_timeout` / `fire_error` events as `fire`.

### ServerEvent (server → client)

`{"kind": "<name>", …}`. Variants:

| kind                | fields                                                                                                  |
|---------------------|---------------------------------------------------------------------------------------------------------|
| `spawned`           | `session`, optional `project_id`, optional `resume_cursor`                                              |
| `spawning`          | `agent`, optional `project_id` — ack emitted at start of `spawn` before agent process launch            |
| `attached`          | `session`, `seq_at_attach`                                                                              |
| `frame`             | `session`, `entry` (the `JournalEntry` shape above)                                                     |
| `input_acquired`    | `session`, `acquired: bool`                                                                             |
| `input_released`    | `session`                                                                                               |
| `injected`          | `session`, `seq` — ack to `inject` with `respond:false` (the appended `note`'s seq)                      |
| `detached`          | `session`                                                                                               |
| `closed`            | `session`                                                                                               |
| `listed`            | `sessions: [{id, project_id}]`                                                                          |
| `listed_archived`   | `sessions: [{id, project_id}]`                                                                          |
| `resumed`           | `session`, optional `resume_cursor`                                                                     |
| `resuming`          | `session` — ack emitted at start of `resume` before agent process re-launch                             |
| `journal_read`      | `session`, `entries: [JournalEntry]`, `next_seq`, `has_more: bool`                                       |
| `projects_listed`   | `projects: [Project]`                                                                                   |
| `project_created`   | `project: Project`                                                                                      |
| `project_deleted`   | `project_id: string`, `deleted_sessions: [string]`                                                      |
| `agents_list`       | `agents: [AgentInfo]`, `config_path: string`, `status: AgentsConfigStatus`                              |
| `error`             | optional `session`, typed `code` (see below), `message`                                                 |

`spawned.project_id` is `null` for an orphan session, a UUID string otherwise.

`SessionInfo` shape (used in `listed` / `listed_archived`):

```json
{"id": "<session_id>", "project_id": "<uuid>" | null}
```

`project_id: null` indicates an orphan session.

`Project` shape (used in `projects_listed` / `project_created`):

```json
{"id": "<uuid>", "name": "<name>", "path": "<absolute_path>", "created_at": 1722345600}
```

`spawned.resume_cursor` is the cursor to pass back to a later `spawn`'s
`resume` field, or to `resume` directly.

For every accepted `spawn` and `resume` command the daemon emits an ack
event before the terminal one: `spawning → (spawned | error)` and
`resuming → (resumed | error)`. The ack lets clients render a loading
indicator during the slow agent-process startup phase and turns silent
hangs (e.g. an unauthenticated `claude-code-acp` blocking inside ACP
`initialize`) into a visible "started but never finished" state. Clients
clear the loading state on any terminal event for that command.

`journal_read.next_seq` is the seq the client should pass to its next
`read_journal` to continue polling.

`AgentInfo` and `ModelInfo` shapes (used in `agents_list.agents[]`):

```json
{
  "preset": "claude",
  "models": [
    {"id": "claude-sonnet-4-6", "label": "Claude Sonnet 4.6", "default": true},
    {"id": "claude-opus-4-7",   "label": "Claude Opus 4.7",   "default": false}
  ]
}
```

`label` is always populated by the daemon (defaults to `id` if the user
omitted it in `agents.toml`). `default` is `true` for exactly one model
per agent: the explicitly-marked one, or the first if none was marked.

`AgentsConfigStatus` is a tagged union (`{"kind": "<variant>", …}`):

| kind      | extra fields    | meaning                                                |
|-----------|-----------------|--------------------------------------------------------|
| `ok`      | —               | File parsed and validated; `agents` may still be empty |
| `created` | —               | File was missing; sample was just written              |
| `invalid` | `reason: string`| Parse or validation failure; `agents` is `[]`          |

See [agents-config.md](./agents-config.md) for the user-facing reference.

### ErrorCode

`error.code` is a stable snake_case string. Known values:

`bad_request`, `spawn_failed`, `no_session`, `attach_failed`,
`archive_read_failed`, `no_lease`, `send_failed`, `close_failed`,
`list_archived_failed`, `resume_failed`, `read_journal_failed`,
`no_project`, `project_exists`, `create_project_failed`,
`delete_project_failed`, `invalid_project_name`.

Forward-compat: any unknown string is preserved in
`ErrorCode::Other(s)` on parsing and round-trips verbatim — an older
client can read a newer server's error without losing information.

## Framing

| transport     | framing                                                                       |
|---------------|-------------------------------------------------------------------------------|
| Unix socket   | one JSON object per line, `\n`-delimited                                      |
| WebSocket     | one JSON object per `tungstenite::Message::Text` frame (via `roy-gateway` relay) |
| Journal file  | one JSON object per line, `\n`-delimited (same as Unix socket)                 |
| CLI stdout    | one JSON object per line, `\n`-delimited                                       |

The daemon speaks only the `\n`-delimited Unix framing. The `Message::Text`
framing for WebSocket clients is provided by the WS relay in `roy-gateway`
(`crates/roy-gateway/src/ws.rs`), which bridges each WS connection to a
dedicated Unix-socket connection to the daemon.

## Versioning

The wire format is pre-1.0; field additions follow serde conventions
(missing fields default to `None`/empty). Breaking changes are noted in
commit messages. The `Other(s)` escape hatch on `ErrorCode` and `Raw`
on `TurnEvent` are intentional — new servers can introduce new codes /
event types without breaking older clients.
