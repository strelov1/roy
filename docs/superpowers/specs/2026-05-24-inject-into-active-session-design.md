# Inject a message into an active session

## Problem

A background agent's `inject_parent` subscriber cannot deliver its result into a
session the user is actively viewing in roy-web.

Root cause, confirmed end-to-end:

- roy-web, for the session you have open, calls `acquire_input` and **holds the
  exclusive `InputLease`** for the whole time the session is focused (the
  textarea is gated on `inputAcquired`). Background attaches to other sessions
  are read-only — no lease.
- `inject_parent` delivers via `FireTarget::Resume`, which in the daemon enters
  `handle_fire` and tries to take that same exclusive lease
  (`daemon.rs:840`). It makes a **single** `try_acquire_input()` attempt; since
  roy-web holds the lease, it returns `None` and the daemon immediately replies
  `FireError { code: NoLease, message: "session busy" }`.
- The agent fire itself still succeeds (`fires.status = ok`), but the subscriber
  delivery fails, so nothing lands in the parent session's journal. Verified: the
  parent session's `*.jsonl` contains zero injected `[prefix]…` user turns.

There is also a stale doc/behavior mismatch: the `inject_parent.rs` header claims
"Live and busy → WaitForResult (5 min cap), then send", but `handle_fire` never
waits — it errors immediately on a held lease.

## Key architectural facts that shape the design

- `publish(engine, TurnEvent)` (`engine.rs:554`) is the single function that
  appends to the journal and broadcasts to attached subscribers. It needs
  **neither the input lease nor the transport** — it just records an event.
- Turns are serialized by the engine actor's mpsc command queue, not by the
  lease. The `InputLease` is a *coordination token* so two interactive writers
  don't both issue prompts; it is not what guarantees actor safety.
- A `Cmd::Prompt` that arrives while a turn is in flight is **dropped** with a
  warning (`engine.rs:530`). So an "inject as a real turn" path must run when the
  session is idle.
- `TurnEvent` (`event.rs`) is the common cross-agent vocabulary. The idiomatic
  way to add a new kind of event is a new variant with explicit wire mapping —
  not a `Raw` shim.

## Goal

Let `inject_parent` (and any client) drop a message into a live session's
transcript **without** fighting the interactive input lease. Two modes,
selected per subscriber:

- **note (default):** the message appears in the transcript as a distinct
  "background" entry that references the originating child session. The agent
  does **not** respond. No lease required.
- **respond (opt-in):** the message is delivered as a real user turn the agent
  processes and answers, as `injectIntoParent` does in the `claude-agent`
  project.

## Design

### 1. New event variant — `TurnEvent::Note`

`crates/roy/src/event.rs`:

```rust
Note {
    text: String,
    source_session: Option<String>,
}
```

Wire form: `{"type":"note","text":…,"source_session":…}` (`source_session` may
be `null`). Add the mapping to both `event_to_json` and `event_from_json`, plus a
round-trip test. `source_session` carries the child session id so the UI can link
back to "the session that produced this".

### 2. New command — `ClientCommand::Inject`

`crates/roy/src/control.rs`:

```rust
Inject {
    session: String,
    text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_session: Option<String>,
    #[serde(default)]
    respond: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timeout_ms: Option<u64>, // respond=true only; default 600_000
}
```

A dedicated command rather than overloading `Fire`: an inject targets an existing
live session and never spawns or resumes a child.

### 3. Responses (`ServerEvent`)

- `respond = false` → new `ServerEvent::Injected { session, seq }` (the seq of
  the appended `Note`).
- `respond = true` → reuse `FireDone` / `FireTimeout` / `FireError` — identical
  wait-for-result semantics to `Fire`.
- Session not live → existing `ErrorCode::NoSession`. (Inject requires a live
  session; resuming an archived one is the caller's job, out of scope here.)

### 4. Engine methods (`crates/roy/src/engine.rs`)

- `pub async fn inject_note(&self, text: String, source_session: Option<String>) -> Result<Seq>`
  — calls `publish(self, TurnEvent::Note { text, source_session })`. No lease, no
  transport. Returns the appended seq. **This is the fix for the reported bug.**
- `respond = true` (approach A1):
  - Add an `AtomicBool turn_active`, set on `drive_turn` entry and cleared on
    exit, exposed via `is_busy()`.
  - Add `pub fn inject_prompt(&self, text: String) -> Result<()>` that pushes
    `Cmd::Prompt` directly into the actor queue, **bypassing the lease flag**.
  - Daemon `respond` path: if `is_busy()`, `wait_for_result` for the in-flight
    turn first; then `inject_prompt`; then `wait_for_result` for the injected
    turn and reply `FireDone`/`FireTimeout`/`FireError`.
  - Known limitation: a narrow race remains if the human submits a turn in the
    same instant. Acceptable for an opt-in mode on a session the user is actively
    watching. The note mode (default) has no such race.

### 5. Daemon handler

`handle_inject` in `daemon.rs`:

1. Resolve the live engine for `session`; `NoSession` if absent.
2. `respond = false` → `engine.inject_note(...)`, reply `Injected { session, seq }`.
3. `respond = true` → the A1 flow above, replying with the `Fire*` events.

### 6. `inject_parent` subscriber + roy_client (`crates/roy-scheduler`)

- Config gains `respond: bool` (default `false`):
  `{ "session_id": "...", "prefix": "...", "respond": false }`.
- `source_session` = `fire_result.session_id` (the child fire's session — already
  available in `FireSuccess`).
- Add `roy_client::inject(socket, session, text, source_session, respond, timeout)`
  beside the existing `fire(...)`. It frames `ClientCommand::Inject` and maps the
  reply: `Injected` → ok; `FireDone` → ok; `FireTimeout` → error; `FireError`/
  `Error` → error.
- Rewrite `inject_parent::execute` to call `inject(...)` instead of
  `fire(Resume)`.
- Fix the stale header comment in `inject_parent.rs` to describe the real
  behavior.

### 7. roy-web (separate repo, `../roy-web`)

Render `TurnEvent::Note` as a distinct "background" entry (chip + the text), with
a link to `source_session` when present. Required companion change — without it
the injected note arrives over the broadcast but isn't displayed.

### 8. Docs

- `docs/wire-protocol.md`: document the `note` event and the `inject` command +
  `injected` reply.
- `docs/persistence.md`: `Note` is journaled like any other event; note it
  replays on attach/resume. (Review for any other needed edits.)

## Testing

- `event.rs`: round-trip `Note` (with and without `source_session`).
- `control.rs`: round-trip `Inject` (respond true/false) and `Injected`.
- `daemon.rs` tests (the important ones — they reproduce the live bug):
  - Connection A acquires the input lease on a live session; connection B sends
    `Inject { respond: false }` → B receives `Injected`, and A (attached) receives
    a `Frame` carrying the `Note`. This is exactly the case that fails today.
  - `Inject` into a non-live session → `NoSession`.
  - `Inject { respond: true }` against an idle fake-agent session → `FireDone`
    with the agent's reply.

## Out of scope

- Reworking the `InputLease` model itself (the "fix the lease globally" option).
  Note mode sidesteps the lease entirely; respond mode coexists with it via A1.
- Injecting into archived/non-live sessions (caller resumes first).
- Per-trigger (vs per-agent) inject configuration beyond what subscribers already
  support.
