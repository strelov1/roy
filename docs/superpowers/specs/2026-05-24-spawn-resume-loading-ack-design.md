# Spawn/Resume early ack — loading indicator

Date: 2026-05-24
Scope: `crates/roy` wire protocol + daemon hook

## Problem

`ClientCommand::Spawn` and `ClientCommand::Resume` are slow commands — both end
up spawning a child ACP agent and performing one or two JSON-RPC round-trips
(`initialize` + `session/new` for Spawn; `initialize` + `session/load` for
Resume). Realistic latencies:

- Healthy `claude-code-acp`: ~1–3 s.
- Missing/expired auth: the child can hang for tens of seconds (or forever)
  inside `initialize`.

The current wire contract is "one command → one terminal event". Between the
request and `Spawned`/`Resumed`/`Error` the socket is silent, so a client has
no way to distinguish "the daemon is working" from "the daemon got wedged".
This blocks loading-indicator UX and makes auth hangs invisible.

## Goal

For every accepted `Spawn` and `Resume` command, the daemon emits one
in-progress ack event immediately on entry to the handler, before any I/O. The
ack lets clients render a "spawning…/resuming…" state, and turns silent hangs
into a visible "started but never finished" state.

Out of scope:

- `Fire` (composite command; already has its own terminal events
  `FireDone`/`FireTimeout`/`FireError`; consumers are automation — scheduler
  and gateway — not interactive UIs).
- Phased progress (`initializing` / `session_load` / etc.). One ack on entry
  is the minimum sufficient change; phases would require touching transport
  and ACP layers and don't materially improve the UX over a single ack.
- Surfacing child `stderr` or auth-failure diagnostics. Orthogonal feature; can
  follow if the ack alone is not enough.

## Design

### Wire protocol

Two new variants in `ServerEvent` (`crates/roy/src/control.rs`):

```rust
/// Emitted immediately upon receiving `Spawn`, before the agent process is
/// started. Lets clients render a "spawning…" indicator during the process
/// launch + ACP `initialize` + `session/new` round-trip. The session id is
/// not yet known at this point — clients correlate by request order on
/// their own connection.
Spawning {
    agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_id: Option<String>,
},
/// Emitted immediately upon receiving `Resume`, before the agent process is
/// re-started. Lets clients render a "resuming…" indicator during the
/// process launch + ACP `session/load` round-trip.
Resuming { session: String },
```

Wire form (snake_case `kind` per existing convention):

```json
{"kind":"spawning","agent":"claude","project_id":null}
{"kind":"resuming","session":"d0cc413c-c4dd-46ae-86dd-3dd245b3a40a"}
```

Field choice rationale:

- `Spawning.agent` and `Spawning.project_id` — minimum useful info for a
  client that has multiple in-flight Spawns on the same connection to
  disambiguate. The session id does not exist yet (it is assigned inside
  `manager.spawn`); echoing the full request is redundant since the client
  knows what it sent.
- `Resuming.session` — the session id is known at request time and is the
  natural correlation key. Resume cannot fan out the way Spawn can.

### Contract

For every `Spawn` command the daemon accepts, the order of events on that
connection is:

```
Spawning → (Spawned | Error)
```

For every `Resume`:

```
Resuming → (Resumed | Error)
```

The ack arrives even if the command later fails validation (e.g. missing
project). The client clears its "loading" state on any terminal event.

### Backwards compatibility

`ServerEvent` is externally tagged by `kind`. Clients that ignore unknown
`kind` values (the user's browser-side socket consumer; any client that
matches a fixed allow-list) continue to work unchanged. Clients that
strict-deserialize the enum — currently the in-tree CLI and the MCP tools in
`roy-cli` — are updated in the same PR.

### Daemon changes

In `crates/roy/src/daemon.rs`:

- `handle_spawn` (currently at line 562): first line sends
  `ServerEvent::Spawning { agent: agent.as_str().to_string(),
  project_id: project_id.clone() }`. Existing flow follows unchanged.
- `handle_resume` (currently at line 606): first line sends
  `ServerEvent::Resuming { session: session.clone() }`. Existing flow follows
  unchanged.

`event_tx` is a `broadcast::Sender<ServerEvent>` scoped to the connection;
`send` is non-blocking and we already discard the result (`let _ = …`) for
all other events.

### Tests

Add ack-ordering assertions to the existing daemon tests:

- `resume_brings_back_closed_session` (around daemon.rs:2046): after issuing
  the `Resume` command, assert the next event is `ServerEvent::Resuming`
  before the existing `ServerEvent::Resumed` assertion.
- Equivalent assertion in a spawn test (the daemon-level spawn happy path
  in `daemon.rs`'s `#[cfg(test)] mod tests`) for `ServerEvent::Spawning`.

Both go through `tokio::io::duplex` and assert exact event sequences, so the
new ordering check is idiomatic.

### Documentation

- `docs/wire-protocol.md`: document the two new events and the
  `Spawning → Spawned|Error` / `Resuming → Resumed|Error` ordering.
- This spec stays as the design record.

## Trade-offs

- One extra `broadcast::send` per Spawn/Resume — negligible cost.
- Clients that strict-parse `ServerEvent` and were *not* updated in the same
  PR will fail on first encounter. Mitigated by updating the in-tree CLI/MCP
  in the same change; external consumers (the user's browser client) already
  use lenient `kind` switching.
- Phases (initialize/session_load) are deliberately not modelled. If the
  auth-hang case becomes a recurring debugging cost, follow-up work could
  surface child `stderr` or per-phase events — but that lives below the
  transport boundary and is a larger change.
