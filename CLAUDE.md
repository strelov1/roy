# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Code quality bar

Non-negotiable expectations for any change in this repo:

- **No hacks, no temporary solutions, no tech debt.** Code must be reliable and simple. Don't ship "for now" workarounds or stop-gaps. When two designs exist, choose the idiomatic/intended one (e.g. a library's intended API) over a clever shim.
- **Fix root causes, never symptoms.** When something breaks, trace it to the underlying cause and fix that. Don't patch the surface effect — prefer the fix that makes the symptom impossible, not merely invisible.
- **Real refactors over awkward preservation.** If a clean change requires touching existing code (renaming, dropping a trait param, reshaping an abstraction), do it rather than bolting compatibility shims on top.

## What this is

`roy` is a library (no `[[bin]]`) that drives coding-agent CLIs (`claude`, `gemini`, `opencode`) as child processes over stdio and exposes each turn as a stream of normalized `TurnEvent`s. It spawns the CLI; it does not install it. The CLI must be on `PATH`.

## Commands

```bash
cargo build --all-targets
cargo fmt                 # config in rustfmt.toml (edition 2021, max_width 100)
cargo test                # unit + integration; uses fake agents, no real CLI needed

cargo test parses_tool_use                    # single test by name
cargo test --test acp_transport               # one integration test file
cargo test send_streams_until_turn_end -- --nocapture
```

`clippy` is not installed in the toolchain by default (`rustup component add clippy` if needed).

### Real-CLI smoke tests (ignored by default)

Three tests hit real binaries and are `#[ignore]`d. They self-skip if the dependency is absent, so running them without setup is a no-op pass:

```bash
cargo test -- --ignored real_claude                       # needs CLAUDE_CODE_OAUTH_TOKEN
cargo test --test acp_transport -- --ignored real_gemini  # needs `gemini` on PATH, logged in
cargo test --test acp_transport -- --ignored real_opencode
```

### Running the demos

Each example drives one agent through a two-turn conversation (requires that agent's CLI installed):

```bash
cargo run --example demo           # claude via PrintTransport
cargo run --example demo_gemini    # gemini via AcpTransport
cargo run --example demo_opencode  # opencode via AcpTransport
```

## Architecture

Three decoupled layers. The key design goal is that the **transport stays agent-agnostic** and new agents drop in without touching session/streaming logic.

1. **`Session`** (`src/session.rs`) — a multi-turn conversation with one agent. Holds an `id`, an opaque `resume_cursor`, and lazily opens a live process on the first `send`. Subsequent `send`s reuse the same process (multi-turn). `resume` / `resume_with_cursor` re-open a prior conversation (e.g. after a host-app restart).

2. **`Transport`** (`src/transport/mod.rs`) — how bytes move to/from the process. `open()` spawns and returns a `Handle`; `Handle::send()` writes one user turn and returns a `TurnStream` (`Pin<Box<dyn Stream<Item = TurnEvent>>>`) that ends after the turn's terminal event. Two implementations:
   - **`PrintTransport`** (`print.rs`) — for claude. Spawns the CLI in `stream-json` mode, reads stdout line-by-line, delegates parsing to a `Provider`.
   - **`AcpTransport`** (`acp/`) — for gemini/opencode. Speaks JSON-RPC 2.0 (the Agent Client Protocol) over stdio.

3. **`Provider`** (`src/provider.rs`) — *only used by `PrintTransport`*. Pure logic, no I/O: one CLI's dialect (executable name, spawn args, how to encode a user message, how to parse a stdout line into a `TurnEvent`, what marks turn-end). `ClaudeProvider` is the only impl. This is the extension point for other line-oriented stream-json CLIs.

### TurnEvent normalization

`TurnEvent` (`src/event.rs`) is the common vocabulary across all dialects: `System`, `AssistantText`, `ToolUse`, `Result { cost_usd, is_error }`, and `Raw(Value)`. **Unknown/unmodeled messages become `Raw` rather than being dropped** — so a new event type from an upgraded CLI surfaces instead of vanishing silently. A turn's stream always terminates with `Result`.

### resume_cursor

The opaque token to resume a session on the next `open`. Its meaning differs per transport, which is why `Session` distinguishes the host `id` from the `resume_cursor`:
- **claude**: the cursor *is* the session id (`--session-id` on new, `--resume <id>` thereafter).
- **ACP**: the cursor is the agent-issued ACP `sessionId` from `session/new`, distinct from the host session id. Use `Session::resume_with_cursor` to restore this case.

### ACP client internals (`acp/client.rs`)

`JsonRpcClient` is a JSON-RPC peer over the child's stdio with a single background reader task that routes every incoming line three ways:
- **responses** to handshake `request()` calls → resolve a per-id `oneshot`;
- **the terminal `session/prompt` result** (matched by `active_prompt_id`) and **`session/update` notifications** → forwarded into the current turn's `mpsc` channel (installed by `begin_prompt`);
- **agent→client requests** (notably `session/request_permission`) → answered automatically per `PermissionPolicy` (`AllowAll` selects `allow`; `Deny` cancels).

If the child's stdout closes mid-turn, the reader emits a terminal `Result { is_error: true }` so the stream still terminates. `acp/protocol.rs` maps ACP `session/update` shapes and `session/prompt` results to `TurnEvent`s (a non-`end_turn`/`max_tokens` stop reason is treated as an error).

Per-agent ACP setup lives in `AcpConfig` (`acp/mod.rs`): `AcpConfig::gemini()` uses `yolo` mode + `AllowAll`; `AcpConfig::opencode()` sends no `set_mode` (OpenCode has no ACP modes) and defaults to `Deny`.

### Testing approach

Integration tests avoid real CLIs by faking the agent process: `tests/scripts/fake-agent.sh` (stream-json, for `PrintTransport`) and `tests/scripts/fake-acp-agent.py` (JSON-RPC, for `AcpTransport`). The Python fake takes flags (`--permission`, `--exit-mid-turn`, `--no-initialize-reply`, `--jsonrpc-error`, etc.) to exercise error/timeout/permission paths deterministically. `acp/client.rs` unit tests drive the client over in-memory `tokio::io::duplex` pipes.
