# Architecture

`roy` is a small Cargo workspace with one library crate and one binary
crate:

- **`crates/roy`** — the library. Owns sessions: spawning agent CLIs over
  stdio, journaling each turn, broadcasting events to N subscribers, and
  persisting metadata so sessions survive across daemon restarts.
- **`crates/roy-cli`** — the `roy` binary. A thin trigger client over the
  daemon (Unix socket) and an MCP server (`roy mcp`) that exposes the
  daemon to MCP-aware AI clients.

This document describes the layering. The wire formats are documented
separately in [wire-protocol.md](./wire-protocol.md); journal and resume
semantics live in [persistence.md](./persistence.md).

## Pipeline

```
┌──────────────────────────────────────────────────────────┐
│ roy serve   (single-instance daemon, ~/.roy/daemon.sock) │
│  ┌──────────────────────────────────────────────────┐    │
│  │ SessionManager                                    │    │
│  │   ├ SessionEngine { id, journal, broadcast, … } │    │
│  │   ├ SessionEngine { … }                           │    │
│  │   └ …                                             │    │
│  └──────────────────────────────────────────────────┘    │
│   ▲ Unix socket                  ▲ stdio MCP             │
└───┼──────────────────────────────┼──────────────────────┘
    │                              │
 roy run                       LLM via roy mcp
 roy attach
 roy list / list-archived
 roy resume / close
 roy-gateway WS relay ◄── WS client (browser/IDE)
 (127.0.0.1:8787, token-auth)
```

Bytes only cross trait boundaries at `Transport`. Adding a new agent is a
new `AcpConfig` preset, not new session/journal/protocol code.

## Layers

### `Transport` — agent-IO boundary

One trait, one implementation today.

`Transport::open(session_id, resume_cursor, cwd) -> Box<dyn Handle>` spawns
the agent process and runs the ACP handshake (`initialize` →
`session/new` or `session/load` → optional `session/set_mode`). It returns
a `Handle` that exposes:

- `Handle::send(prompt) -> TurnStream` — fire one prompt, get a `Stream`
  of normalised `TurnEvent`s up to and including the terminal
  `Result { stop_reason }`.
- `Handle::resume_cursor()` — the agent-issued session id, suitable for
  passing back to `Transport::open` on a later run.
- `Handle::close()` — terminate the underlying child.

The only `Transport` impl is `AcpTransport`, which talks to the official
`agent-client-protocol` SDK and owns the child process directly (rather
than going through `AcpAgent::from_args`). The reason: roy needs to detect
mid-turn process death so it can emit a synthetic `Result {
stop_reason: Error }`; the SDK's high-level helper would otherwise leave
`send_request` pending forever on a clean `exit(0)`.

`AcpConfig` presets centralise agent-specific knobs (command, args, ACP
mode, default permission policy, open-handshake timeout) for the four
supported agents: `claude`, `gemini`, `opencode`, `codex`.

### `SessionEngine` — the actor that owns a session

Spawned once per session by `SessionManager::spawn`. Owns:

- the `Transport` `Handle`,
- an `Arc<Journal>` (persistent JSONL + in-memory ring),
- a `tokio::sync::broadcast::Sender<JournalEntry>` for live observers,
- a single `InputLease` slot (RAII; only one writer at a time),
- `last_activity` for idle-GC,
- the metadata fields used to persist `SessionMetadata` after every
  cursor change.

The engine's actor task reads `Cmd::Prompt` / `Cmd::Close` from an mpsc
channel. On `Prompt`:
1. Touch `last_activity`.
2. Call `Handle::send(text).await` and stream the result.
3. For each `TurnEvent`: `Journal::append` → `JournalEntry` →
   `broadcast::send`. (No receivers is not an error.)
4. If the handle reports a new `resume_cursor`, persist it back to
   `SessionMetadata` so a daemon restart can resume the session.

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

### `SessionManager` — the registry

In-process `HashMap<session_id, Arc<SessionEngine>>` plus on-disk
inspection helpers. Owns a `TransportFactory` so it can resurrect a
session from on-disk metadata without the trigger having to remember
which agent it was.

Front-door methods:

- `spawn(SessionSpawnConfig, capacity_opts)` — fresh session: build the
  transport via the factory, write initial metadata, register the engine.
- `resume(session_id, capacity_opts)` — read metadata, rebuild the
  transport via the factory, call `SessionEngine::resume` (same id, same
  journal), register.
- `attach`-friendly inspection: `list` (live), `list_archived` (closed),
  `get` (live engine handle), `open_archive` (read-only journal view),
  `read_journal` (unified live-or-archive read).
- Lifecycle: `close`, `resume_all` (used by `roy serve --resume-all`),
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

WebSocket clients connect through the `roy-gateway` WS relay
(`crates/roy-gateway/src/ws.rs`), which accepts token-authenticated WS
connections (shared secret via `Sec-WebSocket-Protocol`, loopback by
default at `127.0.0.1:8787`) and bridges each to a dedicated
Unix-socket connection to the daemon, pumping the same control-protocol
JSON verbatim. From the daemon's perspective every WS client is just
another Unix-socket connection.

### Triggers

Three today, all speaking the same `ClientCommand` / `ServerEvent`
payload — only framing differs.

| Trigger              | Framing                              | Where it lives                              |
|----------------------|---------------------------------------|---------------------------------------------|
| Unix socket          | `\n`-delimited JSON Lines             | `roy::daemon` (in-crate)                    |
| WebSocket            | `tungstenite::Message::Text`           | `roy-gateway::ws` (transparent relay)       |
| MCP (stdio)          | MCP JSON-RPC 2.0 over stdio           | `roy-cli::mcp` (out-of-crate bridge)        |

`roy mcp` is intentionally a separate bridge process spawned by an
MCP-aware client (Claude Desktop, IDE plugin). Each tools/call opens a
fresh Unix-socket connection to the daemon and drives one round trip.

The WebSocket relay in `roy-gateway` is a peer to the scheduler and
Telegram adapter: it speaks the control protocol over the Unix socket
like any other external client, and re-frames the same JSON as
`Message::Text` for WS peers. The daemon is unaware of WS.

### Agents discovery layer

`crates/roy/src/agents_config.rs` is a stateless module that reads
`~/.config/roy/agents.toml` on demand (`load_or_bootstrap`), validates
it, and normalises into wire-facing `AgentInfo` / `ModelInfo`
structures. The daemon's `handle_list_agents` (`daemon.rs`) is a thin
wrapper that translates outcomes into `ServerEvent::AgentsList`
variants (`status: ok | created | invalid`).

No cache, no file watcher — the file is re-read on every `ListAgents`
request. Bootstrap is atomic (write-to-tmp + rename) and concurrent-safe
via per-call UUID-suffixed temp names. The daemon never overwrites a
user's existing `agents.toml`.

This module is the single source of truth for the set of available
agents and models. The CLI (`roy agents list`), the MCP tool
(`roy_list_agents`), and `roy-web`'s `agentsConfig` store all consume
the same wire shape — see [agents-config.md](./agents-config.md) for
the user-facing reference and [wire-protocol.md](./wire-protocol.md) for
the JSON shapes.

### Tests

Hermetic by default. `crates/roy/tests/scripts/fake-acp-agent.py` is a
small Python script that speaks JSON-RPC over stdio and takes flags
(`--permission`, `--exit-mid-turn`, `--cancellable`, `--no-initialize-reply`,
`--jsonrpc-error`, etc.) to drive error/timeout/permission paths
deterministically. The daemon-level tests (`crates/roy/src/daemon.rs`
`#[cfg(test)] mod tests`) drive the Unix-socket path through
`tokio::io::duplex`; WS relay tests live in `roy-gateway`. Real-CLI
smoke tests in `crates/roy/tests/acp_transport.rs` are `#[ignore]`d and
self-skip when the corresponding agent binary isn't installed.
