# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Working principles

Non-negotiable for any change in this repo. Bias toward caution over speed; for trivial tasks, use judgment.

- **Think before coding.** Surface assumptions explicitly. If multiple interpretations exist, present them — don't pick silently. If a simpler approach exists, say so. If something is unclear, stop and ask.
- **Simplicity first.** Minimum code that solves the problem. No features, abstractions, flexibility, or error handling that wasn't asked for. No hacks or "for now" workarounds — when two designs exist, choose the idiomatic one (e.g. a library's intended API) over a clever shim.
- **Surgical changes, except when a real refactor is the task.** By default touch only what the task requires; don't improve adjacent code, refactor unbroken things, or rework formatting. Match existing style. Clean up imports/variables your changes orphaned; leave pre-existing dead code alone. **Exception:** when a clean change *requires* reshaping existing code (renaming, dropping a trait param, replacing an abstraction), do the real refactor rather than bolting a compatibility shim on top. Surgical ≠ avoiding the necessary refactor; surgical = not expanding scope beyond what the task needs.
- **Fix root causes, never symptoms.** Trace breakage to the underlying cause and fix that. Prefer the fix that makes the symptom impossible, not merely invisible.
- **No overengineering.** When an audit or review surfaces many findings, filter to the ones with real impact (durability loss, lost panics, invisible IO errors) and skip paranoia-tier additions (logs for impossible cases, defensive instrumentation that doesn't change outcomes). "Clean and simple" beats "exhaustive".
- **Goal-driven execution.** Translate tasks into verifiable goals before coding: "add validation" → "write tests for invalid inputs, then make them pass"; "fix the bug" → "write a test that reproduces it, then make it pass". For multi-step work, state a brief plan with a verification check per step.

## What this is

A Cargo workspace with eight crates:

- **`crates/roy`** — core library. Owns sessions: spawns ACP harness binaries over stdio, journals each turn, broadcasts events to N subscribers, persists boot-kit metadata in SQLite (`~/.local/state/roy/sessions.db`) so sessions survive across daemon restarts.
- **`crates/roy-cli`** — binary `roy`. Thin trigger over the daemon (Unix socket). The `roy mcp`, `roy gateway`, `roy scheduler`, `roy management`, and `roy inbound` subcommands delegate to the matching adapter crates, so a single binary covers every adapter.
- **`crates/roy-mcp`** — MCP (Model Context Protocol) server library. Two subcommands:
  - `roy mcp serve` (also bare `roy mcp`) — daemon-control MCP server. JSON-RPC 2.0 over stdio; exposes session operations as MCP tools.
  - `roy mcp serve-connections` — proxying MCP server. Reads a `Bundle` (`{session_id, connections: [ConnectionSpec...]}`) from `--specs <path>` or stdin, spawns each upstream stdio MCP as a child, aggregates `tools/list` with `<slug>__<tool>` namespacing, and proxies `tools/call`. Spawned by the daemon as a child of the ACP agent (claude harness only — others reject non-empty connections) via the project-level `.mcp.json` written into the session cwd at spawn time.
  - **Connections scope:** stdio upstream only; claude + opencode + gemini + codex harnesses; pi is unsupported by design (per its README); secrets stored in plaintext JSON in the `0600` DB file; tools snapshot at spawn; resume gets a clean MCP slate.
- **`crates/roy-management`** — axum HTTP service for projects, session metadata, agent personas, and connections. Exposes `pub async fn run(args)`; `roy-cli` dispatches `roy management` to it. Owns `MetaStore` (`projects`, `session_meta`, `session_tags`, `connections`) on top of the shared `agents.db` SQLite file (`~/.local/state/roy/agents.db`, override with `ROY_AGENTS_DB`). Talks to the daemon over Unix socket via `DaemonClient` for session-coordination operations; routes project/tag/agent/connection operations directly to the database. Agent personas live as files in `.roy/agents/<slug>.md` (YAML frontmatter: `name`, `description`, `harness`, optional `model`; body becomes the session's system prompt). `POST /agents/_builder` spawns a session backed by an agent-builder persona that edits target agents via `roy agents update` CLI calls.
- **`crates/roy-auth`** — `users`, `teams`, `team_members`, `team_invites` tables in the same `agents.db` SQLite file, JWT cookie auth, ACL helpers. Consumers: `roy-management` (HTTP middleware + handlers) and `roy-gateway` (WS subprotocol verification). Exposes `UserStore`, `TeamStore`, `InviteStore`, `sign_session`/`verify_session`, `verify_cookie`/`verify_ws_protocol`, `Acl`. Test helpers under `pub mod test_support` (feature `test-support`).
- **`crates/roy-scheduler`** — cron + one-shot fire dispatcher. Talks to the daemon over its Unix socket using `ClientCommand::Fire`; never reaches into `SessionManager`, `Engine`, or `Journal`. Owns its own SQLite state (`~/.local/state/roy-scheduler/state.db`): `agents`, `triggers`, `fires`, `fire_subscribers`, `fire_subscriber_runs`. Exposes `pub async fn cli::run(cli)` so `roy-cli` dispatches `roy scheduler` to the same code as the standalone binary. A Postgres dialect of the same schema is maintained in `migrations/postgres/` but not wired into the binary.
- **`crates/roy-gateway`** — Telegram bots + WebSocket relay bridge to the daemon. Exposes `pub async fn run(args)`; `roy-cli` dispatches `roy gateway` to it. Same boundary rule as `roy-scheduler`. Telegram bots are sourced from the `connections` table at startup (N concurrent `teloxide` tasks); `SessionBinder` keys `(connection_id, external_id) → session_id`. WS relay is a transparent bridge: each WS connection opens a dedicated Unix-socket connection to the daemon and pumps `Message::Text` ↔ `\n`-delimited JSON verbatim.
- **`crates/roy-inbound`** — inbound event bus for external systems (HTTP webhook today; IMAP / WhatsApp / Telegram-customer-support are roadmap). Pure publishers normalize external events into `InboundEvent`s onto an in-process `tokio::mpsc` bus; a single dispatcher resolves a per-source session strategy (`ephemeral`/`persistent_one`/`per_sender_sticky`), fires the agent over the daemon Unix socket, and a per-channel `ReplyHook` delivers the result back. Owns SQLite state at `~/.local/state/roy-inbound/state.db` (table `bindings`). Configured via TOML (`~/.config/roy/inbound.toml`).

External crates (`roy-mcp`, `roy-scheduler`, `roy-gateway`, `roy-management`, `roy-inbound`) depend on `roy` only for the wire-protocol types (`ClientCommand`, `ServerEvent`, `FireTarget`, `TurnEvent`, `ErrorCode`, `StopReason`) and the `PidLock` utility. No direct calls into `SessionManager`, `SessionEngine`, `Journal`, or `Transport` are allowed — the Unix socket is the only API. `roy-auth` is a sibling library used by `roy-management` and `roy-gateway` for user/team storage and JWT verification; it does not depend on `roy`.

Roy spawns harness binaries; it does not install them. The agent's working directory comes from the client: `roy run --cwd …`, MCP `cwd` argument, or `ClientCommand::Spawn.cwd`. When no client supplies one, the daemon falls back to `ROY_CWD` (env), then its own `current_dir`. Set `ROY_CWD` on the systemd/launchd unit to pin a default project root for every default-cwd session.

### Per-scope cwd layout

For multi-user setups, `roy-management` resolves session cwd from `(user_id, scope, optional team_id, optional project_id, session_id)` under `$ROY_WORKSPACE_DIR` (default `~/.roy/workspace`):

```
$ROY_WORKSPACE_DIR/
├── users/<user_id>/sessions/<session_id>/
├── users/<user_id>/projects/<project_id>/sessions/<session_id>/
├── teams/<team_id>/sessions/<session_id>/
└── teams/<team_id>/projects/<project_id>/sessions/<session_id>/
```

`roy-management` only `mkdir`s the cwd — no auto-generated `CLAUDE.md` or `.memory/`. If the user wants per-scope agent context, they place `CLAUDE.md` themselves in `users/<user_id>/` or `teams/<team_id>/`; the ACP agent walks up to find it.

The daemon stays trusted: it accepts `ClientCommand::Spawn { cwd, ... }` from the Unix socket without knowing about users. The HTTP layer is the only auth boundary.

### Session-to-session collaboration

Two patterns sit on top of existing primitives, no new wire variants:

- **Agent asks human.** A background agent runs
  `roy inject <human_session> "<question>" --source $ROY_SESSION_ID`.
  The daemon sets `ROY_SESSION_ID` on every spawned ACP child
  (`transport/acp/mod.rs` `AcpTransport::open`), so the agent passes
  its own session id without the orchestrator templating it in. The
  human's roy-web renders the `Note` with a clickable link back to the
  asker's session (`MessageGroups.svelte`); the human navigates there
  and types a reply, which goes to the agent as a normal `Cmd::Prompt`.
- **Agent asks agent.** A background agent runs
  `roy ask <target> "<prompt>" [--context "..."] [--timeout 10m]`.
  `<target>` resolves to a live roy session id (→ `Fire { Resume }`) or
  an agent slug/id from roy-management (→ `Fire { Spawn { harness,
  system_prompt: agent.prompt } }`). The CLI blocks on `Fire`, prints
  `{"type":"answer","session":..,"text":..}` on `FireDone`, and exits
  0 / 1 / 2 just like `roy fire`.

Both flows are sync from the agent's perspective. Neither introduces a
pending-question store, a new `TurnEvent`, or a new `ClientCommand`.

### Harnesses

Each harness maps to a specific binary that must be on `PATH` and pre-authenticated:

| Harness    | Binary             | Notes                                                                                       |
|------------|--------------------|---------------------------------------------------------------------------------------------|
| `claude`   | `claude-code-acp`  | ACP adapter for Claude Code (not the plain `claude` CLI)                                    |
| `gemini`   | `gemini`           | Launched with `--acp --skip-trust`; uses `yolo` mode                                        |
| `opencode` | `opencode`         | Launched with `acp`; no ACP modes                                                           |
| `codex`    | `codex-acp`        | ACP adapter for Codex; uses `full-access` mode                                              |
| `pi`       | `pi-acp`           | ACP adapter for `pi` coding agent (spawns `pi --mode rpc` under the hood); `npm i -g pi-acp` |

Which harnesses and models are *surfaced* to clients is controlled by
`~/.config/roy/harnesses.toml` (see `docs/harnesses-config.md`). The
harness binaries above must still be installed and authenticated.

> **Terminology:** *harness* = the ACP-adapter binary (one of the five
> above). *Agent* = a persona defined in `.roy/agents/<slug>.md` with
> YAML frontmatter (`name`, `description`, `harness`, optional `model`)
> and a body that becomes the session's system prompt.

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

Four tests hit real harness binaries and are `#[ignore]`d. They self-skip if the dependency is absent, so running them without setup is a no-op pass:

```bash
cargo test --test acp_transport -- --ignored real_claude     # needs `claude-code-acp` on PATH, logged in
cargo test --test acp_transport -- --ignored real_gemini     # needs `gemini` on PATH, logged in
cargo test --test acp_transport -- --ignored real_opencode   # needs `opencode` on PATH
cargo test --test acp_transport -- --ignored real_codex      # needs `codex-acp` on PATH
```

### Auth (multi-user)

`roy-management` requires `ROY_JWT_SECRET` (≥32 ASCII bytes) at startup; without it, the service fails fast. On first startup with an empty `users` table, a bootstrap user is created with username from `ROY_BOOTSTRAP_USERNAME` (default `root`) and password from `ROY_BOOTSTRAP_PASSWORD` (or a generated 32-char hex value printed to stderr exactly once).

`roy-cli` exposes auth helpers (HTTP-backed, except `auth reset` which talks to the DB):

```bash
roy auth login              # interactive prompt → ~/.config/roy/cookie (mode 0600)
roy auth whoami             # GET /auth/me (reads cookie)
roy auth reset <username>   # direct DB password override (recovery)
```

`roy-gateway`'s WebSocket handshake authenticates via `Sec-WebSocket-Protocol: roy-jwt,<JWT>` — the same JWT cookie issued by `/auth/login`.

## Architecture

A short pipeline. Triggers (CLI, MCP, HTTP, WS, Telegram, scheduler, webhook) talk to a single `Daemon`; `Daemon` owns a `SessionManager`; `SessionManager` owns `SessionEngine` actors; each engine drives one ACP `Transport`. Bytes only cross trait boundaries at `Transport`, so adding a new harness is a new `AcpConfig` constructor + a new `Harness` enum variant, not new session/journal/protocol code.

1. **`Daemon`** (`src/daemon.rs`) — accepts Unix-socket connections only, parses `ClientCommand`s, dispatches to per-command `handle_*` methods, and pumps `ServerEvent`s back. Single-instance guard via `PidLock` (`src/pid_lock.rs`): the lock at `<socket>.pid` is the source of truth; a second `roy serve` on the same socket bails with `daemon already running (pid N)`, but a dead PID is detected and taken over (handles `kill -9`). Optional idle-GC + resume-all on startup via `ServeOpts`. For every `Spawn` and `Resume` the daemon emits an early ack (`Spawning` / `Resuming`) before the slow agent-process startup phase. WebSocket clients are served by `roy-gateway`'s WS relay (JWT-authenticated via `Sec-WebSocket-Protocol`, loopback by default at `127.0.0.1:8787`), which bridges each connection to a dedicated Unix-socket connection to this daemon.

2. **`SessionManager`** (`src/manager.rs`) — in-process registry of live `SessionEngine`s keyed by session id, plus on-disk archive operations: `list_archived`, `open_archive`, `read_journal` (unified live-or-archive read), `resume_all`, `sweep_idle`.

3. **`SessionEngine`** (`src/engine.rs`) — long-lived per-session actor. Pipes the agent's events into a `Journal` (persistent JSONL + in-memory ring) and a `broadcast` channel; gates writes via a single `InputLease`; persists boot-kit metadata to `SessionStore` (SQLite) so a fresh daemon process can resurrect the session via ACP `session/load`. `Cmd::Inject` appends a `TurnEvent::Note` to the journal/broadcast without taking the input lease.

4. **`Transport`** (`src/transport/mod.rs`) — single trait, single impl `AcpTransport` (`src/transport/acp/mod.rs`). Spawns the harness binary as a child, drives the official `agent-client-protocol` SDK, handles `session/new` / `session/load`, optional `set_mode`, and auto-answers `session/request_permission` per `PermissionPolicy`.

5. **Control protocol** (`src/control.rs`) — wire-level enums (`ClientCommand`, `ServerEvent`, typed `ErrorCode`) shared by every trigger. The JSON payload is identical regardless of transport; the daemon itself uses only `\n`-delimited Unix framing. The `Message::Text` framing for WebSocket clients is provided by `roy-gateway`'s WS relay.

6. **`roy-cli`** (`crates/roy-cli/src/main.rs`) — clap subcommands: `serve`, `status`, `run`, `attach`, `resume`, `list`, `list-archived`, `close`, `wait`, `fire`, `inject`, `ask`, `mcp`, `gateway`, `scheduler`, `management`, `inbound`, `harnesses`, `projects`, `set-tags`, `auth`. `status` is a non-side-effecting health probe (exit 0 if the daemon socket accepts a connection, 2 otherwise) — prefer it over `pgrep`-ing the binary in scripts and skills. The adapter subcommands (`mcp`, `gateway`, `scheduler`, `management`, `inbound`) dispatch into the matching crate's `cli::run` / `run` entry point.

### TurnEvent normalization

`TurnEvent` (`src/event.rs`) is the common vocabulary across all harnesses: `System`, `UserPrompt`, `AssistantText`, `AssistantThought`, `ToolUse`, `Usage`, `Result { cost_usd, stop_reason }`, `Note { text, source_session }`, and `Raw(Value)`. **Unknown/unmodeled messages become `Raw` rather than being dropped** — so a new event type from an upgraded SDK surfaces instead of vanishing silently. `UserPrompt` is journaled by the engine before each prompt is sent to the transport — ACP agents don't echo user input, so this is how the user side of the conversation survives across refreshes / late attaches. A turn's stream always terminates with `Result`. Wire format is a single JSON shape (`event_to_json` / `event_from_json`) used by stdout, the JSONL journal, and the control protocol.

### Journal

`Journal::append` is single-writer-in-practice (the engine actor) but `Mutex`-guarded. Each append:
1. Writes one JSONL line to disk and `flush`es;
2. Updates an in-memory `VecDeque` ring of size `mem_capacity`;
3. Bumps `next_seq`.

`replay_from(from)` returns entries with `seq >= from`, reading from the in-memory window first and falling back to disk for older entries. `ArchivedJournal::replay_from` is the disk-only variant used for closed sessions. `parse_entry_line` is the single source of truth for JSONL → `JournalEntry`.

### resume_cursor

The opaque token to resume an agent-side session on the next `Transport::open`. Distinct from the roy host session id, which is a UUID kept stable across restarts. For ACP, the cursor is the agent-issued `sessionId` from `session/new`. After a turn that produces a fresh cursor, the engine persists it into `SessionStore` so `SessionManager::resume` can hand it back to `Transport::open` and route through ACP `session/load`.

### ACP details (`acp/mod.rs`)

We own the child process directly (not `AcpAgent::from_args`) so we can detect mid-turn process exit and emit a terminal `Result { stop_reason: Error }`. A `watch` channel propagates "child died" into `run_session` / `run_turn`, which would otherwise hang on a never-resolved `send_request`. `update_to_event` maps `session/update` variants to `TurnEvent`; everything we don't model goes through `Raw(Value)`. Per-harness setup is centralised in `AcpConfig::{claude, gemini, opencode, codex, pi}`.

### Testing approach

Integration tests avoid real CLIs by faking the agent: `tests/scripts/fake-acp-agent.py` speaks JSON-RPC over stdio and takes flags (`--permission`, `--exit-mid-turn`, `--no-initialize-reply`, `--jsonrpc-error`, etc.) to drive error/timeout/permission paths deterministically. Daemon-level tests (`crates/roy/src/daemon.rs` `#[cfg(test)] mod tests`) drive the Unix-socket path through `tokio::io::duplex`; WS relay tests live in `roy-gateway`. Real-CLI smoke tests (`#[ignore]`d) live in `crates/roy/tests/acp_transport.rs`.

## Reference docs

Deep-dive design notes (read these before reshaping the wire format, persistence layer, or component layering):

- `docs/architecture.md` — full layering and component responsibilities across all eight crates.
- `docs/wire-protocol.md` — the single JSON shape used on stdout, in the JSONL journal, and on every trigger.
- `docs/persistence.md` — every SQLite file roy writes, every table, the two ids (roy host id vs agent `resume_cursor`), resume flow, idle GC.
- `docs/harnesses-config.md` — `~/.config/roy/harnesses.toml` user-facing reference.

Historical iteration notes are deliberately not preserved — `git log` is the authoritative record of how the code got to its current shape.
