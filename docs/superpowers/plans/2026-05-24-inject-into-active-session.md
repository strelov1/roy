# Inject a message into an active session — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a background agent's `inject_parent` subscriber deliver its result into a session the user is actively viewing in roy-web, without fighting the interactive input lease.

**Architecture:** Add a lease-free `TurnEvent::Note` event and an `Inject` control command. Default (note) mode appends the message to the journal + broadcast via `publish()` — no lease, no transport. Opt-in (respond) mode drives a real turn by waiting for idle and pushing a prompt past the lease flag. The `inject_parent` subscriber switches from `Fire{Resume}` to `Inject`.

**Tech Stack:** Rust (cargo workspace: `roy`, `roy-scheduler`), serde JSON wire protocol, tokio actors. roy-web companion (Svelte/TypeScript) in `../roy-web`.

**Spec:** `docs/superpowers/specs/2026-05-24-inject-into-active-session-design.md`

---

## File Structure

- `crates/roy/src/event.rs` — add `TurnEvent::Note` variant + wire mapping.
- `crates/roy/src/control.rs` — add `ClientCommand::Inject` + `ServerEvent::Injected`.
- `crates/roy/src/engine.rs` — add `inject_note`, `turn_active`/`is_busy`, `inject_prompt`.
- `crates/roy/src/daemon.rs` — add `handle_inject` + dispatch arm + tests.
- `crates/roy-scheduler/src/roy_client.rs` — add `inject()` helper + `InjectOutcome`.
- `crates/roy-scheduler/src/subscribers/inject_parent.rs` — add `respond` config, switch to `inject()`, fix stale doc.
- `docs/wire-protocol.md` — document the `note` event + `inject` command + `injected` reply.
- `../roy-web/src/lib/wire.ts` — mirror `note` event + `inject`/`injected` types.
- `../roy-web/src/lib/ChatView.svelte` — render the `note` event.

---

## Task 1: `TurnEvent::Note` variant

**Files:**
- Modify: `crates/roy/src/event.rs` (enum at `:176`, `event_to_json` at `:61`, `event_from_json` at `:96`)
- Test: `crates/roy/src/event.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/roy/src/event.rs`:

```rust
    #[test]
    fn note_round_trips_through_wire() {
        let e = TurnEvent::Note {
            text: "Запуск #4: 1 + 4 = 5".into(),
            source_session: Some("child-sid".into()),
        };
        let v = event_to_json(&e);
        assert_eq!(v["type"], "note");
        assert_eq!(v["text"], "Запуск #4: 1 + 4 = 5");
        assert_eq!(v["source_session"], "child-sid");
        let back = event_from_json(&v).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn note_round_trips_without_source() {
        let e = TurnEvent::Note {
            text: "hello".into(),
            source_session: None,
        };
        let v = event_to_json(&e);
        assert!(v["source_session"].is_null());
        assert_eq!(event_from_json(&v).unwrap(), e);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p roy --lib event::tests::note_round_trips_through_wire`
Expected: FAIL — `no variant named Note`.

- [ ] **Step 3: Add the variant**

In `crates/roy/src/event.rs`, add to the `TurnEvent` enum (after `Result { .. }`, before `Raw(Value)`):

```rust
    /// A message injected into the session out-of-band (e.g. a background
    /// agent's result landing in the parent session). Not produced by the
    /// agent and not a user turn — UIs render it as a distinct "background"
    /// entry. `source_session` links back to the session that produced it.
    Note {
        text: String,
        source_session: Option<String>,
    },
```

- [ ] **Step 4: Add the wire mapping**

In `event_to_json`, add a match arm (before `TurnEvent::Raw`):

```rust
        TurnEvent::Note {
            text,
            source_session,
        } => json!({
            "type": "note",
            "text": text,
            "source_session": source_session,
        }),
```

In `event_from_json`, add a match arm (before `"raw" =>`):

```rust
        "note" => Ok(TurnEvent::Note {
            text: v
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            source_session: v
                .get("source_session")
                .and_then(Value::as_str)
                .map(str::to_string),
        }),
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p roy --lib event::tests`
Expected: PASS (all event tests).

- [ ] **Step 6: Commit**

```bash
git add crates/roy/src/event.rs
git commit -m "feat(roy): add TurnEvent::Note event variant"
```

---

## Task 2: `ClientCommand::Inject` + `ServerEvent::Injected`

**Files:**
- Modify: `crates/roy/src/control.rs` (`ClientCommand` enum at `:145`, `ServerEvent` enum at `:284`)
- Test: `crates/roy/src/control.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/roy/src/control.rs`:

```rust
    #[test]
    fn inject_command_roundtrips() {
        roundtrip(&ClientCommand::Inject {
            session: "sid".into(),
            text: "result text".into(),
            source_session: Some("child".into()),
            respond: false,
            timeout_ms: None,
        });
        roundtrip(&ClientCommand::Inject {
            session: "sid".into(),
            text: "do this".into(),
            source_session: None,
            respond: true,
            timeout_ms: Some(60_000),
        });
    }

    #[test]
    fn inject_defaults_respond_false_when_absent() {
        let cmd: ClientCommand =
            serde_json::from_str(r#"{"op":"inject","session":"s","text":"t"}"#).unwrap();
        match cmd {
            ClientCommand::Inject { respond, source_session, .. } => {
                assert!(!respond);
                assert!(source_session.is_none());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn injected_event_roundtrips() {
        roundtrip(&ServerEvent::Injected {
            session: "sid".into(),
            seq: 42,
        });
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p roy --lib control::tests::inject_command_roundtrips`
Expected: FAIL — `no variant named Inject`.

- [ ] **Step 3: Add `ClientCommand::Inject`**

In `crates/roy/src/control.rs`, add to the `ClientCommand` enum (after `Fire { .. }`, before `ListProjects`):

```rust
    /// Drop a message into a live session out-of-band. `respond = false`
    /// (default) appends a `Note` event to the journal/broadcast without
    /// taking the input lease — works even while an interactive client holds
    /// it. `respond = true` delivers `text` as a real user turn the agent
    /// answers (waits for any in-flight turn first), replying with the same
    /// `Fire*` events as `Fire`. `source_session` links a `Note` back to the
    /// session that produced the message (note mode only).
    Inject {
        session: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_session: Option<String>,
        #[serde(default)]
        respond: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
```

- [ ] **Step 4: Add `ServerEvent::Injected`**

In `crates/roy/src/control.rs`, add to the `ServerEvent` enum (after `InputReleased { .. }`):

```rust
    /// Response to `Inject { respond: false }`: the seq of the appended `Note`.
    Injected { session: String, seq: Seq },
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p roy --lib control::tests`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/roy/src/control.rs
git commit -m "feat(roy): add Inject command and Injected event to control protocol"
```

---

## Task 3: Engine `inject_note`

**Files:**
- Modify: `crates/roy/src/engine.rs` (impl `SessionEngine`, near `set_model` at `:224`)
- Test: `crates/roy/src/engine.rs` (`#[cfg(test)] mod tests` — uses the existing fake transport harness)

- [ ] **Step 1: Find the existing engine test harness**

Run: `grep -n "async fn\|fn spawn_test_engine\|FakeTransport\|mod tests" crates/roy/src/engine.rs | tail -30`
Expected: shows the `#[cfg(test)] mod tests` block and a helper that builds an engine over a fake transport. Reuse whatever helper spawns a test engine (commonly named like `test_engine()` / `spawn_fake`). Read the first existing test to copy its setup verbatim.

- [ ] **Step 2: Write the failing test**

Add to `mod tests` in `crates/roy/src/engine.rs`, copying the engine-construction lines from the first existing test (shown by Step 1) into the marked spot:

```rust
    #[tokio::test]
    async fn inject_note_appends_without_lease() {
        // <copy the same engine setup the other tests use to get `engine`>
        let engine = /* existing test helper that returns Arc<SessionEngine> */;

        // Hold the input lease, as an interactive client would.
        let _lease = engine.try_acquire_input().expect("first lease");

        // Inject still succeeds despite the held lease.
        let seq = engine
            .inject_note("background result".into(), Some("child-sid".into()))
            .await
            .expect("inject_note");

        let entries = engine.replay_from(seq).await.unwrap();
        let note = entries.iter().find(|e| e.seq == seq).expect("note entry");
        assert_eq!(
            note.event,
            TurnEvent::Note {
                text: "background result".into(),
                source_session: Some("child-sid".into()),
            }
        );
    }
```

(`replay_from` is the engine method at `engine.rs:339`; `TurnEvent` is already imported in the test module — if not, add `use crate::event::TurnEvent;`.)

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p roy --lib engine::tests::inject_note_appends_without_lease`
Expected: FAIL — `no method named inject_note`.

- [ ] **Step 4: Implement `inject_note`**

In `crates/roy/src/engine.rs`, add to `impl SessionEngine` (right after `set_model`, around `:238`):

```rust
    /// Append a `Note` event to the journal + broadcast. Unlike a prompt this
    /// takes no input lease and never touches the transport, so it lands even
    /// while an interactive client holds the lease. Returns the appended seq.
    pub async fn inject_note(
        &self,
        text: String,
        source_session: Option<String>,
    ) -> Result<Seq> {
        let entry = publish(self, TurnEvent::Note { text, source_session }).await?;
        Ok(entry.seq)
    }
```

(`publish` is the module fn at `engine.rs:554` and returns `Result<JournalEntry>`; `Seq` and `TurnEvent` are already in scope in this file.)

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p roy --lib engine::tests::inject_note_appends_without_lease`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/roy/src/engine.rs
git commit -m "feat(roy): add SessionEngine::inject_note (lease-free journal append)"
```

---

## Task 4: Engine busy flag + `inject_prompt` (for respond mode)

**Files:**
- Modify: `crates/roy/src/engine.rs` (struct at `:71`, `start` at `:152`, `run_actor` at `:434`, impl block)
- Test: `crates/roy/src/engine.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/roy/src/engine.rs`:

```rust
    #[tokio::test]
    async fn is_busy_false_when_idle_and_inject_prompt_drives_turn() {
        // <copy the same engine setup the other tests use to get `engine`>
        let engine = /* existing test helper that returns Arc<SessionEngine> */;

        assert!(!engine.is_busy(), "fresh engine is idle");

        let since = engine.next_seq().await;
        engine.inject_prompt("hello".into()).expect("inject_prompt");

        // The fake transport finishes the turn; wait for the terminal Result.
        let got = engine
            .wait_for_result(since, std::time::Duration::from_secs(5))
            .await
            .expect("wait_for_result");
        assert!(got.is_some(), "turn produced a terminal Result");
    }
```

(Mirror the timeout/assert style of the existing turn-driving tests found in Step 1 of Task 3; if the fake transport needs a scripted reply, copy that scripting from the existing test that drives a full turn.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p roy --lib engine::tests::is_busy_false_when_idle_and_inject_prompt_drives_turn`
Expected: FAIL — `no method named is_busy`.

- [ ] **Step 3: Add the `turn_active` field + import**

At the top of `crates/roy/src/engine.rs`, add to the existing `use std::sync::...` imports (or add a new line):

```rust
use std::sync::atomic::{AtomicBool, Ordering};
```

In the `SessionEngine` struct (`:71`), add after `last_activity`:

```rust
    /// True while a turn is being driven. Lets an out-of-band injector decide
    /// whether to wait for the in-flight turn before pushing its own prompt.
    turn_active: AtomicBool,
```

In `start` (`:152`), add to the `Arc::new(Self { .. })` initializer after `last_activity: StdMutex::new(Instant::now()),`:

```rust
            turn_active: AtomicBool::new(false),
```

- [ ] **Step 4: Set/clear the flag around `drive_turn`**

In `run_actor` (`:441`, the `Cmd::Prompt(text)` arm), wrap the `drive_turn` call:

```rust
                engine.turn_active.store(true, Ordering::SeqCst);
                drive_turn(&engine, handle.as_mut(), &text, &mut input_rx).await;
                engine.turn_active.store(false, Ordering::SeqCst);
```

- [ ] **Step 5: Add `is_busy` + `inject_prompt`**

In `impl SessionEngine`, add after `inject_note` (from Task 3):

```rust
    /// True while a turn is in flight. An out-of-band injector waits on this
    /// before pushing a prompt, because a prompt that arrives mid-turn is
    /// dropped by the actor (`drive_turn`).
    pub fn is_busy(&self) -> bool {
        self.turn_active.load(Ordering::SeqCst)
    }

    /// Queue a prompt without holding the input lease. The actor journals it
    /// as a `UserPrompt` and drives a turn, exactly like a leased `send`. Used
    /// by `Inject { respond: true }`; the caller must ensure the session is
    /// idle (see `is_busy`) or the prompt is dropped mid-turn.
    pub fn inject_prompt(&self, text: String) -> Result<()> {
        self.input_tx
            .send(Cmd::Prompt(text))
            .map_err(|_| RoyError::Protocol("engine actor gone".into()))
    }
```

(`Cmd` is the private enum at `engine.rs:93` — accessible here; `RoyError` is already imported, mirroring `InputLease::send` at `:418`.)

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p roy --lib engine::tests::is_busy_false_when_idle_and_inject_prompt_drives_turn`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/roy/src/engine.rs
git commit -m "feat(roy): add is_busy + inject_prompt for out-of-band turn injection"
```

---

## Task 5: Daemon `handle_inject` + dispatch

**Files:**
- Modify: `crates/roy/src/daemon.rs` (dispatch `match` at `:503`, add handler near `handle_fire` at `:755`)
- Test: `crates/roy/src/daemon.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Add the dispatch arm**

In `crates/roy/src/daemon.rs`, in the command `match` (after the `ClientCommand::Fire { .. } => { .. }` arm at `:503-511`), add:

```rust
            ClientCommand::Inject {
                session,
                text,
                source_session,
                respond,
                timeout_ms,
            } => {
                self.handle_inject(session, text, source_session, respond, timeout_ms, event_tx)
                    .await
            }
```

- [ ] **Step 2: Implement `handle_inject`**

In `crates/roy/src/daemon.rs`, add a new method right after `handle_fire` (after `:888`, before the projects handlers). Follow the `manager.get` + `NoSession` pattern from `handle_cancel_turn` (`:1131`) and the `wait_for_result` → `FireDone`/`FireTimeout`/`FireError` mapping from `handle_fire` (`:864-888`):

```rust
    async fn handle_inject(
        self: &Arc<Self>,
        session: String,
        text: String,
        source_session: Option<String>,
        respond: bool,
        timeout_ms: Option<u64>,
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

        if !respond {
            match engine.inject_note(text, source_session).await {
                Ok(seq) => {
                    let _ = event_tx.send(ServerEvent::Injected { session, seq });
                }
                Err(e) => {
                    send_error(
                        event_tx,
                        Some(session),
                        ErrorCode::SendFailed,
                        &format!("inject_note failed: {e}"),
                    );
                }
            }
            return;
        }

        // respond = true: deliver as a real turn. Wait for any in-flight turn
        // to finish first — a prompt that lands mid-turn is dropped.
        let timeout = Duration::from_millis(timeout_ms.unwrap_or(600_000));
        if engine.is_busy() {
            let since = engine.next_seq().await;
            let _ = engine.wait_for_result(since, timeout).await;
        }

        let since = engine.next_seq().await;
        if let Err(e) = engine.inject_prompt(text) {
            let _ = event_tx.send(ServerEvent::FireError {
                session: Some(session),
                code: ErrorCode::SendFailed,
                message: format!("inject_prompt failed: {e}"),
            });
            return;
        }

        match engine.wait_for_result(since, timeout).await {
            Ok(Some((seq, result, assistant_text))) => {
                let _ = event_tx.send(ServerEvent::FireDone {
                    session,
                    seq_range: (since, seq),
                    result,
                    assistant_text,
                });
            }
            Ok(None) => {
                let _ = event_tx.send(ServerEvent::FireTimeout {
                    session,
                    partial_seq_range: (since, engine.next_seq().await),
                });
            }
            Err(e) => {
                let _ = event_tx.send(ServerEvent::FireError {
                    session: Some(session),
                    code: ErrorCode::SendFailed,
                    message: format!("wait_for_result failed: {e}"),
                });
            }
        }
    }
```

(Confirm the `send_error` helper signature by reading its definition — `grep -n "fn send_error" crates/roy/src/daemon.rs`; adjust the `&str` vs `String` last arg to match. `Duration` is already imported in this file, used by `handle_fire`.)

- [ ] **Step 3: Write the failing daemon test (the lease bug reproduction)**

Find the existing daemon test that drives a Unix-socket / duplex connection and acquires input (search: `grep -n "AcquireInput\|InputAcquired\|duplex\|connect_pair\|fn test" crates/roy/src/daemon.rs | head`). Copy that harness exactly. Add to `mod tests`:

```rust
    #[tokio::test]
    async fn inject_note_lands_while_another_connection_holds_lease() {
        // <copy the harness that boots a Daemon over a live session and gives
        //  you two client connections (conn_a, conn_b) on it, plus the spawned
        //  session id `sid` — mirror the existing two-connection test>

        // Connection A acquires the input lease (interactive client).
        send(&mut conn_a, &ClientCommand::AcquireInput { session: sid.clone() }).await;
        let ev = recv(&mut conn_a).await;
        assert!(matches!(ev, ServerEvent::InputAcquired { acquired: true, .. }));

        // Connection B injects a note — must succeed despite A's lease.
        send(
            &mut conn_b,
            &ClientCommand::Inject {
                session: sid.clone(),
                text: "bg result".into(),
                source_session: Some("child".into()),
                respond: false,
                timeout_ms: None,
            },
        )
        .await;
        let ev = recv(&mut conn_b).await;
        let ServerEvent::Injected { seq, .. } = ev else {
            panic!("expected Injected, got {ev:?}");
        };
        assert!(seq >= 0);
    }

    #[tokio::test]
    async fn inject_into_unknown_session_is_no_session() {
        // <copy the harness that gives you one client connection `conn`>
        send(
            &mut conn,
            &ClientCommand::Inject {
                session: "does-not-exist".into(),
                text: "x".into(),
                source_session: None,
                respond: false,
                timeout_ms: None,
            },
        )
        .await;
        let ev = recv(&mut conn).await;
        assert!(matches!(
            ev,
            ServerEvent::Error { code: ErrorCode::NoSession, .. }
        ));
    }
```

(`send`/`recv`/`conn_a`/`sid` are placeholders for whatever the existing daemon tests name their frame-write / frame-read helpers and fixtures — use the real names from the copied harness.)

- [ ] **Step 4: Run tests to verify they fail then pass**

Run: `cargo test -p roy --lib daemon::tests::inject_note_lands_while_another_connection_holds_lease daemon::tests::inject_into_unknown_session_is_no_session`
Expected: PASS (handler implemented in Step 2). If a test fails to compile on helper names, fix the names to match the copied harness.

- [ ] **Step 5: Commit**

```bash
git add crates/roy/src/daemon.rs
git commit -m "feat(roy): handle Inject command (lease-free note + opt-in respond)"
```

---

## Task 6: roy-scheduler `roy_client::inject`

**Files:**
- Modify: `crates/roy-scheduler/src/roy_client.rs` (after `fire` at `:41`)
- Test: `crates/roy-scheduler/src/roy_client.rs` (`#[cfg(test)] mod tests`, mirrors `fire_done_maps_to_success`)

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/roy-scheduler/src/roy_client.rs`:

```rust
    #[tokio::test]
    async fn inject_note_maps_to_noted() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sock");
        spawn_mock(
            path.clone(),
            ServerEvent::Injected {
                session: "sid".into(),
                seq: 7,
            },
        )
        .await;

        let out = inject(
            &path,
            "sid".into(),
            "bg result".into(),
            Some("child".into()),
            false,
            Duration::from_secs(60),
        )
        .await
        .unwrap();

        match out {
            InjectOutcome::Noted { session_id, seq } => {
                assert_eq!(session_id, "sid");
                assert_eq!(seq, 7);
            }
            other => panic!("expected Noted, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p roy-scheduler --lib roy_client::tests::inject_note_maps_to_noted`
Expected: FAIL — `cannot find function inject` / `InjectOutcome`.

- [ ] **Step 3: Implement `inject` + `InjectOutcome`**

In `crates/roy-scheduler/src/roy_client.rs`, add after the `FireOutcome` enum (`:38`):

```rust
/// Outcome of an Inject call. `Noted` is the respond=false reply; the other
/// three mirror Fire for respond=true.
#[derive(Debug, Clone)]
pub enum InjectOutcome {
    Noted {
        session_id: String,
        seq: u64,
    },
    Done(FireSuccess),
    Timeout {
        session_id: String,
        partial_seq_range: (u64, u64),
    },
    Error {
        session_id: Option<String>,
        code: String,
        message: String,
    },
}
```

Add after the `fire` function (`:118`):

```rust
pub async fn inject(
    socket_path: &Path,
    session: String,
    text: String,
    source_session: Option<String>,
    respond: bool,
    timeout: Duration,
) -> Result<InjectOutcome> {
    let stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connecting to roy daemon at {}", socket_path.display()))?;
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    let cmd = ClientCommand::Inject {
        session,
        text,
        source_session,
        respond,
        timeout_ms: Some(timeout.as_millis() as u64),
    };
    let line = serde_json::to_string(&cmd)?;
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;

    loop {
        let raw = lines
            .next_line()
            .await?
            .ok_or_else(|| anyhow!("daemon hung up before terminal Inject event"))?;
        let evt: ServerEvent = serde_json::from_str(raw.trim())?;
        match evt {
            ServerEvent::Injected { session, seq } => {
                return Ok(InjectOutcome::Noted {
                    session_id: session,
                    seq,
                });
            }
            ServerEvent::FireDone {
                session,
                seq_range,
                result,
                assistant_text,
            } => {
                let TurnEvent::Result {
                    cost_usd,
                    stop_reason,
                } = result
                else {
                    return Err(anyhow!("non-Result in FireDone"));
                };
                return Ok(InjectOutcome::Done(FireSuccess {
                    session_id: session,
                    seq_range,
                    cost_usd,
                    stop_reason: format!("{stop_reason:?}"),
                    assistant_text,
                }));
            }
            ServerEvent::FireTimeout {
                session,
                partial_seq_range,
            } => {
                return Ok(InjectOutcome::Timeout {
                    session_id: session,
                    partial_seq_range,
                });
            }
            ServerEvent::FireError {
                session,
                code,
                message,
            }
            | ServerEvent::Error {
                session,
                code,
                message,
            } => {
                return Ok(InjectOutcome::Error {
                    session_id: session,
                    code: code.to_string(),
                    message,
                });
            }
            _ => continue,
        }
    }
}
```

(`ServerEvent::Error` and `FireError` have the same field shape — both `{ session: Option<String>, code, message }` — so the combined arm binds cleanly.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p roy-scheduler --lib roy_client::tests::inject_note_maps_to_noted`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-scheduler/src/roy_client.rs
git commit -m "feat(roy-scheduler): add roy_client::inject for the Inject command"
```

---

## Task 7: `inject_parent` subscriber switches to `inject()`

**Files:**
- Modify: `crates/roy-scheduler/src/subscribers/inject_parent.rs` (`Config` at `:21`, `execute` at `:32`, header comment at `:1-10`)
- Test: `crates/roy-scheduler/src/subscribers/inject_parent.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `crates/roy-scheduler/src/subscribers/inject_parent.rs`:

```rust
    #[tokio::test]
    async fn parses_config_with_respond() {
        let c = parse_config(r#"{"session_id":"sid","respond":true}"#).unwrap();
        assert_eq!(c.session_id, "sid");
        assert!(c.respond);
    }

    #[tokio::test]
    async fn respond_defaults_false() {
        let c = parse_config(r#"{"session_id":"sid"}"#).unwrap();
        assert!(!c.respond);
    }

    #[tokio::test]
    async fn execute_ok_when_daemon_returns_injected() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sock");
        spawn_mock(
            path.clone(),
            roy::ServerEvent::Injected {
                session: "parent-sid".into(),
                seq: 12,
            },
        )
        .await;

        let cfg = parse_config(r#"{"session_id":"parent-sid"}"#).unwrap();
        let out = execute(&path, &cfg, &fake_success()).await;
        assert_eq!(out.status, super::super::RunStatus::Ok);
    }
```

(`spawn_mock`, `fake_success`, `parse_config`, `RunStatus` all already exist in this test module — see `:86-170`.)

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p roy-scheduler --lib subscribers::inject_parent::tests::parses_config_with_respond`
Expected: FAIL — `Config` has no field `respond`.

- [ ] **Step 3: Add `respond` to `Config`**

In `crates/roy-scheduler/src/subscribers/inject_parent.rs`, update `Config` (`:21`):

```rust
#[derive(Debug, Deserialize)]
pub struct Config {
    pub session_id: String,
    #[serde(default)]
    pub prefix: Option<String>,
    #[serde(default)]
    pub respond: bool,
}
```

- [ ] **Step 4: Rewrite `execute` to call `inject`**

Replace the body of `execute` (`:32-65`) with:

```rust
pub async fn execute(
    socket_path: &Path,
    cfg: &Config,
    fire_result: &FireSuccess,
) -> super::Outcome {
    let body = match &cfg.prefix {
        Some(p) => format!("{p}{}", fire_result.assistant_text),
        None => fire_result.assistant_text.clone(),
    };

    let outcome = roy_client::inject(
        socket_path,
        cfg.session_id.clone(),
        body,
        Some(fire_result.session_id.clone()),
        cfg.respond,
        Duration::from_secs(5 * 60),
    )
    .await;

    match outcome {
        Ok(InjectOutcome::Noted { .. }) | Ok(InjectOutcome::Done(_)) => super::Outcome::ok(),
        Ok(InjectOutcome::Timeout { .. }) => {
            super::Outcome::error("parent stayed busy past 5min")
        }
        Ok(InjectOutcome::Error { code, message, .. }) => {
            super::Outcome::error(format!("{code}: {message}"))
        }
        Err(e) => super::Outcome::error(format!("roy_client: {e:#}")),
    }
}
```

Update the imports at `:19` from:

```rust
use crate::roy_client::{self, FireOutcome, FireSuccess};
```

to:

```rust
use crate::roy_client::{self, FireSuccess, InjectOutcome};
```

(Remove `FireOutcome` if now unused; the compiler will flag it. `FireTarget` import — if any — is no longer needed here.)

- [ ] **Step 5: Fix the stale header comment**

Replace the header comment (`:1-10`) with an accurate description:

```rust
//! inject_parent subscriber — drop the fire's result into the parent session.
//!
//! Default (`respond: false`): append a `Note` event referencing the child
//! session. No input lease needed, so it lands even while an interactive
//! client (roy-web) is holding the parent session's lease.
//!
//! `respond: true`: deliver the result as a real user turn the parent agent
//! answers. The daemon waits for any in-flight turn first; a session the user
//! is actively typing into may still race.
//!
//! v1 config:
//!   { "session_id": "<roy session id>", "prefix": "optional", "respond": false }
```

- [ ] **Step 6: Update the two pre-existing `execute` tests**

The old tests `execute_ok_when_daemon_returns_fire_done` and `execute_error_when_daemon_returns_fire_error` (`:129-170`) still pass unchanged (the mock now answers an `inject` request, but the wire reply types `FireDone`/`FireError` are still mapped by `inject`). Run them to confirm; no edit expected.

- [ ] **Step 7: Run the whole module's tests**

Run: `cargo test -p roy-scheduler --lib subscribers::inject_parent`
Expected: PASS (new + existing tests).

- [ ] **Step 8: Commit**

```bash
git add crates/roy-scheduler/src/subscribers/inject_parent.rs
git commit -m "feat(roy-scheduler): inject_parent uses Inject with note/respond modes"
```

---

## Task 8: Wire-protocol docs

**Files:**
- Modify: `docs/wire-protocol.md`

- [ ] **Step 1: Locate the event + command sections**

Run: `grep -n "user_prompt\|## TurnEvent\|## ClientCommand\|## ServerEvent\|\"op\":\|### Fire" docs/wire-protocol.md | head`
Expected: shows where TurnEvent variants, ClientCommands, and ServerEvents are documented.

- [ ] **Step 2: Document the `note` event**

In the TurnEvent section, after the `user_prompt` entry, add:

```markdown
- `note` — a message injected into the session out-of-band (e.g. a background
  agent's result). Not from the agent and not a user turn.
  ```json
  {"type": "note", "text": "Запуск #4: 1 + 4 = 5", "source_session": "child-sid"}
  ```
  `source_session` (nullable) links back to the session that produced it.
```

- [ ] **Step 3: Document the `inject` command + `injected` reply**

In the ClientCommand section, after `fire`, add:

```markdown
- `inject` — drop a message into a live session.
  ```json
  {"op": "inject", "session": "<sid>", "text": "...", "source_session": "<child>", "respond": false}
  ```
  - `respond: false` (default) → appends a `note` event; **no input lease
    required** (works while an interactive client holds the lease). Reply:
    `{"kind": "injected", "session": "<sid>", "seq": N}`.
  - `respond: true` → delivers `text` as a real user turn the agent answers;
    waits for any in-flight turn first. Reply: the same `fire_done` /
    `fire_timeout` / `fire_error` events as `fire`.
  - Unknown/non-live session → `error` with code `no_session`.
```

- [ ] **Step 4: Commit**

```bash
git add docs/wire-protocol.md
git commit -m "docs(wire-protocol): document note event and inject command"
```

---

## Task 9: roy-web renders the `note` event

**Files (separate repo `../roy-web`):**
- Modify: `../roy-web/src/lib/wire.ts` (`TurnEvent` at `:53`, `ClientCommand`, `ServerEvent`)
- Modify: `../roy-web/src/lib/ChatView.svelte` (group builder `switch` at `:90`, render block at `:566`)

- [ ] **Step 1: Mirror the `note` event in `wire.ts`**

In `../roy-web/src/lib/wire.ts`, add to the `TurnEvent` union (after the `user_prompt` line):

```ts
  | { type: 'note'; text: string; source_session: string | null }
```

Add to the `ClientCommand` union an `inject` op, and to the `ServerEvent` union an `injected` reply (match the existing style of those unions):

```ts
  | {
      op: 'inject';
      session: string;
      text: string;
      source_session?: string;
      respond?: boolean;
      timeout_ms?: number;
    }
```

```ts
  | { kind: 'injected'; session: string; seq: Seq }
```

- [ ] **Step 2: Add the `note` case to the group builder**

In `../roy-web/src/lib/ChatView.svelte`, in the `switch (e.event.type)` (`:90`), add after the `user_prompt` case:

```ts
        case 'note':
          flush();
          out.push({
            kind: 'note',
            text: e.event.text,
            sourceSession: e.event.source_session,
            key: `n${e.seq}`,
          });
          break;
```

- [ ] **Step 3: Render the `note` item**

In `../roy-web/src/lib/ChatView.svelte`, in the render block, add a branch before the final `{:else}` raw fallback (`:575`):

```svelte
          {:else if item.kind === 'note'}
            <article class="self-stretch rounded-lg border border-primary/30 bg-primary/5 px-4 py-2.5 text-sm">
              <div class="mb-1 text-[0.65rem] font-semibold uppercase tracking-wider text-primary/80">
                background{#if item.sourceSession} · <a class="underline" href={`/s/${item.sourceSession}`}>{item.sourceSession.slice(0, 8)}</a>{/if}
              </div>
              <pre class="m-0 whitespace-pre-wrap break-words font-sans">{item.text}</pre>
            </article>
```

(If the `out` array has an explicit TypeScript item type/union declared above the `$derived`, add the `note` shape `{ kind: 'note'; text: string; sourceSession: string | null; key: string }` to it. Find it with `grep -n "kind: 'user'\|type GroupItem\|: Item\[\]\|out:" src/lib/ChatView.svelte`.)

- [ ] **Step 4: Type-check + build**

Run: `cd ../roy-web && npm run check` (or `pnpm check` — match the repo's script in `package.json`)
Expected: no type errors related to `note` / `injected`.

- [ ] **Step 5: Commit (in the roy-web repo)**

```bash
cd ../roy-web && git add src/lib/wire.ts src/lib/ChatView.svelte && git commit -m "feat: render note (injected background) events"
```

---

## Task 10: Full workspace verification

- [ ] **Step 1: Run the CI gate locally (roy repo)**

Run:
```bash
cargo fmt --all -- --check
cargo build --workspace --all-targets
cargo test --workspace --no-fail-fast
```
Expected: all pass. (`python3` must be on PATH for the integration tests.)

- [ ] **Step 2: Manual end-to-end smoke**

```bash
# Ensure daemons up
roy status || nohup roy serve > /tmp/roy-daemon.log 2>&1 &
roy-scheduler status || nohup roy-scheduler serve > /tmp/roy-scheduler.log 2>&1 &

# Recreate a 1/min counter agent injecting into a session you have open in roy-web
roy-scheduler agents add --name counter-sum --preset claude \
  --task 'Increment N in /tmp/roy-counter.txt and print "Запуск #N".'
# (capture <agent-id>)
roy-scheduler triggers add --agent <agent-id> --cron '* * * * *'
roy-scheduler subscribers add --agent <agent-id> --kind inject_parent \
  --config '{"session_id":"<your open roy-web session>","prefix":"[counter-sum]\n\n"}'
```
Expected within ~1 min: a "background" note bubble appears in the roy-web session, linking to the child session. Confirm with:
`grep -c '"type":"note"' ~/.roy/journals/<your-session>.jsonl` → ≥ 1.

- [ ] **Step 3: Final commit (if any formatting fixups)**

```bash
git add -A && git commit -m "chore: workspace verification fixups for inject feature"
```

---

## Self-review notes (resolved)

- **Spec coverage:** Note event (T1), Inject command + Injected (T2), engine inject_note (T3), busy/inject_prompt for respond A1 (T4), daemon handler incl. the lease-bug test (T5), roy_client::inject (T6), subscriber `respond` + doc fix (T7), wire docs (T8), roy-web render (T9). All spec sections mapped.
- **Type consistency:** `inject_note(text, source_session) -> Seq`, `is_busy() -> bool`, `inject_prompt(text) -> Result<()>`, `InjectOutcome::{Noted,Done,Timeout,Error}`, `ServerEvent::Injected { session, seq }`, `ClientCommand::Inject { session, text, source_session, respond, timeout_ms }` — used identically across tasks.
- **Placeholders:** the only `<...>` markers are test-harness fixture names that must be copied from existing tests in the same file (engine/daemon test setups vary; the plan points at the exact existing tests to copy). All production code is complete.
