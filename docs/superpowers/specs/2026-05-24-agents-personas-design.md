# Design: agents (personas) via roy-management + inline system prompts

Status: draft for review · 2026-05-24 (revised — supersedes the earlier
file-per-agent-in-core draft)

## Problem

roy's notion of an "agent" is thin: `~/.config/roy/agents.toml` is a
*capability catalog* (which presets to surface, which model labels per preset).
A session carries `cwd`, `model`, `tags`, `resume_cursor` — nothing resembling
a persona. There is no system/initial prompt anywhere and no point in the spawn
path to inject one (`engine.rs` forwards `Cmd::Prompt` verbatim).

We want Claude-Code-subagent-style **agents**: a named, reusable identity with
an initial/system prompt plus its CLI (preset) and model — listable, editable,
and creatable from a UI, backed by a database. Memory is not a separate object:
it falls out of how the agent is launched (a continued persistent session has
memory; a fresh session does not). The personality is constant either way.

## Architecture decision

Agents live in a **new standalone service crate, `roy-management`**, which owns
a SQLite database and exposes an HTTP (axum) API for CRUD. It does **not** reach
into the daemon's internals; like `roy-scheduler` and `roy-gateway` it depends
on `roy` only for wire types and talks to the daemon over the Unix socket.

The pivotal mechanism that keeps this clean: **the persona is passed inline in
the spawn command**, not fetched by the daemon. When `roy-management` starts a
session for an agent, it reads the agent from its own DB and puts the agent's
prompt directly into `ClientCommand::Spawn { system_prompt, … }`. The daemon
stays stateless about agents — it receives `preset + model + system_prompt` and
injects. Dependency direction stays correct: `roy-management → roy` (wire
types). The daemon never depends on `roy-management`.

Consequently the `roy` core does **not** store agents, has **no** agent CRUD,
and the previously-proposed `agents.toml → models.toml` / `Spawn.agent →
preset` rename is **dropped**: with personas out of core there is no naming
collision, `Spawn.agent` keeps meaning "preset", and the inline persona field
is simply `system_prompt`. Removing that breaking rename is a deliberate
simplification (YAGNI / no-overengineering).

## Non-goals

- No agent storage, CRUD, or persona files in `roy` core.
- No DB in `roy` core; `SessionMetadata` stays file-based.
- No catalog rename (`agents.toml` and `roy agents`/`ListAgents` stay as-is;
  `roy-management` consumes the existing catalog API to populate preset/model
  pickers).
- MCP-integration session settings are out of scope; metadata is only made
  *forward-compatible* so they can be added later without a migration.
- Building the web UI itself. `roy-management`'s HTTP API is the contract a UI
  consumes; the UI is a separate effort.

## Three parts

1. **roy core** — accept an inline `system_prompt` on spawn, snapshot it into
   `SessionMetadata`, and inject it (ACP `_meta.systemPrompt` where supported,
   first-turn fallback otherwise). Self-contained and shippable: any trigger can
   now pass a system prompt.
2. **roy-management** — new crate: SQLite store of agents, axum CRUD API, and a
   daemon client that spawns sessions with the agent's prompt passed inline.
3. **UI** (later) — consumes the `roy-management` HTTP API.

This decomposes into **two implementation plans**: Plan A (core injection),
Plan B (roy-management). Plan B depends on Plan A.

---

## Part A — roy core: inline system prompt + injection

### A1. Wire protocol (`control.rs`)

Add an optional inline persona to the two spawn entry points:

- `ClientCommand::Spawn` gains `system_prompt: Option<String>`
  (`#[serde(default, skip_serializing_if = "Option::is_none")]`).
- `FireTarget::Spawn` gains `system_prompt: Option<String>` (same attrs), so a
  one-shot `Fire` can also carry a persona.

`Spawn.agent` is unchanged and still means the preset. No other wire renames.

### A2. Injection channel (`transport`)

ACP has no system-prompt field on `prompt`, but `_meta` is the blessed
extension channel and is present on both `NewSessionRequest` and
`LoadSessionRequest` in the Rust SDK (`agent.rs:954`, `:1136`).
`claude-code-acp` reads `_meta.systemPrompt` — `string` replaces, `{ append }`
appends to its `claude_code` preset — and honors it on both `session/new` and
`session/load` (`acp-agent.js:756`, `:179`). So a real system prompt is
reachable; it lives outside conversation history and survives resume.

- `AcpConfig` gains `system_prompt_channel: SystemPromptChannel`, an enum
  `{ Meta, FirstTurn }`. Presets: `claude` and `opencode` → `Meta`; `gemini`
  and `codex` → `FirstTurn` (their native channels are full-replace / unproven
  through ACP, so we don't use them).
- `Transport::open` gains a `system_prompt: Option<String>` parameter.
  - `Meta`: `AcpTransport::open` sets `_meta.systemPrompt = { append: <persona> }`
    on the `NewSessionRequest` / `LoadSessionRequest` it builds. Applied on both
    fresh open and resume, so the persona is re-sent on every `session/load`.
  - `FirstTurn`: `open` ignores the parameter for the protocol message and
    instead stashes it so the engine can inject it as the first turn.
- `Handle` trait gains `fn take_pending_persona(&mut self) -> Option<String>`.
  Returns `Some(text)` only when (a) the channel is `FirstTurn` and (b) this was
  a fresh open (no `resume_cursor`). Returns `None` for `Meta` channels and for
  all resumes. This keeps the channel decision inside the transport (single
  source of truth); the engine just asks whether a deferred persona exists.

### A3. Engine first-turn injection (`engine.rs`)

- `SessionSpawnConfig` gains `system_prompt: Option<String>`. Threaded into
  `Transport::open` in `SessionEngine::start`.
- After `transport.open(...)` returns the handle, `start` calls
  `handle.take_pending_persona()`. If `Some(text)`, it enqueues a new
  `Cmd::Persona(text)` on `input_tx` *before* the actor processes any user
  prompt, so the persona is the first turn.
- New `Cmd::Persona(String)`. In `run_actor`, it behaves like `Cmd::Prompt`
  except it journals `TurnEvent::System { subtype: "persona" }` (a marker, not
  the body) instead of `TurnEvent::UserPrompt`, then drives the turn with the
  persona text. The agent's acknowledgment is journaled normally. (The shared
  body of the `Prompt`/`Persona` arms is factored into a helper to stay DRY.)
- On resume, `take_pending_persona()` returns `None` (the agent reloads history
  that already contains the persona turn), so there is no duplicate injection.

### A4. Metadata snapshot (`session_meta.rs`)

- `SessionMetadata` gains `system_prompt: Option<String>`
  (`#[serde(default, skip_serializing_if = "Option::is_none")]`) — a snapshot of
  the persona body, written at spawn. Resume reads it back and threads it into
  `SessionSpawnConfig.system_prompt` → `Transport::open`, so `Meta`-channel
  agents re-send `_meta` from the snapshot and editing/deleting the source agent
  never mutates or breaks a live session. Personality is fixed at session birth.
- Optional `agent_name: Option<String>` (same serde attrs): a display label the
  spawner may pass for UI/journaling. Defaults to `None`.
- Forward-compatibility note: `SessionMetadata` stays a flat struct; every new
  field uses `#[serde(default)]`, which already makes old files load and new
  files load on old binaries. Future MCP-integration settings are added the same
  way — no generic "extra" bag is introduced now (YAGNI).

### A5. Daemon wiring (`daemon.rs`, `manager.rs`)

- `handle_spawn` reads `system_prompt` from the command and passes it through
  `SessionManager::spawn` → `SessionSpawnConfig.system_prompt`. Same for the
  `Fire` → `FireTarget::Spawn` path.
- `handle_resume` reads `system_prompt` from the on-disk `SessionMetadata` and
  puts it into the `SessionSpawnConfig` it builds, so the persona is re-applied.
- `SessionManager::spawn`/`resume` signatures extend `SessionSpawnConfig`
  construction with the new field (snapshot written via existing
  `write_metadata`).

### A6. CLI / MCP (convenience)

- `roy run` gains `--system-prompt-file <path>` (reads the file into
  `Spawn.system_prompt`). Lets a local user attach a persona without
  roy-management. `--system-prompt <text>` is the inline variant.
- MCP `roy_run` / `roy_run_detached` gain an optional `system_prompt` argument.

### A7. Testing (Part A)

- **Transport (fake agent):** extend `tests/scripts/fake-acp-agent.py` to echo
  the received `_meta` back; assert `_meta.systemPrompt = {append}` arrives on
  `session/new` AND `session/load` for a `Meta` config; assert
  `take_pending_persona()` returns the text once for `FirstTurn` fresh open and
  `None` on resume.
- **Engine:** with a `FirstTurn` transport + persona, assert a single leading
  `System { subtype: "persona" }` journal entry on fresh spawn and none on
  resume; with a `Meta` transport, assert no `System` persona entry (it went via
  `_meta`).
- **Metadata:** spawn with `system_prompt` → assert snapshot in
  `<sid>.meta.json`; resume → assert it is read back and passed to `open`.
- **Wire:** `Spawn` / `FireTarget::Spawn` round-trip with and without
  `system_prompt` (and omitted-field back-compat).
- **Daemon round-trip:** `Spawn { system_prompt }` over the socket; kill +
  `Resume` re-applies from metadata.
- **Real-CLI smoke (`#[ignore]`):** persona "always answer with the word FOO"
  yields a response containing FOO (claude, `Meta` channel).

---

## Part B — roy-management: agent store + HTTP API

### B1. Crate skeleton

- New crate `crates/roy-management`, binary `roy-management`.
- Boundary rule (same as scheduler/gateway): imports from `roy` limited to wire
  types (`ClientCommand`, `ServerEvent`, `FireTarget`, `TurnEvent`, `ErrorCode`)
  and `PidLock`. No `SessionManager`/`Engine`/`Journal` access.
- Own state DB at `~/.local/state/roy-management/state.db` (mirrors
  `roy-scheduler`'s state-dir convention).

### B2. Data model (SQLite)

`agents` table:

| column | type | notes |
|--------|------|-------|
| `id` | TEXT PK | uuid |
| `name` | TEXT NOT NULL | display name |
| `slug` | TEXT UNIQUE NOT NULL | url-safe, derived from name |
| `description` | TEXT | |
| `preset` | TEXT NOT NULL | claude / gemini / opencode / codex |
| `model` | TEXT | model id (validated against the daemon catalog) |
| `prompt` | TEXT NOT NULL | the system/initial prompt |
| `persistent` | INTEGER NOT NULL DEFAULT 0 | launch hint (memory across fires) |
| `created_at` | TEXT NOT NULL | RFC3339 |
| `updated_at` | TEXT NOT NULL | RFC3339 |

Migrations live in the crate (same lightweight approach as roy-scheduler's
SQLite state).

### B3. HTTP API (axum)

- `GET    /agents` — list.
- `POST   /agents` — create `{ name, description, preset, model, prompt, persistent }`.
- `GET    /agents/:id` — fetch one.
- `PUT    /agents/:id` — update.
- `DELETE /agents/:id` — delete.
- `GET    /presets` — proxy the daemon's `ListAgents` (catalog) so the UI can
  populate preset/model pickers from one origin.
- `POST   /agents/:id/run` — resolve the agent, then `Fire` (or `Spawn`) the
  daemon with `system_prompt = agent.prompt`, `agent = agent.preset`,
  `model = agent.model`, `system_prompt`-snapshot inline. Returns the spawned
  session id (and, for Fire, the result).

Validation: on create/update, fetch the daemon catalog and warn (not reject) if
`preset`/`model` is absent — matching core's soft model validation.

### B4. Daemon client

A `roy_client` module mirroring `roy-scheduler/src/roy_client.rs`: newline-
delimited JSON `ClientCommand` → `ServerEvent` over `UnixStream`. Adds a `spawn`
helper that sends `ClientCommand::Spawn { agent, model, system_prompt, … }` and
reads back `Spawned`.

### B5. Testing (Part B)

- **Store:** CRUD round-trips against a temp SQLite; slug uniqueness; timestamps.
- **HTTP:** axum handler tests (in-memory store) for each route incl. 404 and
  validation-warning paths.
- **Daemon client:** against a fake socket server (reuse the duplex pattern from
  daemon tests) assert `Spawn` carries `system_prompt`.
- **Integration:** `POST /agents/:id/run` end-to-end against the real daemon +
  fake ACP agent: create agent → run → assert the session was spawned with the
  persona (observable via the `Meta` echo or the first-turn `System` entry).

---

## Open questions

- `opencode` `_meta.systemPrompt` support is inferred from binary strings, not
  verified live. The `Meta`/`FirstTurn` flag isolates the risk — flip opencode
  to `FirstTurn` with no other change if it turns out unsupported.
- Whether `/agents/:id/run` should default to `Fire` (one-shot) or `Spawn`
  (interactive, returns a session to attach to). Leaning `Spawn` for the UI;
  `Fire` is available for headless callers.
