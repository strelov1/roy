# Agent-driven inject + remove the push-model `inject_parent`

## Motivation

The real workflow: launch background agents to search for something; when an
agent *finds* something it notifies the user's main session; the user reviews
and manually kicks off the next chain of agents.

The decision of **whether and what to report is the agent's** (an LLM) — so it
belongs in the agent's prompt, not in Rust routing code. The previously-merged
`inject_parent` subscriber is a *push* model ("always deliver the result to a
fixed session") plus a complex opt-in `respond` mode. On this concrete use case
both are unjustified: a thin "let the agent inject a note when it decides to"
primitive covers "notify when found" *and* "ask for clarification" with zero
branching logic.

This spec **adds** an agent-facing inject primitive and **removes** the
push-model `inject_parent` subscriber and the `respond` machinery.

## What we keep (the lease-free note primitive)

These already exist and are the simple, justified core — keep them:

- `TurnEvent::Note { text, source_session }` (`crates/roy/src/event.rs`) + wire
  mapping, and its render arms in `roy-gateway` (`formatting.rs`), `roy-cli`
  (`mcp.rs`), and `roy-web` (`ChatView.svelte` / `wire.ts`).
- `SessionEngine::inject_note(text, source_session) -> Seq`
  (`crates/roy/src/engine.rs`) — appends a `Note` to the journal + broadcast
  with no input lease and no transport.
- `ServerEvent::Injected { session, seq }`.
- The mid-turn `Close` hang fix in the actor (`drive_turn -> bool`, `run_actor`
  breaks on `true`) — this is an **independent** bug fix (the hang was latent in
  the original actor too) and stays, re-applied to the simplified actor.

## What we add

### 1. `roy inject` CLI subcommand (`crates/roy-cli`)

```
roy inject <session_id> "<text>" [--source <child_session_id>]
```

- Connects to the daemon (same `ROY_SOCKET` client plumbing as other
  subcommands), sends `ClientCommand::Inject { session, text, source_session }`,
  awaits `ServerEvent::Injected` / `Error`.
- Stdout: the daemon's reply as one JSON line (consistent with `roy run` /
  `roy attach` discipline). Structured logs to stderr.
- Exit codes: `0` on `Injected`; `2` on `ServerEvent::Error` (e.g. `no_session`
  when the target isn't live) or transport failure.
- This is what a background agent calls from its Bash tool to notify a session.

### 2. Scheduler templates the notify target into the agent prompt (`crates/roy-scheduler`)

- New optional agent field `notify_session: Option<String>` (SQLite column on
  `agents` + `agents add --notify-session <id>` flag + surfaced in
  `agents show`/`list`).
- At fire time, if `notify_session` is set, the effective prompt sent to the
  daemon is the agent's `task` followed by a fixed instruction block, e.g.:

  ```
  <task>

  [notify] You are running in the background. When you have a finding to
  report, run exactly one Bash command:
      roy inject <notify_session> "<your concise message>"
  If you have nothing to report, do not call it. Do not inject more than once.
  ```

  `<notify_session>` is substituted with the stored id. No wire-protocol change:
  the scheduler already sends a `prompt` string via `ClientCommand::Fire`; we
  only change how that string is built.

- The agent self-injects conditionally; the user reads the `Note` in their main
  session (roy-web) and triggers the next chain manually.

## What we remove (revert unjustified complexity)

### roy-scheduler
- Delete the `inject_parent` subscriber module
  (`crates/roy-scheduler/src/subscribers/inject_parent.rs`).
- Remove its registration (`subscribers/registry.rs`), the
  `SubscriberKind::InjectParent` variant + its string mapping (`types.rs`), and
  the `inject_parent` branch of `subscribers add` kind parsing (`main.rs`).
- Delete `roy_client::inject` and `InjectOutcome`
  (`crates/roy-scheduler/src/roy_client.rs`) — only `inject_parent` used them.
  Keep `roy_client::fire`. Since `fire` becomes the only caller of the
  `connect_and_send` helper, inline it back into `fire` (a one-call helper no
  longer earns its keep).
- `webhook` and `notify_native` subscriber kinds are unaffected.

### roy daemon + engine
- `ClientCommand::Inject` drops `respond` and `timeout_ms`; becomes note-only:
  `Inject { session, text, source_session }`.
- `handle_inject` (`daemon.rs`) drops the entire `respond == true` branch; only
  the note path remains (resolve live engine → `inject_note` → `Injected`;
  `NoSession` if not live).
- Engine: remove `Cmd::Inject`, `inject_prompt`, the `oneshot`/`TurnOutcome`/
  `PendingTurn` machinery, and `run_one_turn`'s `done` param + the `pending`
  queue. Revert the actor to the simple "one `recv` → drive one turn" shape —
  **but keep `drive_turn -> bool` and the `run_actor` break-on-close** so the
  mid-turn `Close` hang stays fixed.

### docs
- `docs/wire-protocol.md`: update the `inject` command entry to note-only (drop
  `respond`/`timeout_ms` and the `fire_*` reply variants); `injected` stays.
- The `background-agents` skill doc (separate, in `~/.claude/skills`) will need a
  follow-up to describe `notify_session` + `roy inject` instead of
  `inject_parent` — noted, not part of this repo change.

## Data flow (end state)

```
scheduler fire (agent has notify_session)
  → daemon Fire: spawn agent with prompt = task + notify-instruction
  → agent searches; on a finding runs:  roy inject <main> "[found] ..."
  → roy inject → ClientCommand::Inject → handle_inject → inject_note
  → Note appended to <main> journal + broadcast (no lease)
  → roy-web renders the "background" note (linking source_session if given)
  → user reviews, manually fires the next chain
```

## Testing

- `roy-cli`: a daemon-backed test (reuse the daemon test harness) where `roy
  inject` against a live session yields `Injected` and the note lands in the
  journal; against an unknown session yields exit 2 / `no_session`.
- `roy-scheduler`: unit test that prompt-building appends the notify instruction
  with the substituted `notify_session` when set, and leaves the prompt
  untouched when unset.
- `roy` engine: keep the mid-turn `Close` regression test
  (`close_during_turn_winds_down_and_does_not_hang`), adapting it to the
  simplified actor (it no longer needs a queued inject — a held turn + `Close`
  is enough). Remove the `inject_prompt`/oneshot test.
- Whole workspace: `cargo fmt --all -- --check`, `cargo build --workspace
  --all-targets`, `cargo test --workspace --no-fail-fast` all green, with no
  dangling references to the removed items.

## Out of scope

- `roy run --notify-session` (templating the notify instruction for ad-hoc,
  non-scheduled agents) — possible later nicety; the user can put the `roy
  inject` line in the prompt manually for one-shots.
- Auto-chaining (an agent launching the next chain itself) — the user
  deliberately stays in the loop and triggers the next chain manually.
- Resolving the agent's *own* roy session id for `--source` automatically — the
  flag is optional; omit when unknown.
