# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Code quality bar

Non-negotiable expectations for any change in this repo:

- **No hacks, no temporary solutions, no tech debt.** Code must be reliable and simple. Don't ship "for now" workarounds or stop-gaps. When two designs exist, choose the idiomatic/intended one (e.g. a library's intended API) over a clever shim.
- **Fix root causes, never symptoms.** When something breaks, trace it to the underlying cause and fix that. Don't patch the surface effect — prefer the fix that makes the symptom impossible, not merely invisible.
- **Real refactors over awkward preservation.** If a clean change requires touching existing code (renaming, dropping a trait param, reshaping an abstraction), do it rather than bolting compatibility shims on top.
- **No overengineering.** Each change must justify its own weight. When an audit or review surfaces many findings, filter to the ones with real impact (durability loss, lost panics, invisible IO errors) and skip paranoia-tier additions (logs for impossible cases, defensive instrumentation that doesn't change outcomes). "Clean and simple" beats "exhaustive".

## What this is

A Cargo workspace with seven crates:

- **`crates/roy`** — library. Owns sessions: spawning ACP agents over stdio, journaling each turn, broadcasting events to N subscribers, and persisting metadata so sessions survive across daemon restarts.
- **`crates/roy-cli`** — binary `roy`. Thin trigger over the daemon (Unix socket). The `roy mcp`, `roy gateway`, `roy scheduler`, and `roy management` subcommands delegate to the matching adapter crates, so a single binary covers every adapter.
- **`crates/roy-mcp`** — library. MCP (Model Context Protocol) server: JSON-RPC 2.0 over stdio, exposes daemon control operations as MCP tools. Linked into `roy-cli` and dispatched via `roy mcp`.
- **`crates/roy-scheduler`** — library + thin binary. Cron + one-shot fire dispatcher. Talks to the daemon over its Unix socket using `ClientCommand::Fire`; never reaches into `SessionManager`, `Engine`, or `Journal`. Owns its own SQLite state (`~/.local/state/roy-scheduler/state.db`) for triggers, fires, and subscribers. Exposes `pub async fn cli::run(cli)` so `roy-cli` can dispatch `roy scheduler` to the same code as the standalone `roy-scheduler` binary.
- **`crates/roy-gateway`** — library. Chat-platform and WebSocket bridge to the daemon (Telegram adapter + WS relay). Exposes `pub async fn run(args)`; `roy-cli` dispatches `roy gateway` to it. Same boundary rule as `roy-scheduler`. Persists `chat_id → roy session_id` in a JSON file so chats survive restarts.
- **`crates/roy-agents`** — library. Canonical agent store: `Agent` type (identity + persona `prompt` + optional scheduled `task`), SQLite CRUD with slug-collision suffixing. Used by `roy-management` today; `roy-scheduler` is planned to migrate onto it later. Shared DB file lives at `~/.local/state/roy/agents.db` (override with `ROY_AGENTS_DB`).
- **`crates/roy-management`** — library. axum HTTP service for agent CRUD and starting sessions. Exposes `pub async fn run(args)`; `roy-cli` dispatches `roy management` to it. Same boundary rule as `roy-scheduler`/`roy-gateway`: talks to the daemon only over the Unix socket, passing `system_prompt = agent.prompt` inline on `Spawn`. Transitional note: `roy-scheduler` still has its own `agents` table until a future Plan C unifies it onto `roy-agents`.

External crates (`roy-mcp`, `roy-scheduler`, `roy-gateway`, `roy-management`) depend on `roy` only for the wire-protocol types (`ClientCommand`, `ServerEvent`, `FireTarget`, `TurnEvent`, `ErrorCode`, `StopReason`) and the `PidLock` utility. No direct calls into `SessionManager`, `SessionEngine`, `Journal`, or `Transport` are allowed — the Unix socket is the only API.

Roy spawns agent CLIs; it does not install them. The agent's working directory comes from the client: `roy run --cwd …`, MCP `cwd` argument, or `ClientCommand::Spawn.cwd`. When no client supplies one, the daemon falls back to `ROY_CWD` (env), then its own `current_dir`. Set `ROY_CWD` on the systemd/launchd unit to pin a default project root for every default-cwd session.

Each preset maps to a specific binary that must be on `PATH` and pre-authenticated:

| Preset | Binary | Notes |
|--------|--------|-------|
| `claude` | `claude-code-acp` | ACP adapter for Claude Code (not the plain `claude` CLI) |
| `gemini` | `gemini` | Launched with `--acp --skip-trust`; uses `yolo` mode |
| `opencode` | `opencode` | Launched with `acp`; no ACP modes |
| `codex` | `codex-acp` | ACP adapter for Codex; uses `full-access` mode |

Which presets and models are *surfaced* to clients is controlled by
`~/.config/roy/agents.toml` (see `docs/agents-config.md`). The four
preset binaries above must still be installed and authenticated.

## Commands

```bash
cargo build --all-targets
cargo fmt                # config in rustfmt.toml (edition 2021, max_width 100)
cargo test --workspace   # unit + integration; uses a python fake ACP agent, no real CLI needed

cargo test --test acp_transport                              # one integration test file
cargo test open_send_streams_until_result -- --nocapture     # single test by name
```

`clippy` is not installed in the toolchain by default (`rustup component add clippy` if needed).

### CI gate

`.github/workflows/ci.yml` runs three commands on every push and PR to `main`/`master`:

```bash
cargo fmt --all -- --check
cargo build --workspace --all-targets
cargo test --workspace --no-fail-fast
```

Run all three locally before pushing — the integration tests spawn `python3 tests/scripts/fake-acp-agent.py`, so a working `python3` on `PATH` is required.

### Stdout/stderr and exit codes

`roy run` and `roy attach` keep **stdout reserved for one JSON object per line** (the `event_to_json` shape — same as the journal and the WS/MCP wire). Structured `tracing` logs go to **stderr** via `init_tracing` (`RUST_LOG` overrides; default is `roy=info,roy_cli=info,warn`). `roy mcp` enforces the same discipline because MCP's stdio JSON-RPC framing collides with anything else on stdout.

Process exit codes from `roy run` / `roy attach`:

- `0` — clean terminal `Result` (non-error stop reason).
- `1` — agent finished with `Result.stop_reason.is_error()`.
- `2` — CLI-level failure (no daemon, bad flag, `ServerEvent::Error`, transport hang-up).

### Real-CLI smoke tests (ignored by default)

Four tests hit real agent binaries and are `#[ignore]`d. They self-skip if the dependency is absent, so running them without setup is a no-op pass:

```bash
cargo test --test acp_transport -- --ignored real_claude   # needs `claude-code-acp` on PATH, logged in
cargo test --test acp_transport -- --ignored real_gemini         # needs `gemini` on PATH, logged in
cargo test --test acp_transport -- --ignored real_opencode       # needs `opencode` on PATH
cargo test --test acp_transport -- --ignored real_codex          # needs `codex-acp` on PATH
```

### Running the demos

Each example drives one agent through a two-turn conversation (requires that agent's CLI installed and authenticated):

```bash
cargo run --example demo_claude
cargo run --example demo_gemini
cargo run --example demo_opencode
cargo run --example demo_codex
cargo run --example engine_two_attach     # SessionEngine + two concurrent attaches against the fake agent
```

## Architecture

A short pipeline. Triggers (CLI, MCP) talk to a single `Daemon`; `Daemon` owns a `SessionManager`; `SessionManager` owns `SessionEngine` actors; each engine drives one ACP `Transport`. Bytes only cross trait boundaries at `Transport`, so adding a new agent is a new `AcpConfig` preset, not new session/journal/protocol code.

1. **`Daemon`** (`src/daemon.rs`) — accepts Unix-socket connections only, parses `ClientCommand`s, dispatches to per-command `handle_*` methods, and pumps `ServerEvent`s back. Single-instance guard via `PidLock` (`src/pid_lock.rs`): the lock at `<socket>.pid` is the source of truth; a second `roy serve` on the same socket bails with `daemon already running (pid N)`, but a dead PID is detected and taken over (handles `kill -9`). Optional idle-GC + resume-all on startup via `ServeOpts`. WebSocket clients are served by `roy-gateway`'s WS relay (token-authenticated via `Sec-WebSocket-Protocol`, loopback by default at `127.0.0.1:8787`), which bridges each connection to a dedicated Unix-socket connection to this daemon.

2. **`SessionManager`** (`src/manager.rs`) — in-process registry of live `SessionEngine`s keyed by session id, plus on-disk archive operations: `list_archived`, `open_archive`, `read_journal` (unified live-or-archive read), `resume_all`, `sweep_idle`.

3. **`SessionEngine`** (`src/engine.rs`) — long-lived per-session actor. Pipes the agent's events into a `Journal` (persistent JSONL + in-memory ring) and a `broadcast` channel; gates writes via a single `InputLease`; persists `SessionMetadata` so a fresh daemon process can resurrect the session via `ACP session/load`.

4. **`Transport`** (`src/transport/mod.rs`) — single trait, single impl `AcpTransport` (`src/transport/acp/mod.rs`). Spawns the agent as a child, sets up the official `agent-client-protocol` SDK, handles `session/new` / `session/load`, optional `set_mode`, and auto-answers `session/request_permission` per `PermissionPolicy`.

5. **Control protocol** (`src/control.rs`) — wire-level enums (`ClientCommand`, `ServerEvent`, typed `ErrorCode`) shared by every trigger. The JSON payload is identical regardless of transport; the daemon itself uses only `\n`-delimited Unix framing. The `Message::Text` framing for WebSocket clients is provided by `roy-gateway`'s WS relay.

6. **`roy-cli`** (`crates/roy-cli/src/main.rs`) — clap subcommands: `serve`, `status`, `run`, `attach`, `resume`, `list`, `list-archived`, `close`, `set-tags`, `wait`, `fire`, `mcp`, `projects`, `engines`, `agents`, `gateway`, `scheduler`, `management`. `status` is a non-side-effecting health probe (exit 0 if the daemon socket accepts a connection, 2 otherwise) — prefer it over `pgrep`-ing the binary in scripts and skills. The `mcp` subcommand delegates to `roy-mcp` (`crates/roy-mcp/src/lib.rs`), an MCP server (JSON-RPC 2.0 over stdio) that exposes daemon control operations as MCP tools.

   - `roy engines` — lists the daemon's preset+model catalog from `agents.toml` (the preset binaries like `claude-code-acp`, `gemini`, etc.).
   - `roy agents` — full CRUD over user-defined personas in `roy-management` (`list`/`get`/`create`/`update`/`delete`/`run`); each agent binds a persona prompt to a preset+model pair and spawns a session on demand.
   - `roy gateway` — Telegram chat-platform and WebSocket relay bridge to the daemon (dispatches to `roy-gateway` crate).
   - `roy scheduler` — cron + one-shot fire dispatcher (dispatches to `roy-scheduler` crate).
   - `roy management` — axum HTTP service for agent CRUD and session launch (dispatches to `roy-management` crate).

   The `POST /agents/_builder` endpoint (proxied from roy-web) spawns a builder session backed by a seeded system agent that gathers requirements via conversation and edits the target via `roy agents update`.

### TurnEvent normalization

`TurnEvent` (`src/event.rs`) is the common vocabulary across all agents: `System`, `UserPrompt`, `AssistantText`, `AssistantThought`, `ToolUse`, `Usage`, `Result { cost_usd, stop_reason }`, and `Raw(Value)`. **Unknown/unmodeled messages become `Raw` rather than being dropped** — so a new event type from an upgraded SDK surfaces instead of vanishing silently. `UserPrompt` is journaled by the engine before each prompt is sent to the transport — ACP agents don't echo user input, so this is how the user side of the conversation survives across refreshes / late attaches. A turn's stream always terminates with `Result`. Wire format is a single JSON shape (`event_to_json` / `event_from_json`) used by stdout, the JSONL journal, and the control protocol.

### Journal

`Journal::append` is single-writer-in-practice (the engine actor) but `Mutex`-guarded. Each append:
1. Writes one JSONL line to disk and `flush`es;
2. Updates an in-memory `VecDeque` ring of size `mem_capacity`;
3. Bumps `next_seq`.

`replay_from(from)` returns entries with `seq >= from`, reading from the in-memory window first and falling back to disk for older entries. `ArchivedJournal::replay_from` is the disk-only variant used for closed sessions. `parse_entry_line` is the single source of truth for JSONL → `JournalEntry`.

### resume_cursor

The opaque token to resume an agent-side session on the next `Transport::open`. Distinct from the roy host session id, which is a UUID kept stable across restarts. For ACP, the cursor is the agent-issued `sessionId` from `session/new`. After a turn that produces a fresh cursor, the engine persists it into `SessionMetadata` so `SessionManager::resume` can hand it back to `Transport::open` and route through ACP `session/load`.

### ACP details (`acp/mod.rs`)

We own the child process directly (not `AcpAgent::from_args`) so we can detect mid-turn process exit and emit a terminal `Result { stop_reason: Error }`. A `watch` channel propagates "child died" into `run_session` / `run_turn`, which would otherwise hang on a never-resolved `send_request`. `update_to_event` maps `session/update` variants to `TurnEvent`; everything we don't model goes through `Raw(Value)`. Per-agent setup is centralized in `AcpConfig::{gemini, opencode, codex, claude}`.

### Testing approach

Integration tests avoid real CLIs by faking the agent: `tests/scripts/fake-acp-agent.py` speaks JSON-RPC over stdio and takes flags (`--permission`, `--exit-mid-turn`, `--no-initialize-reply`, `--jsonrpc-error`, etc.) to drive error/timeout/permission paths deterministically. Daemon-level tests (`crates/roy/src/daemon.rs` `#[cfg(test)] mod tests`) drive the Unix-socket path through `tokio::io::duplex`; WS relay tests live in `roy-gateway`. Real-CLI smoke tests (`#[ignore]`d) live in `crates/roy/tests/acp_transport.rs`.

## Reference docs

Deep-dive design notes (read these before reshaping the wire format, persistence layer, or component layering):

- `docs/architecture.md` — full layering and component responsibilities.
- `docs/wire-protocol.md` — the single JSON shape used on stdout, in the JSONL journal, and on every trigger.
- `docs/persistence.md` — journal + metadata files, the two ids (roy host id vs agent `resume_cursor`), resume flow, idle GC.

Historical iteration notes are deliberately not preserved — `git log` is the authoritative record of how the code got to its current shape.
