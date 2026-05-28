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

## JournalEntry — `seq` + `ts_ms` + `event`

Every entry in the journal AND every `Frame` event on the trigger
protocol uses:

```json
{"seq": 7, "ts_ms": 1748000000000, "event": {"type": "assistant_text", "text": "…"}}
```

`seq` is `u64`, monotonically increasing across all turns of a session.
Resumed sessions continue past the last persisted seq.

`ts_ms` is `u64`, wall-clock milliseconds since the Unix epoch, stamped
by `Journal::append` at the moment the entry hits the journal. UIs use
this to render send/receive times. `seq` remains the ordering key —
multiple events inside a streamed turn can land in the same millisecond.

## Control protocol — ClientCommand / ServerEvent

The payload of every command/event on the Unix-socket trigger (and,
indirectly, on the WebSocket relay in `roy-gateway` and in every MCP
tool result body). The JSON shapes are identical across all transports.

### ClientCommand (client → server)

`{"op": "<name>", …}`. Operations:

| op                | fields                                                                                          |
|-------------------|-------------------------------------------------------------------------------------------------|
| `spawn`           | `harness`, optional `cwd`, `model`, `permission`, `resume`, `system_prompt`, `extra_env`        |
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
| `inject`          | `session`, `text`, optional `source_session`                                                    |
| `list_harnesses`  | —                                                                                               |

`permission` is `"allow"` or `"deny"`. `harness` is one of `claude`,
`gemini`, `opencode`, `codex`, `pi` (with the default `TransportFactory`).

`spawn.system_prompt` (also accepted on a `fire` command's `spawn` target) is
an optional inline persona/system prompt. The daemon injects it via ACP
`_meta.systemPrompt = { append }` for harnesses that support it (`claude`,
`opencode`) and as a first journaled `System` turn otherwise (`gemini`,
`codex`, `pi`), and snapshots it into the boot-kit row so it is re-applied
on `resume`.

`spawn.cwd` is an optional working directory for the session. When omitted,
the daemon uses the current working directory or the value of `ROY_CWD` env.
This field replaces the previous `project_id` + implicit workspace routing.

Project and tag operations now route through `roy-management` HTTP API
(default `127.0.0.1:8079`), not the Unix socket. See `roy projects --help`
and `roy set-tags --help` for CLI usage.

`inject` appends a `note` event to a **live** session's journal/broadcast
without taking the input lease (so it lands even while an interactive client
holds it). Reply: `{"kind":"injected","session":"<sid>","seq":N}`. An
unknown/non-live session replies `error` with code `no_session` (resume an
archived one first). Used by the `roy inject` CLI for agent self-reporting.

### ServerEvent (server → client)

`{"kind": "<name>", …}`. Variants:

| kind                | fields                                                                                                  |
|---------------------|---------------------------------------------------------------------------------------------------------|
| `spawned`           | `session`, optional `resume_cursor`                                                                     |
| `spawning`          | `harness` — ack emitted at start of `spawn` before harness process launch                               |
| `attached`          | `session`, `seq_at_attach`                                                                              |
| `frame`             | `session`, `entry` (the `JournalEntry` shape above)                                                     |
| `input_acquired`    | `session`, `acquired: bool`                                                                             |
| `input_released`    | `session`                                                                                               |
| `injected`          | `session`, `seq` — ack to `inject` (the appended `note`'s seq)                                          |
| `detached`          | `session`                                                                                               |
| `closed`            | `session`                                                                                               |
| `listed`            | `sessions: [{id}]`                                                                                      |
| `listed_archived`   | `sessions: [{id}]`                                                                                      |
| `resumed`           | `session`, optional `resume_cursor`                                                                     |
| `resuming`          | `session` — ack emitted at start of `resume` before agent process re-launch                             |
| `journal_read`      | `session`, `entries: [JournalEntry]`, `next_seq`, `has_more: bool`                                       |
| `harnesses_list`    | `harnesses: [HarnessInfo]`, `config_path: string`, `status: HarnessesConfigStatus`                       |
| `error`             | optional `session`, typed `code` (see below), `message`                                                 |

`SessionInfo` shape (used in `listed` / `listed_archived`):

```json
{"id": "<session_id>"}
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

`HarnessInfo` and `ModelInfo` shapes (used in `harnesses_list.harnesses[]`):

```json
{
  "name": "claude",
  "models": [
    {"id": "claude-sonnet-4-6", "label": "Claude Sonnet 4.6", "default": true},
    {"id": "claude-opus-4-7",   "label": "Claude Opus 4.7",   "default": false}
  ]
}
```

`label` is always populated by the daemon (defaults to `id` if the user
omitted it in `harnesses.toml`). `default` is `true` for exactly one
model per harness: the explicitly-marked one, or the first if none was
marked.

`HarnessesConfigStatus` is a tagged union (`{"kind": "<variant>", …}`):

| kind      | extra fields    | meaning                                                   |
|-----------|-----------------|-----------------------------------------------------------|
| `ok`      | —               | File parsed and validated; `harnesses` may still be empty |
| `created` | —               | File was missing; sample was just written                 |
| `invalid` | `reason: string`| Parse or validation failure; `agents` is `[]`          |

See [agents-config.md](./agents-config.md) for the user-facing reference.

### ErrorCode

`error.code` is a stable snake_case string. Known values:

`bad_request`, `spawn_failed`, `no_session`, `attach_failed`,
`archive_read_failed`, `no_lease`, `send_failed`, `close_failed`,
`list_archived_failed`, `resume_failed`, `read_journal_failed`.

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
