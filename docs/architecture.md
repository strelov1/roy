# Architecture

`roy` is a Cargo workspace of eight crates that together expose a single
HTTP/WebSocket/Telegram/MCP/CLI surface over an ACP-driven coding-agent
daemon. The workspace splits work along a single rule: **everything outside
the `roy` core talks to the daemon only through its Unix-socket control
protocol** — no crate reaches into `SessionManager`, `SessionEngine`,
`Journal`, or `Transport` directly.

```
┌─────────────────────────────────────────────────────────────────────────┐
│  roy (core daemon, ~/.roy/daemon.sock)                                  │
│   ┌──────────────────────────────────────────────────────────┐          │
│   │ SessionManager                                            │         │
│   │   ├ SessionEngine { id, journal, broadcast, transport }   │         │
│   │   ├ SessionEngine { … }                                   │         │
│   │   └ …                                                     │         │
│   └──────────────────────────────────────────────────────────┘          │
│        ▲ Unix socket            ▲ Spawned ACP children (stdio)          │
└────────┼────────────────────────┼───────────────────────────────────────┘
         │                        │
   ┌─────┴─────────────────────┐  │
   │ roy-cli (roy)             │  │
   │   run / attach / fire /…  │  │
   │   gateway / scheduler /   │  │
   │   management / mcp        │  │
   └─────────┬─────────────────┘  │
             │                    │
   ┌─────────┴───────┐ ┌──────────┴──────────┐ ┌────────────────┐ ┌──────────────┐
   │ roy-mcp         │ │ roy-management      │ │ roy-scheduler  │ │ roy-gateway  │
   │  serve          │ │  HTTP /auth /sessions│ │  cron + fire   │ │  Telegram +  │
   │  serve-conns    │ │  /agents /projects  │ │  dispatcher    │ │  WS relay    │
   │  (MCP stdio)    │ │  + roy-auth tables  │ │                │ │              │
   └─────────────────┘ └─────────┬───────────┘ └────────┬───────┘ └──────┬───────┘
                                 │                      │                │
                                 └──────────┬───────────┴────────────────┘
                                            │
                                  ┌─────────┴──────────┐
                                  │ roy-inbound        │
                                  │  HTTP webhook +    │
                                  │  per-source bus    │
                                  └────────────────────┘
```

Crates and responsibilities:

| crate                  | role                                                                                                            |
|------------------------|-----------------------------------------------------------------------------------------------------------------|
| `roy`                  | session lifecycle, journal, Unix-socket daemon, ACP transport, `harnesses.toml` parser                          |
| `roy-cli`              | the `roy` binary; thin trigger that dispatches subcommands to local code or to the daemon socket                |
| `roy-mcp`              | two MCP servers: daemon-control (`roy mcp serve`) and stdio connections-proxy (`roy mcp serve-connections`)     |
| `roy-management`       | axum HTTP service for projects, session metadata, agent personas, connections; owns `agents.db` SQLite          |
| `roy-auth`             | `users` / `teams` / `team_invites` tables in the same `agents.db`, JWT signing/verification, ACL helpers        |
| `roy-scheduler`        | cron + one-shot fire dispatcher with pluggable subscribers; owns `roy-scheduler/state.db`                       |
| `roy-gateway`          | Telegram bot tasks + WebSocket relay; persists per-channel binding state in JSON / DB                           |
| `roy-inbound`          | normalized inbound event bus (HTTP webhook today); owns `roy-inbound/state.db`                                  |

Wire formats are documented in [wire-protocol.md](./wire-protocol.md);
on-disk state is documented in [persistence.md](./persistence.md);
the harness catalog file in [harnesses-config.md](./harnesses-config.md).

## Pipeline

```
 roy run / roy attach              CLI trigger
 roy fire / roy ask / roy inject   one-shot trigger
 roy mcp ──── stdio JSON-RPC ────► Claude Desktop / IDE
 roy gateway WS relay  ◄── WS ──── browser / IDE
 roy gateway Telegram  ◄── HTTPS ─ Telegram bots
 roy-management HTTP   ◄── HTTP ── roy-web (SPA) / curl
 roy-scheduler         ── cron ──► roy-scheduler tick
 roy-inbound webhook   ◄── HTTP ── external systems
                  └──────────┬────────────┘
                             │ ClientCommand JSON over Unix socket
                             ▼
                        roy daemon
                             │ ACP JSON-RPC over stdio
                             ▼
                   claude-code-acp / gemini /
                   opencode / codex-acp / pi-acp
```

Bytes only cross trait boundaries at `Transport`. Adding a new harness is
a new `AcpConfig` constructor plus a `Harness` enum variant, not new
session/journal/protocol code.

## `roy` core

### `Transport` — agent-IO boundary

One trait, one impl today.

`Transport::open(session_id, resume_cursor, cwd) -> Box<dyn Handle>` spawns
the agent process and runs the ACP handshake (`initialize` →
`session/new` or `session/load` → optional `session/set_mode`). It returns
a `Handle` with:

- `Handle::send(prompt) -> TurnStream` — fire one prompt, get a `Stream`
  of normalised `TurnEvent`s up to and including the terminal
  `Result { stop_reason }`.
- `Handle::resume_cursor()` — the agent-issued session id, suitable for
  passing back to `Transport::open` on a later run.
- `Handle::close()` — terminate the underlying child.

The only `Transport` impl is `AcpTransport`, which talks to the official
`agent-client-protocol` SDK and owns the child process directly (rather
than going through `AcpAgent::from_args`). The reason: roy needs to detect
mid-turn process death so it can emit a synthetic
`Result { stop_reason: Error }`; the SDK's high-level helper would
otherwise leave `send_request` pending forever on a clean `exit(0)`.

`AcpConfig` constructors centralise harness-specific knobs (command,
args, ACP mode, default permission policy, open-handshake timeout) for
the five supported harnesses: `claude`, `gemini`, `opencode`, `codex`,
`pi`. The mapping is:

| Harness    | Binary             | Mode flags                | System-prompt channel  |
|------------|--------------------|---------------------------|-------------------------|
| `claude`   | `claude-code-acp`  | (none)                    | `_meta.systemPrompt`    |
| `opencode` | `opencode`         | `acp`                     | `_meta.systemPrompt`    |
| `gemini`   | `gemini`           | `--acp --skip-trust` yolo | first journaled `System`|
| `codex`    | `codex-acp`        | full-access               | first journaled `System`|
| `pi`       | `pi-acp`           | (spawns `pi --mode rpc`)  | first journaled `System`|

`ROY_SESSION_ID` is set on every spawned ACP child so an agent can pass
its own session id to peer commands like `roy inject --source` or
`roy ask`.

### `SessionEngine` — the actor that owns a session

Spawned once per session by `SessionManager::spawn`. Owns:

- the `Transport` `Handle`,
- an `Arc<Journal>` (persistent JSONL + in-memory ring),
- a `tokio::sync::broadcast::Sender<JournalEntry>` for live observers,
- a single `InputLease` slot (RAII; only one writer at a time),
- `last_activity` for idle-GC,
- references to `SessionStore` (SQLite boot-kit persistence: harness, cwd,
  model, permission, resume_cursor, system_prompt, created_at, closed_at).

The engine actor reads `Cmd::Prompt` / `Cmd::Close` / `Cmd::Persona` /
`Cmd::Inject` from an mpsc channel. On `Prompt`:

1. Touch `last_activity`.
2. Journal `UserPrompt` immediately (ACP agents don't echo user input, so
   without this entry a refresh or late attach would only see the agent
   side).
3. Call `Handle::send(text)` and stream the result.
4. For each `TurnEvent`: `Journal::append` → `JournalEntry` →
   `broadcast::send`. (No receivers is not an error.)
5. If the handle reports a new `resume_cursor`, persist it back to
   `SessionStore::update_cursor` so a daemon restart can resume the
   session.

`SessionEngine::attach(from_seq)` is the heart of the multi-observer
contract. It is **race-free**:

1. Subscribe to the broadcast first (anything streamed from now on is
   captured).
2. Read `Journal::replay_from(from_seq)` into a `Vec`.
3. Return a `Stream` that yields the journal slice, then the live
   broadcast, deduplicating by seq.
4. On `broadcast::RecvError::Lagged(n)` (a slow subscriber) the wrapper
   re-reads the journal from its last yielded seq + 1 and continues —
   the agent never blocks for an observer.

`SessionEngine::snapshot(from_seq)` is a synchronous read-only view (no
broadcast subscription) used by poll-style clients.

`Cmd::Inject` appends a `TurnEvent::Note` to the journal/broadcast
without taking the input lease, so a sibling session (e.g. a background
agent calling `roy inject`) can drop a message into a session the user
is actively typing in.

### `SessionManager` — the registry

In-process `HashMap<session_id, Arc<SessionEngine>>` plus on-disk
inspection helpers. Owns a `TransportFactory` and a reference to
`SessionStore` (SQLite at `~/.local/state/roy/sessions.db`) so it can
resurrect a session from the boot-kit columns without the trigger having
to remember which harness it was.

Front-door methods:

- `spawn(SessionSpawnConfig, capacity_opts)` — fresh session: build the
  transport via the factory, insert boot-kit row into SessionStore, register
  the engine.
- `resume(session_id, capacity_opts)` — read boot-kit from SessionStore,
  rebuild the transport via the factory, call `SessionEngine::resume`
  (same id, same journal), register.
- inspection: `list` (live), `list_archived` (closed), `get` (live engine
  handle), `open_archive` (read-only journal view), `read_journal`
  (unified live-or-archive read).
- lifecycle: `close`, `resume_all` (used by `roy serve --resume-all`),
  `sweep_idle` (used by `--idle-timeout`).

### `Daemon` — triggers + lifecycle

`Daemon::new(journal_dir, factory)` builds the manager. Front-door:
`Daemon::run_with_opts(ServeOpts)` orchestrates startup:

1. If `resume_all`: scan archives, resurrect each (log per-id failures).
2. If `idle_timeout`: spawn a background ticker that calls
   `SessionManager::sweep_idle` at `max(threshold/4, 50ms)` intervals.
3. Spawn the Unix listener (`run_unix`).
4. Await its exit.

Single-instance is enforced by `PidLock`: a PID file at `<socket>.pid`
written atomically with `O_CREAT | O_EXCL`. A live PID blocks startup; a
dead PID is treated as stale and taken over. `kill -9` leaks the file;
the next start detects the dead PID and recovers.

Per-connection (`serve_connection`):

- One `mpsc::UnboundedSender<ServerEvent>` is the per-connection writer
  channel; a dedicated writer task drains it and serialises events as
  `\n`-delimited JSON bytes on the Unix socket.
- The dispatch loop reads `ClientCommand`s from the inbound side and
  routes them through a shared `handle()` that operates on the
  `EventTx` only.
- Each connection tracks its own subscriptions (one tokio task per
  `Attach`, aborted on `Detach`/`Close` and on connection drop) and its
  own held `InputLease`s (RAII-released when the connection drops).

For every accepted `Spawn` and `Resume` the daemon emits an early ack
(`Spawning` / `Resuming`) before the slow agent-process startup phase, so
clients can render a loading indicator and turn silent hangs into a
visible "started but never finished" state.

### Harnesses discovery layer

`crates/roy/src/harnesses_config.rs` reads
`~/.config/roy/harnesses.toml` on demand and normalises it into wire-facing
`HarnessInfo` / `ModelInfo` structures. The daemon's
`handle_list_harnesses` is a thin wrapper that translates outcomes into
`ServerEvent::HarnessesList` variants (`status: ok | created | invalid`).
No cache, no file watcher — the file is re-read on every `ListHarnesses`.
Bootstrap is atomic (write-to-tmp + rename); the daemon never overwrites
a user's existing `harnesses.toml`. See
[harnesses-config.md](./harnesses-config.md) for the user-facing reference.

### Tests

Hermetic by default. `crates/roy/tests/scripts/fake-acp-agent.py` speaks
JSON-RPC over stdio and takes flags (`--permission`, `--exit-mid-turn`,
`--cancellable`, `--no-initialize-reply`, `--jsonrpc-error`, etc.) to
drive error/timeout/permission paths deterministically. Daemon-level
tests (`crates/roy/src/daemon.rs` `#[cfg(test)] mod tests`) drive the
Unix-socket path through `tokio::io::duplex`. Real-CLI smoke tests
(`crates/roy/tests/acp_transport.rs`) are `#[ignore]`d and self-skip
when the corresponding harness binary isn't installed.

## Triggers / external surfaces

Three wire framings, one JSON payload. Adding a new trigger is a new
framing layer in front of the same `ClientCommand` / `ServerEvent`
shapes.

| Trigger            | Framing                                | Where it lives                              |
|--------------------|----------------------------------------|---------------------------------------------|
| Unix socket        | `\n`-delimited JSON Lines              | `roy::daemon` (in-crate)                    |
| WebSocket          | `tungstenite::Message::Text`           | `roy-gateway::ws` (transparent relay)       |
| MCP (stdio)        | MCP JSON-RPC 2.0 over stdio            | `roy-mcp` (bridge process)                  |

`roy mcp serve` is a separate bridge process spawned by an MCP-aware
client (Claude Desktop, IDE plugin). Each `tools/call` opens a fresh
Unix-socket connection to the daemon and drives one round trip.

The WebSocket relay in `roy-gateway` is a peer to the scheduler and
Telegram adapter: it speaks the control protocol over the Unix socket
like any other external client and re-frames the same JSON as
`Message::Text` for WS peers. The daemon is unaware of WS.

## `roy-management` — HTTP API and metadata

axum HTTP service (`pub async fn run(args)`; dispatched from
`roy-cli` as `roy management`). Listens on `127.0.0.1:8079` by default
and owns:

- the `MetaStore` (projects / session_meta / session_tags / connections)
  on top of the shared `agents.db` SQLite pool;
- routing of project/tag/agent/connection operations directly to the DB;
- session-coordination operations (spawn, resume, close) that proxy to
  the daemon over its Unix socket via a `DaemonClient` trait;
- per-scope cwd resolution under `$ROY_WORKSPACE_DIR` (default
  `~/.roy/workspace`), so multi-user setups land sessions in
  `users/<user_id>/[projects/<project_id>/]sessions/<session_id>/` or
  the `teams/<team_id>/...` mirror;
- file-based agent personas in `.roy/agents/<slug>.md` (YAML
  frontmatter: `name`, `description`, `harness`, optional `model`; body
  becomes the session's system prompt). The `_builder` endpoint spawns
  a session backed by a builder persona that edits agent files through
  `roy agents update` CLI calls.

The daemon stays trusted: it accepts `ClientCommand::Spawn { cwd, ... }`
from the Unix socket without knowing about users. The HTTP layer is the
only auth boundary, fronted by:

### `roy-auth`

`roy-auth` owns `users`, `teams`, `team_members`, and `team_invites` in
the same `agents.db` file (both crates' migrations live side-by-side in
the shared `_sqlx_migrations` table, each migrator runs with
`set_ignore_missing(true)`).

`roy-management` requires `ROY_JWT_SECRET` (≥32 ASCII bytes) at startup
and fails fast otherwise. On first startup with an empty `users` table,
a bootstrap user is created with username from `ROY_BOOTSTRAP_USERNAME`
(default `root`) and password from `ROY_BOOTSTRAP_PASSWORD` (or a
generated 32-char hex value printed to stderr exactly once).

Auth-related CLI:

```bash
roy auth login              # interactive prompt → ~/.config/roy/cookie (mode 0600)
roy auth whoami             # GET /auth/me (reads cookie)
roy auth reset <username>   # direct DB password override (recovery)
```

`roy-gateway`'s WebSocket handshake authenticates via
`Sec-WebSocket-Protocol: roy-jwt,<JWT>` — the same JWT cookie issued
by `/auth/login`.

## `roy-scheduler`

A long-lived process that polls its own SQLite state for due triggers
and dispatches each one as a roy `Fire` over the daemon Unix socket.
The MVP at `crates/roy-scheduler/` imports only wire-protocol types from
`roy::control` — no direct calls into `Daemon`, `SessionManager`,
`Engine`, or `Journal`. State (`~/.local/state/roy-scheduler/state.db`):

```
agents              (id, name, harness, project_id, task, model, persistent, …, notify_session)
triggers            (id, agent_id FK, kind, cron_expr|fire_at, next_fire_at, paused, …)
fires               (id, agent_id FK, trigger_id FK NULL, session_id, status, …)
fire_subscribers    (id, agent_id FK NULL, trigger_id FK NULL, kind, config JSON, enabled, …)
fire_subscriber_runs(id, fire_id FK, subscriber_id FK, status, …)
```

The driver loop:

1. **Claim in transaction.** Select due rows (`paused = 0 AND
   next_fire_at <= now`) and atomically mark them as in-flight.
2. **Fire outside transaction.** Bounded semaphore caps concurrency.
   Each fire sends a `ClientCommand::Fire` over the Unix socket and
   collects the streamed `TurnEvent`s into a `fires` row.
3. **Dispatch subscribers** registered for the (agent, trigger) pair:
   `webhook` (HTTP POST), `notify_native` (macOS `osascript`), and
   `chain_agent` (queue another agent's `Fire`). Results are written to
   `fire_subscriber_runs` for audit.
4. **Reschedule** cron triggers' `next_fire_at`; one-shot triggers are
   deleted.

When an agent's `notify_session` is set, the scheduler appends a
`roy inject <notify_session>` instruction to the fired prompt so the
agent can self-report progress back to a human-watched session.

The same SQLite schema is mirrored in
`crates/roy-scheduler/migrations/postgres/` for future deployments;
the Postgres dialect is maintained in lock-step but not yet wired into
the binary.

## `roy-gateway`

Two adapters in one library, started by `roy gateway`:

### Telegram

Sources its bot list from the `connections` table (`kind = 'telegram'`,
one row per outbound bot). At startup it queries roy-management's
catalog and spawns N concurrent `teloxide` bot tasks — one per row.
Each task maps `(connection_id, telegram_user_id) → roy session_id` via
the `SessionBinder` and resumes (or spawns) the corresponding roy
session per inbound message. The session is spawned with
`system_prompt = agent.prompt` resolved from `connection.agent_id`, so
every Telegram user gets an isolated session backed by the agent's
persona.

The streaming pipeline decomposes a turn into the explicit
`Spawn`/`Resume → AcquireInput → Send → Attach (Frame loop) →
ReleaseInput` sequence, holding one Unix-socket connection per turn so
the input lease stays in the gateway's hands. The HTML formatter edits
one placeholder Telegram message every ~1s as `TurnEvent`s arrive,
splitting overflow at the 4096-character limit; `/cancel` aborts the
streaming task through a `CancelRegistry` keyed by `chat_id`.

### WebSocket relay

The `ws` module accepts authenticated WebSocket connections
(loopback by default at `127.0.0.1:8787`; `Sec-WebSocket-Protocol:
roy-jwt,<JWT>`) and, per connection, opens a dedicated Unix-socket
connection to the daemon. JSON is pumped verbatim in both directions
(WS `Message::Text` ↔ `\n`-delimited line). From the daemon's
perspective every WS client is just another Unix-socket connection.

## `roy-inbound`

A small bus that turns external events into roy `Fire` calls.
Publishers (HTTP webhook today; IMAP / WhatsApp / Telegram support are
planned) normalise events into `InboundEvent`s onto a `tokio::mpsc`
channel. A single `InboundDispatcher` resolves a per-source session
strategy (`ephemeral` / `persistent_one` / `per_sender_sticky`),
asks `SessionResolver` to translate the strategy into a `FireTarget`,
runs the fire over the daemon Unix socket, and hands the outcome to a
per-channel `ReplyHook` carried on the event. State (`bindings`
table in `~/.local/state/roy-inbound/state.db`) preserves sticky
mappings across restarts. Configuration is TOML
(`~/.config/roy/inbound.toml`; see `docs/examples/inbound.example.toml`).

## Connections — user-owned MCP servers

The `connections` table in `agents.db` describes one upstream MCP
server per row, owned per-user. Three CRUD surfaces:

- `roy-management` HTTP — `/connections` (interactive UI).
- YAML provider catalog (`~/.roy/connections.yaml`) — read-only, lets
  the user click "Connect → paste token" instead of typing
  command/args. The catalog entry's id is stored in
  `connections.provider_id` and constrains the row's command/args/env
  to the catalog's shape; rows without a `provider_id` are
  free-form (CLI-only path; the web UI requires a catalog match).
- `connections.secrets_json` holds inline credentials (plain JSON; the
  `agents.db` file is `0600`).

When a session is spawned with attached connections, the daemon writes
a project-level `.mcp.json` into the session cwd and starts
`roy mcp serve-connections` as a child of the ACP agent (claude preset
only — other harnesses reject non-empty connections). The proxy:

1. reads a `Bundle` (`{session_id, connections: [ConnectionSpec...]}`)
   from `--specs <path>` or stdin;
2. spawns each upstream stdio MCP as a child;
3. aggregates `tools/list` with `<slug>__<tool>` namespacing so tool
   names from different upstreams don't collide;
4. proxies `tools/call` to the right upstream.

Connections are snapshotted at spawn time. Resume gives a clean MCP
slate — the snapshot is not re-attached.

## Session-to-session collaboration

Two patterns sit on top of existing primitives, no new wire variants:

- **Agent asks human.** A background agent runs
  `roy inject <human_session> "<question>" --source $ROY_SESSION_ID`.
  The daemon sets `ROY_SESSION_ID` on every spawned ACP child, so the
  agent passes its own session id without the orchestrator templating
  it in. The human's roy-web renders the `Note` with a clickable link
  back to the asker's session.
- **Agent asks agent.** A background agent runs
  `roy ask <target> "<prompt>" [--context "..."] [--timeout 10m]`.
  `<target>` resolves to a live roy session id (→ `Fire { Resume }`)
  or an agent slug from roy-management (→ `Fire { Spawn { harness,
  system_prompt: agent.prompt } }`). The CLI blocks on `Fire`, prints
  `{"type":"answer","session":..,"text":..}` on `FireDone`, and exits
  0 / 1 / 2 just like `roy fire`.

Both flows are sync from the agent's perspective. Neither introduces a
pending-question store, a new `TurnEvent`, or a new `ClientCommand`.

## Stdout/stderr and exit codes

`roy run` and `roy attach` keep **stdout reserved for one JSON object
per line** (the `event_to_json` shape — same as the journal and the
WS/MCP wire). Structured `tracing` logs go to **stderr** via
`init_tracing` (`RUST_LOG` overrides; default is
`roy=info,roy_cli=info,warn`). `roy mcp` enforces the same discipline
because MCP's stdio JSON-RPC framing collides with anything else on
stdout.

Process exit codes from `roy run` / `roy attach`:

- `0` — clean terminal `Result` (non-error stop reason).
- `1` — agent finished with `Result.stop_reason.is_error()`.
- `2` — CLI-level failure (no daemon, bad flag, `ServerEvent::Error`,
  transport hang-up).
