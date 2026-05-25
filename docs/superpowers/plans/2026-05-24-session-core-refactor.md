# Session Core Refactor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make session management robust and cheap under load — lazy journal loading for large sessions, idle-GC that never aborts an in-flight turn, a deterministic close handshake, and a non-blocking startup index.

**Architecture:** Four independent changes to the `roy` crate's core (`journal.rs`, `engine.rs`, `manager.rs`). The journal stops materializing whole-history `Vec`s on the hot paths (resume reads only the file tail; attach streams the file forward once). The engine gains an explicit "turn in progress" flag and a watch-based "actor closed" signal. The manager's idle sweep honors the turn flag, `close` awaits real termination, and startup indexing reads metadata concurrently.

**Tech Stack:** Rust 2021, tokio (`fs`, `sync::watch`, `time`, `task::JoinSet`), `async-stream`, `tokio-stream`. Tests use the hermetic `tests/scripts/fake-acp-agent.py` (no real agent CLI needed).

---

## Background: current behavior being fixed

- `Journal::resume` (`crates/roy/src/journal.rs:86`) reads **every** line of the journal to recompute `next_seq` and hydrate the in-memory ring → O(history). `SessionManager::resume_all` multiplies this across all sessions at daemon startup.
- `SessionEngine::attach` (`crates/roy/src/engine.rs:345`) calls `replay_from(0)` which collects the **entire** history into one `Vec<JournalEntry>` before the stream starts → a large session freezes the client on open and spikes memory.
- `SessionManager::sweep_idle` (`crates/roy/src/manager.rs:220`) decides solely on `last_activity`. A turn that goes quiet longer than the threshold (long tool call, model thinking) gets closed mid-flight, aborting work.
- Mid-turn `Cmd::Close` is consumed inside `drive_turn` and turns into a bare `return`, so `run_actor`'s outer loop keeps waiting for the next command — `handle.close()` is never called and the child is never wound down. (Latent bug surfaced while designing the close handshake.)
- `SessionManager::close` is fire-and-forget: it removes the engine from the registry but does not wait for the actor to drop the journal's append handle, so a `close` immediately followed by `resume` can briefly have two append handles on the same file.
- `SessionManager::index_existing_sessions` (`crates/roy/src/manager.rs:285`) reads each `*.meta.json` sequentially (`await` inside the loop) → startup latency grows linearly with session count.

## File structure

- `crates/roy/src/journal.rs` — add `stream_disk` (forward streaming reader) and `read_trailing_lines` (reverse tail reader); rewrite `Journal::resume` to use the tail reader.
- `crates/roy/src/engine.rs` — add `turn_active: AtomicBool` and `closed_rx: watch::Receiver<bool>` to `SessionEngine`; add `is_turn_active`, `wait_closed`; make `drive_turn` return a `TurnOutcome`; rewrite `attach` + `build_attach_stream` to stream lazily.
- `crates/roy/src/manager.rs` — `sweep_idle` skips active turns; `close` awaits `wait_closed` (bounded); `index_existing_sessions` reads metadata concurrently.

Task order is chosen so each task is self-contained and committable, smallest/lowest-risk first:

1. Task 1 — concurrent startup index (`manager.rs` only).
2. Task 2 — idle-GC turn guard (`engine.rs` + `manager.rs`).
3. Task 3 — mid-turn close fix + termination handshake (`engine.rs` + `manager.rs`).
4. Task 4 — resume tail-read (`journal.rs`).
5. Task 5 — lazy attach streaming (`journal.rs` + `engine.rs`).

Run the full gate after each task: `cargo fmt --all -- --check && cargo build --workspace --all-targets && cargo test --workspace --no-fail-fast`.

---

## Task 1: Concurrent startup index

**Files:**
- Modify: `crates/roy/src/manager.rs:285-319` (`index_existing_sessions`)
- Test: `crates/roy/src/manager.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/roy/src/manager.rs`:

```rust
#[tokio::test]
async fn index_existing_sessions_handles_many_concurrently() {
    let dir = tmp_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let mgr = new_mgr(&dir);
    let project = mgr.projects().create_project("bulk").unwrap();
    let pid = project.id.clone();

    // 200 sessions all pointing at the same project.
    let mut expected = Vec::new();
    for i in 0..200u32 {
        let sid = format!("bulk-sid-{i}");
        let meta = crate::session_meta::SessionMetadata {
            session_id: sid.clone(),
            agent: "fake".into(),
            cwd: project.path.clone(),
            project_id: Some(pid.clone()),
            model: None,
            permission: None,
            resume_cursor: None,
            tags: Default::default(),
        };
        crate::session_meta::write_metadata(&dir, &meta).await.unwrap();
        std::fs::write(dir.join(format!("{sid}.jsonl")), "").unwrap();
        expected.push(sid);
    }

    mgr.index_existing_sessions().await.unwrap();

    let mut got = mgr.projects().sessions_in(&pid);
    got.sort();
    expected.sort();
    assert_eq!(got, expected, "every session must be registered exactly once");

    let _ = std::fs::remove_dir_all(&dir);
}
```

- [ ] **Step 2: Run test to verify it passes against the OLD implementation, then we refactor without breaking it**

Run: `cargo test -p roy index_existing_sessions_handles_many_concurrently -- --nocapture`
Expected: PASS (the sequential implementation is already correct; this test is the regression guard for the concurrent rewrite).

- [ ] **Step 3: Rewrite `index_existing_sessions` to read metadata concurrently**

Replace the body of `index_existing_sessions` (`crates/roy/src/manager.rs:285-319`) with:

```rust
    pub async fn index_existing_sessions(&self) -> Result<()> {
        if !tokio::fs::try_exists(&self.journal_dir)
            .await
            .map_err(RoyError::Io)?
        {
            return Ok(());
        }

        // 1. Cheap dirent scan: collect the session ids that have a meta file.
        let mut sids = Vec::new();
        let mut entries = tokio::fs::read_dir(&self.journal_dir)
            .await
            .map_err(RoyError::Io)?;
        while let Some(entry) = entries.next_entry().await.map_err(RoyError::Io)? {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if let Some(sid) = name.strip_suffix(".meta.json") {
                sids.push(sid.to_string());
            }
        }

        // 2. Read the metadata files concurrently, bounded so we never exhaust
        //    file descriptors on a large archive. Registration touches
        //    `self.projects` (not `'static`), so it stays on this task — the
        //    only thing fanned out is the IO.
        let permits = Arc::new(tokio::sync::Semaphore::new(64));
        let mut set = tokio::task::JoinSet::new();
        for sid in sids {
            let dir = self.journal_dir.clone();
            let permits = Arc::clone(&permits);
            set.spawn(async move {
                let _permit = permits.acquire_owned().await.expect("semaphore open");
                let meta = read_metadata(&dir, &sid).await;
                (sid, meta)
            });
        }

        while let Some(joined) = set.join_next().await {
            let (sid, meta) = match joined {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!(error = %e, "skip indexing: metadata read task panicked");
                    continue;
                }
            };
            let meta = match meta {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(session = %sid, error = %e, "skip indexing: meta unreadable");
                    continue;
                }
            };
            if let Some(ref pid) = meta.project_id {
                match self.projects.ensure_project(pid) {
                    Ok(verified_pid) => self.projects.register_session(&verified_pid, &sid),
                    Err(e) => {
                        tracing::warn!(session = %sid, project_id = %pid, error = %e,
                            "skip indexing: project not in registry");
                    }
                }
            }
        }
        Ok(())
    }
```

- [ ] **Step 4: Run the new + existing index tests**

Run: `cargo test -p roy index_existing_sessions -- --nocapture`
Expected: PASS for both `index_existing_sessions_rebuilds_project_membership` and `index_existing_sessions_handles_many_concurrently`.

- [ ] **Step 5: Run the full gate**

Run: `cargo fmt --all -- --check && cargo build --workspace --all-targets && cargo test --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/roy/src/manager.rs
git commit -m "perf(roy): read session metadata concurrently on startup index"
```

---

## Task 2: Idle-GC turn guard

**Files:**
- Modify: `crates/roy/src/engine.rs` (struct fields, `start`, `run_actor`, new accessor)
- Modify: `crates/roy/src/manager.rs:220-242` (`sweep_idle`)
- Test: `crates/roy/src/manager.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Add the `turn_active` flag to `SessionEngine`**

In `crates/roy/src/engine.rs`, extend the imports near the top (the file already has `use std::sync::{Arc, Mutex as StdMutex};`):

```rust
use std::sync::atomic::{AtomicBool, Ordering};
```

Add the field to the `SessionEngine` struct (after `last_activity`):

```rust
    /// True while `drive_turn` is running a prompt. Distinct from
    /// `last_activity`: a turn can stay active while producing no journal
    /// events (long tool call, model thinking), and must not be swept as idle.
    turn_active: AtomicBool,
```

In `SessionEngine::start`, initialize it in the `Arc::new(Self { ... })` literal (alongside `last_activity: StdMutex::new(Instant::now()),`):

```rust
            turn_active: AtomicBool::new(false),
```

Add the public accessor (place it next to `last_activity`):

```rust
    /// Whether a prompt turn is currently being driven. Used by
    /// `SessionManager::sweep_idle` so idle-GC never aborts an in-flight turn.
    pub fn is_turn_active(&self) -> bool {
        self.turn_active.load(Ordering::SeqCst)
    }
```

- [ ] **Step 2: Set/clear the flag around `drive_turn` in `run_actor`**

In `crates/roy/src/engine.rs`, in the `Cmd::Prompt(text)` arm of `run_actor`, wrap the `drive_turn` call (currently `drive_turn(&engine, handle.as_mut(), &text, &mut input_rx).await;`):

```rust
                engine.turn_active.store(true, Ordering::SeqCst);
                drive_turn(&engine, handle.as_mut(), &text, &mut input_rx).await;
                engine.turn_active.store(false, Ordering::SeqCst);
```

- [ ] **Step 3: Make `sweep_idle` skip active turns**

In `crates/roy/src/manager.rs`, change the filter inside `sweep_idle` (`crates/roy/src/manager.rs:224-233`) from:

```rust
                .filter_map(|(id, engine)| {
                    if now.duration_since(engine.last_activity()) >= threshold {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
```

to:

```rust
                .filter_map(|(id, engine)| {
                    if !engine.is_turn_active()
                        && now.duration_since(engine.last_activity()) >= threshold
                    {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
```

- [ ] **Step 4: Write the failing test**

This test needs the fake agent in `--cancellable` mode (streams one chunk, then waits for cancel — i.e. an active-but-quiet turn). Add a second test factory and the test to the `tests` module in `crates/roy/src/manager.rs`:

```rust
    /// Like `FakeFactory` but launches the fake agent in `--cancellable` mode:
    /// it emits one chunk then blocks until `session/cancel`, modelling a turn
    /// that is active but produces no further journal events.
    struct CancellableFactory;
    impl TransportFactory for CancellableFactory {
        fn build(
            &self,
            _agent: AgentPreset,
            _model: Option<&str>,
            _permission: Option<&str>,
        ) -> Result<Arc<dyn Transport>> {
            Ok(Arc::new(AcpTransport::new(AcpConfig {
                command: "python3".to_string(),
                args: vec![
                    "tests/scripts/fake-acp-agent.py".to_string(),
                    "--cancellable".to_string(),
                ],
                mode_id: Some("yolo".to_string()),
                permission_policy: PermissionPolicy::AllowAll,
                open_timeout: Duration::from_secs(5),
                env_remove: Vec::new(),
            })))
        }
    }

    #[tokio::test]
    async fn sweep_idle_skips_active_turn() {
        let dir = tmp_dir();
        let mgr = SessionManager::new(
            dir.clone(),
            dir.join("workspace"),
            Arc::new(CancellableFactory),
        )
        .expect("registry load");

        let engine = mgr
            .spawn(orphan_cfg(AgentPreset::Opencode), 256, 1024)
            .await
            .unwrap();
        let id = engine.id().to_string();

        // Drive a turn: the agent streams "working" then blocks for cancel.
        let lease = engine.try_acquire_input().expect("lease");
        lease.send("hi").unwrap();

        // Wait until the turn is actually in flight (flag set by run_actor).
        for _ in 0..100 {
            if engine.is_turn_active() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(engine.is_turn_active(), "turn should be active");

        // Let last_activity go stale relative to the threshold.
        tokio::time::sleep(Duration::from_millis(120)).await;

        // Active turn => NOT swept even though last_activity is older than the
        // threshold.
        let closed = mgr.sweep_idle(Duration::from_millis(100)).await;
        assert!(closed.is_empty(), "active turn must not be swept: {closed:?}");
        assert_eq!(mgr.list().await, vec![id.clone()]);

        // End the turn, then it becomes eligible.
        engine.cancel_turn().unwrap();
        for _ in 0..100 {
            if !engine.is_turn_active() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
        let closed = mgr.sweep_idle(Duration::from_millis(100)).await;
        assert_eq!(closed, vec![id.clone()], "quiet, inactive turn must be swept");

        drop(lease);
        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 5: Run the test**

Run: `cargo test -p roy sweep_idle -- --nocapture`
Expected: PASS for both `sweep_idle_closes_quiet_sessions` and `sweep_idle_skips_active_turn`.

- [ ] **Step 6: Run the full gate**

Run: `cargo fmt --all -- --check && cargo build --workspace --all-targets && cargo test --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/roy/src/engine.rs crates/roy/src/manager.rs
git commit -m "fix(roy): idle-GC no longer aborts an in-flight turn"
```

---

## Task 3: Mid-turn close fix + termination handshake

**Files:**
- Modify: `crates/roy/src/engine.rs` (`TurnOutcome` enum, struct field, `start`, `run_actor`, `drive_turn`, `wait_closed`)
- Modify: `crates/roy/src/manager.rs:246-255` (`close`)
- Test: `crates/roy/src/manager.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Add the closed-signal field and `wait_closed`**

In `crates/roy/src/engine.rs`, add to imports (the file already has `use tokio::sync::{broadcast, mpsc};`):

```rust
use tokio::sync::watch;
```

Add a control-flow enum near the `Cmd` enum:

```rust
/// What `drive_turn` tells `run_actor` to do after a turn ends.
enum TurnOutcome {
    /// Keep serving the session.
    Continue,
    /// A `Close` (or input channel hang-up) arrived mid-turn — wind the
    /// session down.
    Close,
}
```

Add the field to `SessionEngine` (after `turn_active`):

```rust
    /// Flips to `true` once the actor task has fully wound down (handle closed,
    /// child reaped). `wait_closed` awaits it so a `close` followed by a
    /// `resume` never races two append handles on the same journal file.
    closed_rx: watch::Receiver<bool>,
```

Add the accessor near `close`:

```rust
    /// Resolve once the actor task has fully terminated. Cheap to call from
    /// multiple awaiters; late callers see the already-closed state.
    pub async fn wait_closed(&self) {
        let mut rx = self.closed_rx.clone();
        let _ = rx.wait_for(|&closed| closed).await;
    }
```

- [ ] **Step 2: Wire the watch channel through `start` and `run_actor`**

In `SessionEngine::start` (`crates/roy/src/engine.rs`), just after the existing channel setup
(`let (input_tx, input_rx) = mpsc::unbounded_channel();`), add:

```rust
        let (closed_tx, closed_rx) = watch::channel(false);
```

Add `closed_rx,` to the `Arc::new(Self { ... })` literal (next to `turn_active`).

Change the actor spawn from:

```rust
        let engine_for_actor = Arc::clone(&engine);
        tokio::spawn(run_actor(engine_for_actor, handle, input_rx));
```

to:

```rust
        let engine_for_actor = Arc::clone(&engine);
        tokio::spawn(run_actor(engine_for_actor, handle, input_rx, closed_tx));
```

- [ ] **Step 3: Make `run_actor` honor `TurnOutcome` and signal completion**

Replace `run_actor` (`crates/roy/src/engine.rs:434-483`) with:

```rust
async fn run_actor(
    engine: Arc<SessionEngine>,
    mut handle: Box<dyn Handle>,
    mut input_rx: mpsc::UnboundedReceiver<Cmd>,
    closed_tx: watch::Sender<bool>,
) {
    while let Some(cmd) = input_rx.recv().await {
        match cmd {
            Cmd::Prompt(text) => {
                engine.touch_activity();
                // Journal the user's prompt before driving the turn. Agents
                // don't echo user input over ACP, so without this step a
                // refresh / late attach can never reconstruct the user side
                // of the conversation.
                if let Err(e) = publish(&engine, TurnEvent::UserPrompt { text: text.clone() }).await
                {
                    tracing::error!(
                        session = %engine.session_id,
                        error = %e,
                        "failed to journal user prompt; turn still dispatched",
                    );
                }
                engine.turn_active.store(true, Ordering::SeqCst);
                let outcome = drive_turn(&engine, handle.as_mut(), &text, &mut input_rx).await;
                engine.turn_active.store(false, Ordering::SeqCst);
                if let Some(cursor) = handle.resume_cursor() {
                    *engine.resume_cursor.lock().unwrap() = Some(cursor);
                    // Non-fatal: session keeps running, but a stale cursor
                    // on disk means a future Resume reconnects to the wrong
                    // agent-side session. Surface it.
                    if let Err(e) = engine.persist_metadata().await {
                        tracing::warn!(
                            session = %engine.session_id,
                            error = %e,
                            "failed to persist session metadata after turn",
                        );
                    }
                }
                if matches!(outcome, TurnOutcome::Close) {
                    break;
                }
            }
            // Cancel outside an active turn is a no-op; the turn-driving loop
            // is the only place a cancel actually means something.
            Cmd::Cancel => {}
            Cmd::Close => break,
        }
    }
    if let Err(e) = handle.close().await {
        tracing::warn!(
            session = %engine.session_id,
            error = %e,
            "transport close failed; child process may be left in unknown state",
        );
    }
    // Announce full termination so `wait_closed` (and thus `SessionManager::
    // close`) can return only once the handle is down and the child reaped.
    let _ = closed_tx.send(true);
}
```

Note: this folds in the Task 2 `turn_active` store/clear (now reading `outcome`).

- [ ] **Step 4: Make `drive_turn` return `TurnOutcome`**

Replace `drive_turn` (`crates/roy/src/engine.rs:485-552`) with:

```rust
async fn drive_turn(
    engine: &SessionEngine,
    handle: &mut dyn Handle,
    text: &str,
    input_rx: &mut mpsc::UnboundedReceiver<Cmd>,
) -> TurnOutcome {
    let (mut stream, cancel) = match handle.send(text).await {
        Ok(pair) => pair,
        Err(e) => {
            // The transport refused the turn; synthesise a terminal Result
            // so attach subscribers still see a turn boundary. The session
            // stays alive.
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
            return TurnOutcome::Continue;
        }
    };
    // Hold the cancel signal in an Option so the Cancel arm can drop it once.
    // Drop = ACP `session/cancel`; the stream stays open and still yields the
    // terminal `Result`, so we stay in the loop after a cancel.
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
                // A Close (or input hang-up) mid-turn must wind the session
                // down — propagate it instead of silently returning, which
                // would leave the actor waiting for a command that never comes.
                Some(Cmd::Close) | None => return TurnOutcome::Close,
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
    TurnOutcome::Continue
}
```

- [ ] **Step 5: Make `SessionManager::close` await termination (bounded)**

Replace `close` (`crates/roy/src/manager.rs:246-255`) with:

```rust
    pub async fn close(&self, id: &str) -> Result<()> {
        let engine = self
            .sessions
            .write()
            .await
            .remove(id)
            .ok_or_else(|| RoyError::Protocol(format!("no such session: {id}")))?;
        tracing::info!(session = %id, "closing session");
        engine.close()?;
        // Wait (bounded) for the actor to finish tearing the handle down so a
        // subsequent resume opens a freed journal file instead of racing the
        // old append handle. The cap guards against a child that refuses to die.
        if tokio::time::timeout(std::time::Duration::from_secs(5), engine.wait_closed())
            .await
            .is_err()
        {
            tracing::warn!(session = %id, "close: actor did not finish within 5s");
        }
        Ok(())
    }
```

- [ ] **Step 6: Write the failing test**

Add to the `tests` module in `crates/roy/src/manager.rs` (depends on `CancellableFactory` from Task 2):

```rust
    #[tokio::test]
    async fn close_mid_turn_terminates_actor() {
        let dir = tmp_dir();
        let mgr = SessionManager::new(
            dir.clone(),
            dir.join("workspace"),
            Arc::new(CancellableFactory),
        )
        .expect("registry load");

        let engine = mgr
            .spawn(orphan_cfg(AgentPreset::Opencode), 256, 1024)
            .await
            .unwrap();
        let id = engine.id().to_string();

        // Start a turn that blocks (cancellable agent waits for cancel).
        let lease = engine.try_acquire_input().expect("lease");
        lease.send("hi").unwrap();
        for _ in 0..100 {
            if engine.is_turn_active() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(engine.is_turn_active(), "turn should be active before close");

        // Keep a handle to the engine so we can observe termination even after
        // the manager drops its registry reference.
        let engine_observer = Arc::clone(&engine);
        drop(lease);

        // Close mid-turn. Before the fix the actor would hang waiting for a
        // command that never arrives and `wait_closed` would never resolve.
        mgr.close(&id).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), engine_observer.wait_closed())
                .await
                .is_ok(),
            "actor must fully terminate after a mid-turn close",
        );
        assert!(mgr.list().await.is_empty());

        // The journal handle is freed, so resume succeeds cleanly.
        mgr.resume(&id, 256, 1024).await.unwrap();
        mgr.close(&id).await.unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 7: Run the test**

Run: `cargo test -p roy close_mid_turn_terminates_actor -- --nocapture`
Expected: PASS.

- [ ] **Step 8: Run the full gate**

Run: `cargo fmt --all -- --check && cargo build --workspace --all-targets && cargo test --workspace --no-fail-fast`
Expected: PASS. (Pay attention to `resume_all_brings_back_closed_sessions` and `registry_lifecycle` — both exercise `close` and must still pass; `close` now waits for real termination.)

- [ ] **Step 9: Commit**

```bash
git add crates/roy/src/engine.rs crates/roy/src/manager.rs
git commit -m "fix(roy): propagate mid-turn close and await actor termination"
```

---

## Task 4: Resume tail-read

**Files:**
- Modify: `crates/roy/src/journal.rs` (imports, new `read_trailing_lines`, rewrite `Journal::resume`)
- Test: `crates/roy/src/journal.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Add tail-reader imports**

In `crates/roy/src/journal.rs`, extend the tokio IO import line. Change:

```rust
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
```

to:

```rust
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader, SeekFrom};
```

- [ ] **Step 2: Add the reverse tail reader**

Add this free function to `crates/roy/src/journal.rs` (place it just after `parse_entry_line`):

```rust
/// Read up to `max_lines` trailing non-empty lines from `path`, returned in
/// file order (oldest → newest). Reads backward in fixed chunks so the cost is
/// O(tail), not O(file) — this is what keeps `Journal::resume` cheap on a long
/// session. Over-reads by one newline so the first kept line is guaranteed
/// complete (a partial leading line is dropped when we trim to `max_lines`).
async fn read_trailing_lines(path: &Path, max_lines: usize) -> Result<Vec<String>> {
    if max_lines == 0 {
        return Ok(Vec::new());
    }
    let mut file = File::open(path).await.map_err(RoyError::Io)?;
    let len = file.metadata().await.map_err(RoyError::Io)?.len();
    if len == 0 {
        return Ok(Vec::new());
    }

    const CHUNK: u64 = 64 * 1024;
    let mut pos = len;
    let mut buf: Vec<u8> = Vec::new();
    // Read backward until we have one more newline than requested (so the
    // earliest line in `buf` is complete) or we reach the start of the file.
    while pos > 0 {
        let read_size = CHUNK.min(pos);
        pos -= read_size;
        file.seek(SeekFrom::Start(pos)).await.map_err(RoyError::Io)?;
        let mut chunk = vec![0u8; read_size as usize];
        file.read_exact(&mut chunk).await.map_err(RoyError::Io)?;
        chunk.extend_from_slice(&buf);
        buf = chunk;
        if buf.iter().filter(|&&b| b == b'\n').count() > max_lines {
            break;
        }
    }

    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    if lines.len() > max_lines {
        lines = lines.split_off(lines.len() - max_lines);
    }
    Ok(lines)
}
```

- [ ] **Step 3: Rewrite `Journal::resume` to use the tail reader**

Replace `Journal::resume` (`crates/roy/src/journal.rs:86-126`) with:

```rust
    /// Open an existing journal in append mode. Recomputes `next_seq` from the
    /// last entry and hydrates the in-memory ring with the most recent
    /// `mem_capacity` entries — both via a backward tail read, so resume cost
    /// is O(tail) regardless of total history length. Used by
    /// `SessionManager::resume`.
    pub async fn resume(dir: &Path, session_id: &str, mem_capacity: usize) -> Result<Self> {
        let path = dir.join(format!("{session_id}.jsonl"));
        if !tokio::fs::try_exists(&path).await.map_err(RoyError::Io)? {
            return Err(RoyError::Protocol(format!(
                "no journal at {}",
                path.display()
            )));
        }

        let tail = read_trailing_lines(&path, mem_capacity).await?;
        let mut mem: VecDeque<JournalEntry> = VecDeque::with_capacity(mem_capacity);
        let mut next_seq: Seq = 0;
        for line in &tail {
            // A corrupt line within the resumed window fails loudly: a bad
            // `next_seq` would let the next append overwrite valid entries.
            let entry = parse_entry_line(line)?;
            next_seq = entry.seq + 1;
            mem.push_back(entry);
        }

        let writer = OpenOptions::new()
            .write(true)
            .append(true)
            .open(&path)
            .await
            .map_err(RoyError::Io)?;
        Ok(Self {
            path,
            inner: Mutex::new(JournalInner {
                writer,
                mem,
                mem_capacity,
                next_seq,
            }),
        })
    }
```

- [ ] **Step 4: Write the failing test**

Add to the `tests` module in `crates/roy/src/journal.rs`:

```rust
    #[tokio::test]
    async fn resume_tail_read_recovers_seq_with_small_window() {
        let dir = tmpdir();
        let session = "s-tail";
        // Write 5 entries.
        {
            let j = Journal::open(&dir.0, session, 2).await.unwrap();
            for i in 0..5u32 {
                j.append(TurnEvent::AssistantText {
                    text: format!("e{i}"),
                })
                .await
                .unwrap();
            }
        }
        // Resume with a 2-entry window: tail read sees only the last lines but
        // must still recover next_seq == 5 from the final entry.
        let j2 = Journal::resume(&dir.0, session, 2).await.unwrap();
        let seq = j2
            .append(TurnEvent::AssistantText { text: "after".into() })
            .await
            .unwrap();
        assert_eq!(seq, 5, "next_seq must be recovered from the last on-disk entry");

        // replay_from(0) falls back to disk for the prefix outside the window.
        let all = j2.replay_from(0).await.unwrap();
        assert_eq!(all.len(), 6);
        for (i, entry) in all.iter().enumerate() {
            assert_eq!(entry.seq, i as Seq);
        }
        match &all[5].event {
            TurnEvent::AssistantText { text } => assert_eq!(text, "after"),
            other => panic!("expected AssistantText, got {other:?}"),
        }
    }
```

- [ ] **Step 5: Run the journal tests**

Run: `cargo test -p roy --lib journal -- --nocapture`
Expected: PASS for `resume_tail_read_recovers_seq_with_small_window`, the existing `resume_continues_next_seq_from_disk`, and `resume_errors_on_corrupt_jsonl_line` (its 3-line file fits inside the window, so the corrupt middle line still triggers a `Protocol` error).

- [ ] **Step 6: Run the full gate**

Run: `cargo fmt --all -- --check && cargo build --workspace --all-targets && cargo test --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/roy/src/journal.rs
git commit -m "perf(roy): resume reads only the journal tail instead of the whole file"
```

---

## Task 5: Lazy attach streaming

**Files:**
- Modify: `crates/roy/src/journal.rs` (imports, new public `stream_disk`)
- Modify: `crates/roy/src/engine.rs` (`attach`, `build_attach_stream`)
- Test: `crates/roy/src/journal.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Add a forward streaming disk reader to the journal**

In `crates/roy/src/journal.rs`, add the `Stream` import next to the existing `serde` imports:

```rust
use tokio_stream::Stream;
```

Add this public function (place it after `read_trailing_lines`):

```rust
/// Stream journal entries with `seq >= from_seq` straight off disk in a single
/// forward pass. Constant memory regardless of history size — `BufReader` pulls
/// the file in chunks and each entry is yielded as it is parsed. The stream's
/// `Err` item carries a parse / IO failure so the consumer can stop cleanly.
/// This is the lazy building block behind `SessionEngine::attach`.
pub fn stream_disk(
    path: PathBuf,
    from_seq: Seq,
) -> impl Stream<Item = Result<JournalEntry>> + Send {
    async_stream::try_stream! {
        let file = File::open(&path).await.map_err(RoyError::Io)?;
        let mut lines = BufReader::new(file).lines();
        while let Some(line) = lines.next_line().await.map_err(RoyError::Io)? {
            if line.trim().is_empty() {
                continue;
            }
            let entry = parse_entry_line(&line)?;
            if entry.seq < from_seq {
                continue;
            }
            yield entry;
        }
    }
}
```

- [ ] **Step 2: Write the failing test for `stream_disk`**

Add to the `tests` module in `crates/roy/src/journal.rs`:

```rust
    #[tokio::test]
    async fn stream_disk_yields_in_order_from_seq() {
        use tokio_stream::StreamExt;
        let dir = tmpdir();
        let session = "s-stream";
        {
            let j = Journal::open(&dir.0, session, 4).await.unwrap();
            for i in 0..6u32 {
                j.append(TurnEvent::AssistantText {
                    text: format!("e{i}"),
                })
                .await
                .unwrap();
            }
        }
        let path = dir.0.join(format!("{session}.jsonl"));

        // from_seq = 0 streams everything in order.
        let s = stream_disk(path.clone(), 0);
        tokio::pin!(s);
        let mut seqs = Vec::new();
        while let Some(item) = s.next().await {
            seqs.push(item.unwrap().seq);
        }
        assert_eq!(seqs, vec![0, 1, 2, 3, 4, 5]);

        // from_seq = 3 skips the prefix.
        let s = stream_disk(path, 3);
        tokio::pin!(s);
        let mut seqs = Vec::new();
        while let Some(item) = s.next().await {
            seqs.push(item.unwrap().seq);
        }
        assert_eq!(seqs, vec![3, 4, 5]);
    }
```

- [ ] **Step 3: Run the test to verify it passes**

Run: `cargo test -p roy --lib stream_disk_yields_in_order_from_seq -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Rewrite `SessionEngine::attach` to stream lazily**

Replace `attach` (`crates/roy/src/engine.rs:343-355`) with:

```rust
    /// Subscribe an observer. Race-free: subscribes to the live broadcast
    /// first, then streams the journal from disk forward (lazily, constant
    /// memory), then splices into the live tail. A large history no longer
    /// materializes into one `Vec` before the stream starts.
    pub async fn attach(&self, from_seq: Option<Seq>) -> Result<Attach> {
        let rx = self.broadcast_tx.subscribe();
        let from = from_seq.unwrap_or(0);
        // Snapshot the boundary: the next seq the journal will assign == the
        // position just past the last entry that exists right now.
        let seq_at_attach = self.journal.next_seq().await;
        let path = self.journal.path().to_path_buf();
        let stream = build_attach_stream(path, from, rx);
        Ok(Attach {
            seq_at_attach,
            stream,
        })
    }
```

- [ ] **Step 5: Rewrite `build_attach_stream` to read disk lazily**

Replace `build_attach_stream` (`crates/roy/src/engine.rs:563-612`) with:

```rust
/// Stitch a lazy disk replay + the live broadcast into one ordered, dedup'd
/// stream. Phase 1 streams the journal file forward once (constant memory).
/// Phase 2 serves the live broadcast; on `Lagged` it re-streams the journal
/// from the last yielded seq + 1, so the agent never blocks for a slow
/// subscriber and no entry is lost or duplicated.
fn build_attach_stream(
    path: PathBuf,
    from_seq: Seq,
    rx: broadcast::Receiver<JournalEntry>,
) -> Pin<Box<dyn Stream<Item = JournalEntry> + Send>> {
    Box::pin(async_stream::stream! {
        let mut expected_next = from_seq;
        let mut last_yielded: Option<Seq> = None;

        // Phase 1: lazy forward disk replay.
        {
            let disk = crate::journal::stream_disk(path.clone(), from_seq);
            tokio::pin!(disk);
            while let Some(item) = disk.next().await {
                match item {
                    Ok(entry) => {
                        if entry.seq < expected_next {
                            continue;
                        }
                        expected_next = entry.seq + 1;
                        last_yielded = Some(entry.seq);
                        yield entry;
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "attach: disk replay failed");
                        break;
                    }
                }
            }
        }

        // Phase 2: live broadcast (rx was subscribed before phase 1, so no gap).
        let mut rx = rx;
        loop {
            match rx.recv().await {
                Ok(entry) => {
                    if entry.seq < expected_next {
                        continue; // dedup against disk overlap
                    }
                    expected_next = entry.seq + 1;
                    last_yielded = Some(entry.seq);
                    yield entry;
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let from = last_yielded.map(|s| s + 1).unwrap_or(expected_next);
                    let catchup = crate::journal::stream_disk(path.clone(), from);
                    tokio::pin!(catchup);
                    while let Some(item) = catchup.next().await {
                        match item {
                            Ok(entry) => {
                                if entry.seq < expected_next {
                                    continue;
                                }
                                expected_next = entry.seq + 1;
                                last_yielded = Some(entry.seq);
                                yield entry;
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "attach: lagged catch-up failed");
                                break;
                            }
                        }
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}
```

Note: `build_attach_stream` no longer takes `replay: Vec<JournalEntry>` or `journal: Arc<Journal>` — it reads the journal file by path. The `JournalEntry` import in `engine.rs` is still used elsewhere (broadcast channel, `Attach`), so leave imports as-is. If `cargo build` reports an unused import after this change, remove only what it names.

- [ ] **Step 6: Verify `seq_at_attach` consumers still hold**

The live attach path (`crates/roy/src/daemon.rs:1271-1290`) forwards `attach.seq_at_attach` verbatim into `ServerEvent::Attached`, so the new snapshot value (`journal.next_seq()`) is wire-compatible. The archive path (`crates/roy/src/daemon.rs:1055`) computes its own `seq_at_attach` from `entries.last()` and is untouched. No code change needed — this step is a read-only confirmation while reviewing.

Run: `grep -rn "seq_at_attach" crates/`
Expected: only the two daemon sites above plus the `engine.rs` definition; none compute it from the now-removed `replay` Vec.

- [ ] **Step 7: Run the attach-stressing daemon tests**

The daemon tests use the fake agent's `--flood N` flag to push many events through the broadcast/journal pipeline and attach against them — the regression guard for this change.

Run: `cargo test -p roy --lib daemon -- --nocapture`
Expected: PASS.

- [ ] **Step 8: Run the full gate**

Run: `cargo fmt --all -- --check && cargo build --workspace --all-targets && cargo test --workspace --no-fail-fast`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/roy/src/journal.rs crates/roy/src/engine.rs
git commit -m "perf(roy): stream attach replay lazily from disk"
```

---

## Final verification

- [ ] **Step 1: Run the full CI gate one more time**

Run: `cargo fmt --all -- --check && cargo build --workspace --all-targets && cargo test --workspace --no-fail-fast`
Expected: PASS (matches `.github/workflows/ci.yml`).

- [ ] **Step 2: Confirm the two large-session paths are now lazy**

Read `crates/roy/src/journal.rs` and confirm neither `Journal::resume` nor `stream_disk` reads the whole file into a `Vec` (resume uses `read_trailing_lines`; attach uses `stream_disk`). `replay_from` is intentionally left as-is — it serves poll-style readers (`snapshot`, `wait_for_result`) that pass a recent `from_seq`, so its reads stay bounded.

---

## Self-review notes (for the plan author)

- **Spec coverage:** Task 1 = concurrent index (finding #4); Task 2 = idle turn guard (finding #2); Task 3 = mid-turn close + termination handshake (finding #3, plus the latent mid-turn-Close hang found during design); Task 4 = resume tail-read (finding #1a); Task 5 = lazy attach (finding #1b). All four agreed-scope items plus lazy attach are covered.
- **Type consistency:** `TurnOutcome` defined in Task 3 is the return type used by `drive_turn` in the same task. `turn_active`/`is_turn_active` introduced in Task 2 are reused (not redefined) in Task 3's `run_actor`. `CancellableFactory` defined in Task 2 is reused by Task 3's test. `stream_disk(path, from_seq)` defined in Task 5 step 1 is called with the same signature in step 5.
- **Left intentionally unchanged:** `Journal::replay_from` and the in-memory ring (bounded readers still want them); no journal rotation/compaction, no directory sharding (YAGNI at current scale per the repo's "no overengineering" bar).
