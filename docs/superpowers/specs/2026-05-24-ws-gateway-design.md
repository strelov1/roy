# WebSocket gateway: extract WS out of the core daemon

Date: 2026-05-24

## Context

Today the daemon (`crates/roy/src/daemon.rs`) runs **two peer frontends** to the
same command handler: a Unix-socket listener and a WebSocket listener. Both parse
`ClientCommand`, dispatch through `dispatch_one_command` → `handle`, and pump
`ServerEvent`s back. WebSocket is therefore *not* a downstream consumer of the
daemon — it is an alternate transport for the exact same wire protocol, welded
into the process that spawns child agent CLIs and owns all session/journal state.

External clients (browser / web) connect over WebSocket. That makes the
network-facing handshake/framing code the most exposed surface in the codebase,
and it currently lives in the most privileged process. `CLAUDE.md` already flags
this: *"the WebSocket listener … is currently unauthenticated — bind to loopback
only or front it with auth."* It also states the boundary rule that external
crates talk to the daemon **only** over the Unix socket — a rule the in-core WS
listener violates by construction.

## Goal

Move WebSocket out of the core daemon into a gateway, mirroring the existing
`roy-gateway` (Telegram). After this change:

- The daemon has exactly one API surface: the Unix socket.
- The most network-exposed code runs in a separate process from the one that
  spawns agents and owns persistent state.
- WebSocket clients see no protocol change — same JSON, same token auth, same
  port. Only the process that terminates the WS connection changes.

## Non-goals

- No change to the wire protocol (`ClientCommand` / `ServerEvent` JSON shape).
- No change to Telegram gateway semantics.
- No TLS termination inside the gateway (front it externally if exposing beyond
  loopback) — same posture as today.
- No multi-tenant auth, no per-client tokens — single shared-secret token, as
  today.

## Architecture: transparent relay

The WS gateway is a **transparent bidirectional relay**, fundamentally simpler
than the Telegram gateway. The protocol is identical on both sides (the same
JSON), so there is nothing to translate:

```
WS client  <--Message::Text-->  ws gateway  <--\n-delimited line-->  daemon (Unix socket)
```

Per WS connection, the gateway:

1. Accepts the TCP/WS upgrade and validates the shared-secret token via the
   `Sec-WebSocket-Protocol` header (auth logic ported verbatim from the daemon).
2. Opens a **dedicated** Unix-socket connection to the daemon.
3. Runs two pump loops concurrently:
   - **inbound**: WS `Message::Text` → trim → write line + `\n` to the daemon
     socket. Ignore binary/ping/pong (tungstenite answers ping/pong itself).
   - **outbound**: daemon socket line → WS `Message::Text`.

Because each WS connection gets its own daemon connection, all per-connection
state (input leases, subscription tasks) lives in the daemon's existing
`serve_connection` path. The daemon cannot tell a gateway connection from a
direct client. This preserves lease/subscription semantics with zero new logic
in the daemon.

### Why a relay, not the Telegram `Conn` abstraction

The Telegram path is built on `TurnConn`/`Conn` (a turn-structured client:
Spawn→AcquireInput→Send→frame-loop→ReleaseInput), plus `orchestrator`, `binder`,
`formatting`, `draft_stream`, `typing`. **The WS relay needs none of that** — it
never interprets a command. Forcing `TurnConn` onto it would be an awkward shim
(disallowed by the `CLAUDE.md` quality bar). The relay opens a raw byte-stream
connection and pumps lines. The only thing it shares with the Telegram path is
(a) dependence on `roy` wire types and (b) the act of dialing the daemon socket.

### Connection lifecycle / cleanup (load-bearing)

Half-close must propagate in both directions so the daemon tears down
subscriptions:

- **WS closed by client** (`Message::Close`, stream end, or error) → drop/close
  the daemon socket. The daemon's `dispatch_lines` loop sees EOF, exits, and
  runs `for handle in subs.into_values() { handle.abort() }`. Without this, a
  disconnected browser would leak subscription tasks in the daemon.
- **Daemon socket EOF** (daemon shut down or hung up) → close the WS with a
  Close frame so the client learns the session is gone.

Implementation: `tokio::select!` over the two pump futures; whichever finishes
first triggers shutdown of the other. Use `into_split()` on the `UnixStream` and
`SplitSink`/`SplitStream` on the WS, as the daemon does today.

## Component layout

Single crate `roy-gateway`, single **config-driven binary**. The binary reads
the TOML config and starts whichever adapters are configured (Telegram, WS, or
both) as concurrent tokio tasks. One adapter per config section.

New / changed files in `crates/roy-gateway/`:

| File | Change |
|------|--------|
| `src/ws.rs` | **new** — the relay: token load/create, WS accept + auth callback, the dual pump loop, listener accept loop. |
| `src/lib.rs` | export `pub mod ws;` |
| `src/config.rs` | `telegram` becomes optional; add optional `websocket` section. |
| `src/main.rs` | config-driven: build the daemon socket path once, then spawn the Telegram dispatcher and/or the WS listener based on which sections are present; `tokio::join!`/`select!` them; error if neither is configured. |
| `Cargo.toml` | add `tokio-tungstenite = "0.24"`, `futures-util = "0.3"`, `http` (for the auth `Response`/`ErrorResponse` types), `uuid` (token mint). |

Removed from `crates/roy/`:

| Item | File |
|------|------|
| `load_or_create_ws_token`, `ws_auth_callback`, `WS_TOKEN_HEADER` | `src/daemon.rs` |
| `run_ws`, `serve_ws_connection`, `dispatch_ws`, `ws_writer_loop` | `src/daemon.rs` |
| `ServeOpts.ws_port` field | `src/daemon.rs` |
| WS branch in `run_with_opts` (token load + `run_ws` spawn + the `join!`) | `src/daemon.rs` |
| WS tests: `spawn_attach_send_round_trip_over_websocket`, `ws_handshake_rejects_missing_or_wrong_token`, the `load_or_create_ws_token` token test | `src/daemon.rs` (logic re-tested in the gateway) |
| `tokio-tungstenite` dep | `Cargo.toml` |
| `--port` flag on `ServeArgs`; `ws_port` wiring; the `roy serve: WebSocket …` eprintlns | `crates/roy-cli/src/main.rs` |

**Stays in `roy`:** `futures-util` — still used by `StreamExt` at the attach pump
(`daemon.rs:1298`, `while let Some(entry) = stream.next().await`). Only the
`SinkExt` part of `use futures_util::{SinkExt, StreamExt};` goes away.
`tokio::net::TcpListener` and `std::net::SocketAddr` imports drop (only WS used
them); `UnixListener` stays.

After removal `run_with_opts` runs only the Unix listener (plus resume-all /
idle-GC), so its `match ws { … }` collapses to awaiting the single Unix task.

> Note on `dispatch_ws`: its only non-shared logic vs `dispatch_lines` is the
> `Message::Text`/`Close` matching. With WS gone from the daemon, `dispatch_ws`
> disappears entirely and `dispatch_lines` stays as the sole dispatcher.

## Configuration

The config is already sectioned (`[daemon]` / `[telegram]` / `[binder]`), so
changes are additive. `[telegram]` becomes optional; `[binder]` moves under the
Telegram concern (it is the chat_id→session store, meaningless for WS).

```toml
[daemon]
socket = "~/.roy/daemon.sock"      # else ROY_SOCKET, else ~/.roy/daemon.sock

[telegram]                         # present → start Telegram adapter
token = "..."
preset = "claude"
project_id = "..."
turn_timeout_secs = 600
allowed_user_ids = [1, 2]

[binder]                           # required iff [telegram] present
path = "~/.local/state/roy-gateway/binder.json"

[websocket]                        # present → start WS relay
bind = "127.0.0.1:8787"            # default; loopback-only unless overridden
token_path = "..."                 # optional; default derived from state dir
```

Validation: at least one of `[telegram]` / `[websocket]` must be present, else
error at startup. If `[telegram]` is present, `[binder]` is required.

### Token

Ported verbatim from the daemon: `<token_path>` is read if present, else a fresh
UUID is minted and written owner-only (`0600`) via the existing
`create_owner_only_file` pattern. Default `token_path` when unset:
`~/.local/state/roy-gateway/ws.token` (sibling to the gateway's other state).
The auth callback semantics (`Sec-WebSocket-Protocol` match, 401 on
missing/invalid, echo the subprotocol back) are preserved 1:1 — browsers can't
set arbitrary headers, so the token rides the subprotocol slot.

`roy` re-exports `pid_lock::{peek_pid, pid_alive, PidLock}` but **not**
`create_owner_only_file`. Rather than widen the `roy` public API for one caller,
the gateway implements a small local owner-only write helper (`O_CREAT | O_EXCL`,
mode `0600`) for minting the token file.

## Error handling

- Bad token → HTTP 401 during upgrade (as today); connection never reaches the
  pump loop.
- Daemon socket unreachable when a WS client connects → the relay fails to dial,
  closes the WS with a Close frame, logs at `warn`. It does not crash the
  listener; the next client retries.
- Malformed JSON from a WS client is **not** the relay's problem — it forwards
  the line verbatim and the daemon answers with its existing
  `ErrorCode::BadRequest` event, which the relay pumps back. The relay stays
  transparent and never parses `ClientCommand`.
- Either pump loop ending tears down the other (see lifecycle above).

## Testing

- **Relay unit/integration test** in `roy-gateway`: stand up a fake daemon
  (a `UnixListener` that echoes / scripts `ServerEvent` lines), connect a real
  `tokio-tungstenite` client through the relay, assert a `ClientCommand` text
  frame arrives on the Unix side and a scripted `ServerEvent` line comes back as
  a WS text frame. Mirrors the spirit of the daemon's old WS round-trip test.
- **Auth test**: connect without / with a wrong subprotocol token → handshake
  rejected (401); correct token → accepted.
- **Cleanup test**: drop the WS client → assert the relay closes its daemon
  socket (observable as EOF on the fake daemon side).
- **Config tests**: `[websocket]`-only, `[telegram]`-only, both, neither
  (error); `[telegram]` without `[binder]` (error).
- Daemon test suite shrinks by the one removed WS round-trip test; the Unix-path
  tests are unaffected.

## Migration / breaking changes

- `roy serve --port N` is removed. Operators who relied on the in-daemon WS
  listener now run the gateway binary with a `[websocket]` config section. This
  is a real behavior change, documented in `docs/architecture.md` and
  `docs/wire-protocol.md`.
- The token file moves from `<socket>.token` (daemon-owned) to the gateway's
  `token_path`. Existing clients must point at the new token; the value can be
  copied over to avoid re-pairing.
- Config: existing Telegram configs keep working unchanged (sections already
  match); only the newly-optional `[telegram]` and the new `[websocket]` are
  added. No flat-format shim is kept.

## Docs to update

- `crates/roy/CLAUDE.md` — remove the "daemon accepts Unix-socket and WebSocket"
  framing and the unauthenticated-WS warning; note WS now lives in the gateway.
- `docs/architecture.md` — daemon is Unix-socket-only; add the WS gateway as a
  peer bridge alongside Telegram and scheduler.
- `docs/wire-protocol.md` — note the WS framing is now provided by the gateway,
  not the daemon, but the JSON shape is unchanged.
