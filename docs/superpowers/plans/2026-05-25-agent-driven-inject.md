# Agent-driven inject — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the push-model `inject_parent` subscriber + `respond` mode with a thin agent-facing primitive: a `roy inject` CLI the background agent calls when it decides to notify, plus a scheduler `notify_session` that templates the target into the agent's prompt.

**Architecture:** Keep the lease-free `TurnEvent::Note` + `ClientCommand::Inject` (note-only) + `inject_note`. Add a `roy inject` CLI over it. Remove `inject_parent`, `roy_client::inject`, the `respond` field/branch, and the engine's `Cmd::Inject`/oneshot/`pending` machinery (keeping the independent mid-turn `Close` fix). The scheduler gains an optional `notify_session` it appends as a notify instruction to the fired prompt.

**Tech Stack:** Rust workspace (`roy`, `roy-cli`, `roy-scheduler`), clap, sqlx/SQLite, tokio, serde JSON wire protocol.

**Spec:** `docs/superpowers/specs/2026-05-25-agent-driven-inject-design.md`

---

## File Structure

- `crates/roy-scheduler/src/subscribers/inject_parent.rs` — **delete**.
- `crates/roy-scheduler/src/subscribers/{mod.rs,registry.rs}` — drop `InjectParent` wiring.
- `crates/roy-scheduler/src/types.rs` — drop `SubscriberKind::InjectParent`; add `Agent.notify_session`.
- `crates/roy-scheduler/src/roy_client.rs` — delete `inject` + `InjectOutcome`; inline `connect_and_send` into `fire`.
- `crates/roy-scheduler/src/main.rs` — drop inject_parent from kind parsing; add `--notify-session`.
- `crates/roy-scheduler/src/store/agents.rs` — `NewAgent.notify_session` + insert column.
- `crates/roy-scheduler/src/driver.rs` — `effective_prompt(agent)` appends notify instruction.
- `crates/roy-scheduler/migrations/{sqlite,postgres}/0002_notify_session.sql` — **new**.
- `crates/roy/src/control.rs` — `Inject` becomes note-only.
- `crates/roy/src/daemon.rs` — `handle_inject` note-only.
- `crates/roy/src/engine.rs` — revert respond machinery; keep note + Close fix.
- `crates/roy/tests/engine.rs` — drop the oneshot test; adapt the Close test.
- `crates/roy-cli/src/main.rs` — add `inject` subcommand.
- `docs/wire-protocol.md` — `inject` note-only.

---

## Task 1: Remove the `inject_parent` subscriber and `roy_client::inject`

**Files:**
- Delete: `crates/roy-scheduler/src/subscribers/inject_parent.rs`
- Modify: `crates/roy-scheduler/src/subscribers/mod.rs`, `crates/roy-scheduler/src/subscribers/registry.rs`, `crates/roy-scheduler/src/types.rs`, `crates/roy-scheduler/src/main.rs`, `crates/roy-scheduler/src/roy_client.rs`

- [ ] **Step 1: Delete the module file**

```bash
git rm crates/roy-scheduler/src/subscribers/inject_parent.rs
```

- [ ] **Step 2: Drop the module declaration + registry entry**

In `crates/roy-scheduler/src/subscribers/mod.rs` remove the line `pub mod inject_parent;`.
In `crates/roy-scheduler/src/subscribers/registry.rs` remove the line that inserts the builder, i.e.:
```rust
        m.insert(SubscriberKind::InjectParent, super::inject_parent::build);
```

- [ ] **Step 3: Drop the `InjectParent` enum variant + mappings**

In `crates/roy-scheduler/src/types.rs`, find `SubscriberKind` and remove the `InjectParent` variant and its two string mappings (`SubscriberKind::InjectParent => "inject_parent"` in the `as_db`/`as_str` fn, and `"inject_parent" => Some(Self::InjectParent)` in the parse fn). Keep `Webhook` and `NotifyNative`.

- [ ] **Step 4: Drop inject_parent from the CLI kind hint**

In `crates/roy-scheduler/src/main.rs` update the two user-facing strings that list subscriber kinds (the doc comment `/// inject_parent | webhook | notify_native` and the error `"unknown subscriber kind: {:?} (expected inject_parent|webhook|notify_native)"`) to `webhook | notify_native`.

- [ ] **Step 5: Delete `roy_client::inject` + `InjectOutcome`, inline `connect_and_send`**

In `crates/roy-scheduler/src/roy_client.rs`:
- Delete the `InjectOutcome` enum and the entire `pub async fn inject(...)`.
- `connect_and_send` is now only called by `fire`. Inline it back: replace `fire`'s `let mut lines = connect_and_send(socket_path, &cmd).await?;` with the original inline body and delete the `connect_and_send` fn:

```rust
    let stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connecting to roy daemon at {}", socket_path.display()))?;
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let line = serde_json::to_string(&cmd)?;
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
```

- Remove the now-unused imports `Lines` and `OwnedReadHalf` from the `use tokio::io::...` / `use tokio::net::unix::OwnedReadHalf` lines (let the compiler confirm).

- [ ] **Step 6: Build the crate**

Run: `cargo build -p roy-scheduler --all-targets`
Expected: compiles. If `inject_parent` is referenced anywhere else (e.g. a test), the compiler points at it — remove that reference too.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor(roy-scheduler): remove push-model inject_parent subscriber"
```

---

## Task 2: Make `ClientCommand::Inject` note-only and revert the engine respond machinery

This is one coherent change across `control.rs`, `daemon.rs`, `engine.rs`, and the engine tests; it must end compiling + green.

**Files:**
- Modify: `crates/roy/src/control.rs`, `crates/roy/src/daemon.rs`, `crates/roy/src/engine.rs`, `crates/roy/tests/engine.rs`

- [ ] **Step 1: `control.rs` — drop `respond` + `timeout_ms` from `Inject`**

Replace the `Inject` variant with:
```rust
    /// Drop a message into a live session out-of-band. Appends a `Note` event
    /// to the journal/broadcast without taking the input lease, so it lands
    /// even while an interactive client holds the lease. `source_session` links
    /// the `Note` back to the session that produced the message.
    Inject {
        session: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_session: Option<String>,
    },
```
In the `mod tests`, update `inject_command_roundtrips` and `inject_defaults_respond_false_when_absent` to the new shape (drop `respond`/`timeout_ms`); rename the latter to `inject_omits_optional_source_when_absent`:
```rust
    #[test]
    fn inject_command_roundtrips() {
        roundtrip(&ClientCommand::Inject {
            session: "sid".into(),
            text: "result text".into(),
            source_session: Some("child".into()),
        });
        roundtrip(&ClientCommand::Inject {
            session: "sid".into(),
            text: "do this".into(),
            source_session: None,
        });
    }

    #[test]
    fn inject_omits_optional_source_when_absent() {
        let cmd: ClientCommand =
            serde_json::from_str(r#"{"op":"inject","session":"s","text":"t"}"#).unwrap();
        match cmd {
            ClientCommand::Inject { source_session, .. } => assert!(source_session.is_none()),
            _ => panic!("wrong variant"),
        }
    }
```

- [ ] **Step 2: `daemon.rs` — dispatch arm + `handle_inject` note-only**

Update the dispatch arm to the new fields:
```rust
            ClientCommand::Inject {
                session,
                text,
                source_session,
            } => self.handle_inject(session, text, source_session, event_tx).await,
```
Replace the whole `handle_inject` method body with the note-only version (drop the `respond`/`timeout_ms` params and the entire respond branch):
```rust
    async fn handle_inject(
        self: &Arc<Self>,
        session: String,
        text: String,
        source_session: Option<String>,
        event_tx: &EventTx,
    ) {
        let Some(engine) = self.manager.get(&session).await else {
            send_error(
                event_tx,
                Some(session),
                ErrorCode::NoSession,
                "no such session",
            );
            return;
        };
        match engine.inject_note(text, source_session).await {
            Ok(seq) => {
                let _ = event_tx.send(ServerEvent::Injected { session, seq });
            }
            Err(e) => {
                send_error(
                    event_tx,
                    Some(session),
                    ErrorCode::SendFailed,
                    format!("inject_note failed: {e}"),
                );
            }
        }
    }
```
The two daemon tests added for inject (`inject_note_lands_while_lease_held_by_another_connection`, `inject_into_unknown_session_is_no_session`) construct `ClientCommand::Inject { ... respond: false, timeout_ms: None }`. Remove those two fields from both test constructions so they match the new shape.

- [ ] **Step 3: `engine.rs` — revert respond machinery, keep `inject_note` + Close fix**

Remove all of: the `Cmd::Inject { text, done }` variant, the `inject_prompt` method, the `TurnOutcome` type alias, the `PendingTurn` type alias, the `oneshot` import, and the `pending` queue + `done` plumbing in `run_actor`/`run_one_turn`. Keep `inject_note`, and keep `drive_turn -> bool` + the `run_actor` break-on-close.

Target `Cmd` enum:
```rust
enum Cmd {
    Prompt(String),
    /// Abort the in-flight turn. No-op if no turn is running. The actor reacts
    /// by dropping the current `TurnStream`, which makes the transport send
    /// `session/cancel` to the agent; the synthesised terminal `Result` lands
    /// in the journal with `stop_reason: Cancelled`.
    Cancel,
    Close,
}
```

Target `run_actor` (no `pending`, no `done`):
```rust
async fn run_actor(
    engine: Arc<SessionEngine>,
    mut handle: Box<dyn Handle>,
    mut input_rx: mpsc::UnboundedReceiver<Cmd>,
) {
    while let Some(cmd) = input_rx.recv().await {
        let text = match cmd {
            Cmd::Prompt(text) => text,
            // Cancel outside an active turn is a no-op.
            Cmd::Cancel => continue,
            Cmd::Close => break,
        };
        // A `Close` (or channel hang-up) seen mid-turn is consumed inside
        // `drive_turn`; honour it here so the actor winds down instead of
        // blocking forever on the next `recv` (the engine holds its own
        // `input_tx`, so the channel never closes on its own).
        if run_one_turn(&engine, handle.as_mut(), &text, &mut input_rx).await {
            break;
        }
    }
    if let Err(e) = handle.close().await {
        tracing::warn!(
            session = %engine.session_id,
            error = %e,
            "transport close failed; child process may be left in unknown state",
        );
    }
}
```

Target `run_one_turn` (no `done`, no `pending`; returns `bool` = closed):
```rust
/// Journal the prompt, drive one turn to completion, persist the cursor.
/// Returns `true` if a `Close` / channel hang-up was observed mid-turn.
async fn run_one_turn(
    engine: &SessionEngine,
    handle: &mut dyn Handle,
    text: &str,
    input_rx: &mut mpsc::UnboundedReceiver<Cmd>,
) -> bool {
    engine.touch_activity();
    // Journal the user's prompt before driving the turn. Agents don't echo
    // user input over ACP, so without this a refresh / late attach can never
    // reconstruct the user side of the conversation.
    if let Err(e) = publish(
        engine,
        TurnEvent::UserPrompt {
            text: text.to_string(),
        },
    )
    .await
    {
        tracing::error!(
            session = %engine.session_id,
            error = %e,
            "failed to journal user prompt; turn still dispatched",
        );
    }
    let closed = drive_turn(engine, handle, text, input_rx).await;
    if let Some(cursor) = handle.resume_cursor() {
        *engine.resume_cursor.lock().unwrap() = Some(cursor);
        if let Err(e) = engine.persist_metadata().await {
            tracing::warn!(
                session = %engine.session_id,
                error = %e,
                "failed to persist session metadata after turn",
            );
        }
    }
    closed
}
```

Target `drive_turn` (drop the `pending` param and the `Cmd::Inject` arm; keep `-> bool` and `return true` on Close):
```rust
async fn drive_turn(
    engine: &SessionEngine,
    handle: &mut dyn Handle,
    text: &str,
    input_rx: &mut mpsc::UnboundedReceiver<Cmd>,
) -> bool {
    let (mut stream, cancel) = match handle.send(text).await {
        Ok(pair) => pair,
        Err(e) => {
            tracing::warn!(
                session = %engine.session_id,
                error = %e,
                "transport refused turn; synthesising terminal Result",
            );
            if let Err(e) = publish(
                engine,
                TurnEvent::Result {
                    cost_usd: None,
                    stop_reason: StopReason::Error,
                },
            )
            .await
            {
                tracing::error!(
                    session = %engine.session_id,
                    error = %e,
                    "failed to journal synthetic terminal Result",
                );
            }
            return false;
        }
    };
    let mut cancel = Some(cancel);
    loop {
        tokio::select! {
            biased;
            cmd = input_rx.recv() => match cmd {
                Some(Cmd::Cancel) => {
                    drop(cancel.take());
                }
                Some(Cmd::Prompt(_)) => {
                    tracing::warn!(
                        session = %engine.session_id,
                        "ignoring Cmd::Prompt during active turn",
                    );
                }
                Some(Cmd::Close) | None => return true,
            },
            event = stream.next() => match event {
                Some(event) => {
                    if let Err(e) = publish(engine, event).await {
                        tracing::error!(
                            session = %engine.session_id,
                            error = %e,
                            "journal append failed",
                        );
                    }
                }
                None => break,
            },
        }
    }
    false
}
```

Keep `inject_note` unchanged. Confirm no remaining references to `VecDeque` (remove from imports if now unused) or `oneshot`.

- [ ] **Step 4: `tests/engine.rs` — drop the oneshot test, adapt the Close test**

Delete `inject_prompt_receiver_resolves_with_this_turns_result` entirely.
Rewrite `close_during_turn_winds_down_and_does_not_hang` to not use `inject_prompt` (a held turn + `Close` is enough now). The proof of "didn't hang" is that a fresh `attach` after close eventually sees a terminal `Result` (the dropped turn) OR the call returns; simplest deterministic form:
```rust
#[tokio::test]
async fn close_during_turn_winds_down_and_does_not_hang() {
    let journal_dir = tmp_journal_dir();
    let engine = SessionEngine::spawn(
        fake_acp_transport_with(&["--cancellable"]),
        opts(journal_dir.clone()),
        test_cfg(),
    )
    .await
    .unwrap();

    let lease = engine.try_acquire_input().expect("free lease");
    let attach = engine.attach(None).await.unwrap();
    lease.send("hold").unwrap();

    // Wait until the turn is genuinely active (cancellable fake streams one
    // chunk then waits) so the Close lands mid-turn.
    let mut stream = attach.stream;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut active = false;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, stream.next()).await {
            Ok(Some(entry)) => {
                if matches!(entry.event, TurnEvent::AssistantText { .. }) {
                    active = true;
                    break;
                }
            }
            _ => break,
        }
    }
    assert!(active, "turn should be active before close");

    // Close mid-turn. With the fix the actor breaks and the broadcast channel
    // closes, so the remaining stream terminates within the timeout. A hang
    // (the bug) would make the stream never end.
    drop(lease);
    engine.close().unwrap();

    let drained = tokio::time::timeout(Duration::from_secs(3), async {
        while stream.next().await.is_some() {}
    })
    .await;
    assert!(
        drained.is_ok(),
        "stream must terminate after Close; a timeout means the actor hung",
    );

    let _ = std::fs::remove_dir_all(&journal_dir);
}
```

- [ ] **Step 5: Build + test the roy crate**

Run: `cargo test -p roy --lib && cargo test -p roy --test engine`
Expected: PASS. Fix any leftover references (the compiler will flag removed `Cmd::Inject`/`inject_prompt`/`oneshot`).

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor(roy): make Inject note-only; drop respond machinery (keep Close fix)"
```

---

## Task 3: `roy inject` CLI subcommand

**Files:**
- Modify: `crates/roy-cli/src/main.rs`

- [ ] **Step 1: Add the subcommand enum variant + args struct**

In the `enum Cmd` (after `Fire(FireArgs)`), add:
```rust
    /// Inject a message into a live session as a background note (no input
    /// lease needed). A background agent calls this to notify a session.
    Inject(InjectArgs),
```
Add the args struct near the others (e.g. after `CloseArgs`):
```rust
#[derive(clap::Args)]
struct InjectArgs {
    /// The live session to inject into.
    session: String,
    /// The message text.
    text: String,
    /// Optional source session id to link the note back to (e.g. the child
    /// background session that produced this message).
    #[arg(long)]
    source: Option<String>,
}
```

- [ ] **Step 2: Add the dispatch arm**

In `dispatch`, after `Cmd::Fire(args) => cmd_fire(args).await,`:
```rust
        Cmd::Inject(args) => cmd_inject(args).await,
```

- [ ] **Step 3: Implement `cmd_inject` (mirror `cmd_close`)**

Add near `cmd_close`:
```rust
async fn cmd_inject(args: InjectArgs) -> anyhow::Result<ExitCode> {
    let stream = connect().await?;
    let (reader, mut writer) = stream.into_split();
    let mut events = BufReader::new(reader).lines();

    send_cmd(
        &mut writer,
        &ClientCommand::Inject {
            session: args.session.clone(),
            text: args.text,
            source_session: args.source,
        },
    )
    .await?;
    match read_event(&mut events).await? {
        ServerEvent::Injected { session, seq } => {
            let payload = serde_json::json!({
                "type": "injected",
                "session": session,
                "seq": seq,
            });
            println!("{payload}");
            Ok(ExitCode::SUCCESS)
        }
        ServerEvent::Error { code, message, .. } => {
            eprintln!("roy inject: {code}: {message}");
            Ok(ExitCode::from(2))
        }
        other => anyhow::bail!("unexpected response to Inject: {other:?}"),
    }
}
```

- [ ] **Step 4: Build**

Run: `cargo build -p roy-cli`
Expected: compiles. (`send_cmd`, `read_event`, `connect`, `ClientCommand`, `ServerEvent` are already in scope in this file.)

- [ ] **Step 5: Integration test (daemon-backed)**

Find the existing CLI integration test harness (`grep -rn "fn .*test\|Daemon::new\|assert_cmd\|tests" crates/roy-cli/tests/ 2>/dev/null`; if `crates/roy-cli/tests/` doesn't exist, place the test in `crates/roy/src/daemon.rs` `mod tests` instead, since that's where the wire-level Inject is already tested). Add a test that: spawns a live session, sends `ClientCommand::Inject { session, text, source_session: None }` over a second connection, and asserts a `ServerEvent::Injected`. (This duplicates the existing daemon-level coverage; if `crates/roy-cli` has no integration harness, SKIP a new test here — the daemon test from Task 2 already covers the wire path, and the CLI is a thin shell. Note this decision in the commit.)

- [ ] **Step 6: Commit**

```bash
git add crates/roy-cli/src/main.rs
git commit -m "feat(roy-cli): add roy inject subcommand"
```

---

## Task 4: Scheduler `notify_session` + prompt templating

**Files:**
- Create: `crates/roy-scheduler/migrations/sqlite/0002_notify_session.sql`, `crates/roy-scheduler/migrations/postgres/0002_notify_session.sql`
- Modify: `crates/roy-scheduler/src/types.rs`, `crates/roy-scheduler/src/store/agents.rs`, `crates/roy-scheduler/src/main.rs`, `crates/roy-scheduler/src/driver.rs`

- [ ] **Step 1: Add the migrations**

`crates/roy-scheduler/migrations/sqlite/0002_notify_session.sql`:
```sql
-- Optional roy session to notify. When set, the scheduler appends a
-- `roy inject <notify_session> ...` instruction to the agent's fired prompt.
ALTER TABLE agents ADD COLUMN notify_session TEXT;
```
`crates/roy-scheduler/migrations/postgres/0002_notify_session.sql`: identical body (Postgres also accepts `ALTER TABLE agents ADD COLUMN notify_session TEXT;`).

- [ ] **Step 2: Add the field to `Agent` (types.rs)**

In `crates/roy-scheduler/src/types.rs`, add to `struct Agent` (after `persistent_session_id`):
```rust
    /// Optional roy session id to notify. When set, the fired prompt is
    /// augmented with a `roy inject <id> ...` instruction so the agent can
    /// self-report into that session.
    pub notify_session: Option<String>,
```
(The `FromRow` derive maps the new `notify_session` column automatically; `SELECT *` already covers it.)

- [ ] **Step 3: Add the write path (store/agents.rs)**

In `NewAgent` add `pub notify_session: Option<String>,`. Update `insert` to include the column:
```rust
    sqlx::query(
        "INSERT INTO agents (id, name, preset, project_id, task, model, persistent, notify_session, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&new.name)
    .bind(&new.preset)
    .bind(&new.project_id)
    .bind(&new.task)
    .bind(&new.model)
    .bind(persistent_int)
    .bind(&new.notify_session)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;
```
In `mod tests`, add `notify_session: None,` to the `sample()` `NewAgent`, and add an assertion in a test (or extend `insert_then_get_returns_same_agent`) that a `Some(...)` notify_session round-trips:
```rust
    #[tokio::test]
    async fn notify_session_round_trips() {
        let (_d, pool) = fresh_pool().await;
        let mut n = sample();
        n.notify_session = Some("main-sid".into());
        let a = insert(&pool, n).await.unwrap();
        let back = get_by_id(&pool, &a.id).await.unwrap().unwrap();
        assert_eq!(back.notify_session.as_deref(), Some("main-sid"));
    }
```

- [ ] **Step 4: CLI flag (main.rs)**

In `AgentAddArgs` add:
```rust
    /// Roy session id to notify. When set, the agent's fired prompt gets a
    /// `roy inject <id> ...` instruction so it can self-report findings.
    #[arg(long)]
    notify_session: Option<String>,
```
In the `AgentsCmd::Add(a)` arm, pass it through to `NewAgent`:
```rust
                store::agents::NewAgent {
                    name: a.name,
                    preset: a.preset,
                    project_id: a.project,
                    task: a.task,
                    model: a.model,
                    persistent: a.persistent,
                    notify_session: a.notify_session,
                },
```

- [ ] **Step 5: Driver — append the notify instruction to the fired prompt**

In `crates/roy-scheduler/src/driver.rs`, add a helper:
```rust
/// The prompt sent to the agent on a fire. When the agent has a `notify_session`,
/// append an instruction so it can self-report into that session via the CLI.
fn effective_prompt(agent: &Agent) -> String {
    match &agent.notify_session {
        None => agent.task.clone(),
        Some(sid) => format!(
            "{}\n\n[notify] You are running in the background. When you have a \
finding to report, run exactly one Bash command:\n    roy inject {} \"<your \
concise message>\"\nIf you have nothing to report, do not call it. Do not \
inject more than once.",
            agent.task, sid
        ),
    }
}
```
Replace both `agent.task.clone()` arguments to `roy_client::fire(...)` in `invoke_fire` with `effective_prompt(agent)`.

Add a unit test in `driver.rs` `mod tests`:
```rust
    #[test]
    fn effective_prompt_appends_notify_when_set() {
        let mut a = sample_agent(); // reuse the module's agent builder
        a.notify_session = Some("main-sid".into());
        let p = effective_prompt(&a);
        assert!(p.starts_with(&a.task));
        assert!(p.contains("roy inject main-sid"));
    }

    #[test]
    fn effective_prompt_is_task_when_unset() {
        let a = sample_agent();
        assert_eq!(effective_prompt(&a), a.task);
    }
```
(If `driver.rs` tests have no `sample_agent()` helper, build an `Agent` inline with `notify_session: None`; check the existing driver tests for how they construct an `Agent`.)

- [ ] **Step 6: Build + test the scheduler**

Run: `cargo test -p roy-scheduler`
Expected: PASS (migration applies; new field round-trips; prompt templating tests pass). The first `serve`/test run auto-applies `0002`.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(roy-scheduler): notify_session — template roy inject into fired prompt"
```

---

## Task 5: Wire-protocol docs — `inject` note-only

**Files:**
- Modify: `docs/wire-protocol.md`

- [ ] **Step 1: Replace the `inject` command entry**

Update the ClientCommand table row to drop `respond`/`timeout_ms`:
```
| `inject`          | `session`, `text`, optional `source_session`                                                    |
```
Replace the prose block describing `inject` with the note-only version:
```markdown
`inject` appends a `note` event to a **live** session's journal/broadcast
without taking the input lease (so it lands even while an interactive client
holds it). Reply: `{"kind":"injected","session":"<sid>","seq":N}`. An
unknown/non-live session replies `error` with code `no_session`. Used by the
`roy inject` CLI for agent self-reporting.
```
(Remove the `respond: true` / `fire_done|fire_timeout|fire_error` paragraph.) The `note` event row and the `injected` ServerEvent row stay unchanged.

- [ ] **Step 2: Commit**

```bash
git add docs/wire-protocol.md
git commit -m "docs(wire-protocol): inject is note-only"
```

---

## Task 6: Workspace verification

- [ ] **Step 1: CI gate**

Run:
```bash
cargo fmt --all -- --check
cargo build --workspace --all-targets
cargo test --workspace --no-fail-fast
```
Expected: all green, no dangling references to `inject_parent`, `InjectOutcome`, `Cmd::Inject`, `inject_prompt`, `respond`.

- [ ] **Step 2: Confirm removals are complete**

Run: `grep -rn "inject_parent\|InjectOutcome\|inject_prompt\|Cmd::Inject" crates/ ; echo "exit grep: $?"`
Expected: no matches (grep exits non-zero).

- [ ] **Step 3: Manual smoke (agent-driven notify)**

```bash
roy status || nohup roy serve > /tmp/roy-daemon.log 2>&1 &
roy-scheduler status || nohup roy-scheduler serve > /tmp/roy-scheduler.log 2>&1 &

# Direct CLI test: inject into a live session you have open in roy-web
roy inject <your-live-session> "hello from cli" && \
  grep -c '"type":"note"' ~/.roy/journals/<your-live-session>.jsonl

# Scheduler test: an agent that self-notifies on a finding
roy-scheduler agents add --name finder --preset claude \
  --notify-session <your-live-session> \
  --task 'Look at git log -5. If anything mentions "fix", roy inject the summary; else do nothing.'
# add a cron trigger, wait one minute, confirm a note (or not) lands.
```
Expected: `roy inject` lands a `note` in the session journal and renders in roy-web; the scheduled agent self-injects only when its task says to.

- [ ] **Step 4: Final commit (any fmt fixups)**

```bash
git add -A && git commit -m "chore: workspace verification fixups"
```

---

## Self-review notes (resolved)

- **Spec coverage:** remove inject_parent + roy_client::inject (T1), Inject note-only + engine revert keeping Close fix (T2), `roy inject` CLI (T3), scheduler `notify_session` + prompt templating (T4), wire docs (T5), verification (T6). All spec sections mapped.
- **Type consistency:** `Inject { session, text, source_session }` (no respond/timeout_ms) used identically in control, daemon dispatch, roy-cli, and the daemon tests. `handle_inject(session, text, source_session, event_tx)`. `NewAgent.notify_session` / `Agent.notify_session` / `--notify-session` consistent. `effective_prompt(&Agent) -> String`.
- **Placeholders:** the only deferred decision is the Task 3 Step 5 CLI integration test (skip if `crates/roy-cli` has no harness — daemon test already covers the wire path); all production code is complete.
- **Ordering:** Task 1 (scheduler removal) is self-contained and compiles. Task 2 bundles the coupled roy-crate changes so the tree compiles after it. Tasks 3–5 are additive/independent.
