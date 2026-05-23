# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Code quality bar

Non-negotiable expectations for any change in this repo:

- **No hacks, no temporary solutions, no tech debt.** Code must be reliable and simple. Don't ship "for now" workarounds or stop-gaps. When two designs exist, choose the idiomatic/intended one (e.g. a library's intended API) over a clever shim.
- **Fix root causes, never symptoms.** When something breaks, trace it to the underlying cause and fix that. Don't patch the surface effect — prefer the fix that makes the symptom impossible, not merely invisible.
- **Real refactors over awkward preservation.** If a clean change requires touching existing code (renaming, dropping a trait param, reshaping an abstraction), do it rather than bolting compatibility shims on top.

## What this is

A Cargo workspace with two crates:

- **`crates/roy`** — library. Owns sessions: spawning ACP agents over stdio, journaling each turn, broadcasting events to N subscribers, and persisting metadata so sessions survive across daemon restarts.
- **`crates/roy-cli`** — binary `roy`. Thin trigger over the daemon (Unix socket / WebSocket) plus an MCP server (`roy mcp`) that exposes the daemon to MCP-aware AI clients.

Roy spawns agent CLIs; it does not install them. Each preset (`claude_agent`, `gemini`, `opencode`, `codex`) maps to a CLI that must be on `PATH` and pre-authenticated.

## Commands

```bash
cargo build --all-targets
cargo fmt                # config in rustfmt.toml (edition 2021, max_width 100)
cargo test --workspace   # unit + integration; uses a python fake ACP agent, no real CLI needed

cargo test --test acp_transport                              # one integration test file
cargo test open_send_streams_until_result -- --nocapture     # single test by name
```

`clippy` is not installed in the toolchain by default (`rustup component add clippy` if needed).

### Real-CLI smoke tests (ignored by default)

Four tests hit real agent binaries and are `#[ignore]`d. They self-skip if the dependency is absent, so running them without setup is a no-op pass:

```bash
cargo test --test acp_transport -- --ignored real_claude_agent   # needs `claude-code-acp` on PATH, logged in
cargo test --test acp_transport -- --ignored real_gemini         # needs `gemini` on PATH, logged in
cargo test --test acp_transport -- --ignored real_opencode       # needs `opencode` on PATH
cargo test --test acp_transport -- --ignored real_codex          # needs `codex-acp` on PATH
```

### Running the demos

Each example drives one agent through a two-turn conversation (requires that agent's CLI installed and authenticated):

```bash
cargo run --example demo_claude_agent
cargo run --example demo_gemini
cargo run --example demo_opencode
cargo run --example demo_codex
cargo run --example engine_two_attach     # SessionEngine + two concurrent attaches against the fake agent
```

## Architecture

A short pipeline. Triggers (CLI, MCP, WebSocket) talk to a single `Daemon`; `Daemon` owns a `SessionManager`; `SessionManager` owns `SessionEngine` actors; each engine drives one ACP `Transport`. Bytes only cross trait boundaries at `Transport`, so adding a new agent is a new `AcpConfig` preset, not new session/journal/protocol code.

1. **`Daemon`** (`src/daemon.rs`) — accepts Unix-socket and WebSocket connections, parses `ClientCommand`s, dispatches to per-command `handle_*` methods, and pumps `ServerEvent`s back. Single-instance guard via `PidLock` (`src/pid_lock.rs`). Optional idle-GC + resume-all on startup via `ServeOpts`.

2. **`SessionManager`** (`src/manager.rs`) — in-process registry of live `SessionEngine`s keyed by session id, plus on-disk archive operations: `list_archived`, `open_archive`, `read_journal` (unified live-or-archive read), `resume_all`, `sweep_idle`.

3. **`SessionEngine`** (`src/engine.rs`) — long-lived per-session actor. Pipes the agent's events into a `Journal` (persistent JSONL + in-memory ring) and a `broadcast` channel; gates writes via a single `InputLease`; persists `SessionMetadata` so a fresh daemon process can resurrect the session via `ACP session/load`.

4. **`Transport`** (`src/transport/mod.rs`) — single trait, single impl `AcpTransport` (`src/transport/acp/mod.rs`). Spawns the agent as a child, sets up the official `agent-client-protocol` SDK, handles `session/new` / `session/load`, optional `set_mode`, and auto-answers `session/request_permission` per `PermissionPolicy`.

5. **Control protocol** (`src/control.rs`) — wire-level enums (`ClientCommand`, `ServerEvent`, typed `ErrorCode`) shared by every trigger. Same JSON payload over either framing (Unix socket: `\n`-delimited; WebSocket: `Message::Text`).

6. **`roy-cli`** (`crates/roy-cli/src/main.rs`) — clap subcommands: `serve`, `run`, `attach`, `resume`, `list`, `list-archived`, `close`, `mcp`. The `mcp` subcommand (`crates/roy-cli/src/mcp.rs`) is an MCP server (JSON-RPC 2.0 over stdio) that exposes six tools (`roy_list_sessions`, `roy_list_archived`, `roy_run`, `roy_run_detached`, `roy_read_session`, `roy_close`).

### TurnEvent normalization

`TurnEvent` (`src/event.rs`) is the common vocabulary across all agents: `System`, `AssistantText`, `ToolUse`, `Result { cost_usd, stop_reason }`, and `Raw(Value)`. **Unknown/unmodeled messages become `Raw` rather than being dropped** — so a new event type from an upgraded SDK surfaces instead of vanishing silently. A turn's stream always terminates with `Result`. Wire format is a single JSON shape (`event_to_json` / `event_from_json`) used by stdout, the JSONL journal, and the control protocol.

### Journal

`Journal::append` is single-writer-in-practice (the engine actor) but `Mutex`-guarded. Each append:
1. Writes one JSONL line to disk and `flush`es;
2. Updates an in-memory `VecDeque` ring of size `mem_capacity`;
3. Bumps `next_seq`.

`replay_from(from)` returns entries with `seq >= from`, reading from the in-memory window first and falling back to disk for older entries. `ArchivedJournal::replay_from` is the disk-only variant used for closed sessions. `parse_entry_line` is the single source of truth for JSONL → `JournalEntry`.

### resume_cursor

The opaque token to resume an agent-side session on the next `Transport::open`. Distinct from the roy host session id, which is a UUID kept stable across restarts. For ACP, the cursor is the agent-issued `sessionId` from `session/new`. After a turn that produces a fresh cursor, the engine persists it into `SessionMetadata` so `SessionManager::resume` can hand it back to `Transport::open` and route through ACP `session/load`.

### ACP details (`acp/mod.rs`)

We own the child process directly (not `AcpAgent::from_args`) so we can detect mid-turn process exit and emit a terminal `Result { stop_reason: Error }`. A `watch` channel propagates "child died" into `run_session` / `run_turn`, which would otherwise hang on a never-resolved `send_request`. `update_to_event` maps `session/update` variants to `TurnEvent`; everything we don't model goes through `Raw(Value)`. Per-agent setup is centralized in `AcpConfig::{gemini, opencode, codex, claude_agent}`.

### Testing approach

Integration tests avoid real CLIs by faking the agent: `tests/scripts/fake-acp-agent.py` speaks JSON-RPC over stdio and takes flags (`--permission`, `--exit-mid-turn`, `--no-initialize-reply`, `--jsonrpc-error`, etc.) to drive error/timeout/permission paths deterministically. Daemon-level tests (`crates/roy/src/daemon.rs` `#[cfg(test)] mod tests`) drive the full Unix-socket and WebSocket paths through `tokio::io::duplex` / real loopback TCP. Real-CLI smoke tests (`#[ignore]`d) live in `crates/roy/tests/acp_transport.rs`.
