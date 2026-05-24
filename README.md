# roy

**Agent orchestrator in Rust.** Spawn coding-agent CLIs (Claude Code, Gemini,
OpenCode, Codex) as long-lived sessions, persist every turn as JSONL, attach
multiple observers, and drive everything from a CLI, WebSocket, or MCP-aware
LLM — all through one daemon, one journal, one control protocol.

## What this is

roy started as a Rust library that wraps coding-agent CLIs as a single
`Session::send(prompt) -> Stream<TurnEvent>` API. It now ships as a small
workspace with two crates:

- **`crates/roy`** — the library. `SessionEngine` runs an agent in an actor
  task that pipes every event into a per-session JSONL **journal** and a
  bounded **broadcast** channel; `SessionManager` keeps the registry of live
  sessions; the `Daemon` exposes the registry over Unix-socket and WebSocket
  triggers; the underlying transport speaks ACP via the official
  `agent-client-protocol` SDK.
- **`crates/roy-cli`** — the `roy` binary. Eight subcommands plus an MCP
  server mode. Each subcommand is a thin trigger client over the daemon's
  Unix socket.

```
┌──────────────────────────────────────────────────────────┐
│ roy serve   (single-instance daemon, ~/.roy/daemon.sock) │
│  ┌──────────────────────────────────────────────────┐    │
│  │ SessionManager                                    │    │
│  │   ├ SessionEngine { id, journal, broadcast, … } │    │
│  │   ├ SessionEngine { … }                           │    │
│  │   └ …                                             │    │
│  └──────────────────────────────────────────────────┘    │
│   ▲ Unix socket    ▲ WebSocket    ▲ stdio MCP            │
└───┼────────────────┼───────────────┼─────────────────────┘
    │                │               │
 roy run / fire   WS client       LLM via roy mcp
 roy wait         (browser/IDE)
 roy attach
 roy list / list-archived
 roy resume / close
 roy set-tags
```

Each trigger speaks the same JSON control protocol (`ClientCommand` /
`ServerEvent` enums); only the framing differs. The roy-side normalised
event shape (`event_to_json`) is identical on CLI stdout, in the JSONL
journal, and in WS/MCP frames.

## Build & install

```bash
cargo build --release
# the binary lands at target/release/roy
# put it on $PATH or alias it
```

The agents themselves are NOT bundled. Install whichever ones you intend to
use:

| agent             | how                                                                   |
|-------------------|-----------------------------------------------------------------------|
| `gemini`          | the Google Gemini CLI (`npm i -g @google/gemini-cli`), logged in      |
| `opencode`        | the OpenCode CLI on `$PATH`, logged in                                |
| `codex`           | `npm i -g @zed-industries/codex-acp`                                  |
| `claude`          | `npm i -g @zed-industries/claude-code-acp` + API auth                 |

## Quick start: daemon + CLI

Start the daemon in one terminal:

```bash
roy serve                 # listens on ~/.roy/daemon.sock
# optional knobs:
roy serve --port 7777                       # also expose WebSocket on :7777
roy serve --idle-timeout 600                # auto-close sessions idle > 10 min
roy serve --resume-all                      # resurrect every archived session on startup
roy serve --socket /tmp/roy.sock            # custom socket path
roy serve --journal-dir /var/lib/roy/log    # custom journal location
```

Drive it from another terminal:

```bash
# one-shot: spawn opencode, send a task, stream events, exit on Result.
roy run opencode "explain this repo's architecture"

# fire-and-forget: same as above but exit right after sending; the session
# keeps running on the daemon.
roy run --detach opencode "rewrite the README and open a PR"

# list live + archived sessions.
roy list
roy list-archived

# tail a session's journal (live broadcast).
roy attach <session-id>
roy attach <session-id> --from-seq 42       # replay from this seq onward

# bring a closed session back as a live engine.
roy resume <session-id>

# close a live session.
roy close <session-id>
```

stdout is always one JSON object per line (the `event_to_json` shape; see
`docs/wire-protocol.md`). stderr carries
structured logs from `tracing` — `RUST_LOG=roy=debug roy serve` for verbose
output.

Exit codes: `0` on a clean terminal `Result`, `1` if the agent stopped with
an error stop reason, `2` for CLI-level failures (no daemon, bad flag, etc.).

## Quick start: library

```rust
use std::sync::Arc;
use roy::{
    daemon::DefaultTransportFactory,
    SessionManager, SessionSpawnConfig,
};
use futures_util::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let manager = SessionManager::new(
        "/tmp/roy-journals".into(),
        Arc::new(DefaultTransportFactory),
    );

    let engine = manager
        .spawn(
            SessionSpawnConfig {
                agent: "opencode".into(),
                cwd: std::env::current_dir()?,
                model: None,
                permission: None,
                resume_cursor: None,
            },
            /* broadcast_capacity */ 256,
            /* mem_capacity      */ 1024,
        )
        .await?;

    // N concurrent observers.
    let mut attach = engine.attach(None).await?;

    // Single writer.
    let lease = engine.try_acquire_input().expect("free");
    lease.send("what does this repo do?")?;

    while let Some(entry) = attach.stream.next().await {
        println!("[{}] {:?}", entry.seq, entry.event);
        if matches!(entry.event, roy::TurnEvent::Result { .. }) {
            break;
        }
    }
    Ok(())
}
```

See `crates/roy/examples/engine_two_attach.rs` for a slightly larger demo
(two observers, two turns).

## Quick start: MCP

`roy mcp` is a stdio MCP server. Spawn it from any MCP-aware host (Claude
Desktop config, IDE plugin, etc.):

```json
{
  "mcpServers": {
    "roy": {
      "command": "roy",
      "args": ["mcp"]
    }
  }
}
```

`roy mcp` is a thin bridge — it requires `roy serve` to be running. Tools
exposed:

| tool                    | what                                                            |
|-------------------------|-----------------------------------------------------------------|
| `roy_list_sessions`     | live sessions                                                   |
| `roy_list_archived`     | sessions whose journals exist on disk but aren't live           |
| `roy_run`               | spawn + send + wait for `Result`, return text + stop reason     |
| `roy_run_detached`      | spawn + send, return session id (LLM polls with `roy_read_session`) |
| `roy_read_session`      | paginated journal snapshot (live or archived)                   |
| `roy_close`             | close a live session                                            |
| `roy_set_tags`          | replace the tag map on a live session (pass `{}` to clear all) |
| `roy_wait_for_result`   | long-poll for the next terminal Result on a session             |
| `roy_fire`              | one-shot Spawn-or-Resume + Send + WaitForResult                 |

## Resume + persistence

Every session writes both a JSONL journal (`<session_id>.jsonl`) and a
metadata file (`<session_id>.meta.json`) under the journal dir. After the
daemon restarts:

- `roy list-archived` shows surviving session ids;
- `roy attach <id>` returns a read-only replay of the journal;
- `roy resume <id>` (or `roy serve --resume-all`) brings the session back to
  life. The roy-side journal continues from its last seq; the agent-side
  cursor (ACP `sessionId`) is replayed into `Transport::open`, so agents
  that persist their own session (Gemini, OpenCode, ...) continue where
  they left off.

## Single-instance + auth

`roy serve` holds a PID lock at `<socket>.pid`. A second `roy serve` on the
same socket exits with `protocol error: daemon already running (pid N)`. If
the daemon died unclean (e.g. `kill -9`), the next start detects the dead
PID and takes over.

The WebSocket listener (when enabled via `--port`) currently has **no
auth** — bind only on `127.0.0.1` and trust the local user, or front it
with something that does auth.

## Project layout

```
crates/
  roy/          library: engine, journal, manager, daemon, control, transport
  roy-cli/      binary `roy`: run/attach/list/list-archived/resume/close/serve/mcp
docs/
  superpowers/specs/         design docs for the major iterations
CLAUDE.md       project memory for code-assistant sessions
README.md       this file
```

## Tests

```bash
cargo test --workspace            # ~45 tests; uses hermetic fake agents
cargo test --workspace -- --ignored   # additionally runs smoke tests against the real claude/gemini/opencode/codex CLIs (need them installed + logged in)
```

## License

TBD.
