# Spawn/Resume early-ack Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add two `ServerEvent` ack variants (`Spawning`, `Resuming`) emitted at the entry of the `Spawn` / `Resume` daemon handlers, so clients can render a loading indicator during the slow agent-process startup phase (notably ACP `initialize` / `session/load`).

**Architecture:** Pure additive wire-protocol change. Two new variants in `ServerEvent`. Each new variant is emitted as the first statement of its respective handler in `daemon.rs`, before any I/O or validation, via the existing per-connection `event_tx` broadcaster. No new abstractions; no changes to `SessionManager`, `SessionEngine`, `Journal`, or `Transport`.

**Tech Stack:** Rust 2021, `tokio`, `serde`, `cargo test`. Tests use the in-tree fake ACP agent (`crates/roy/tests/scripts/fake-acp-agent.py`) — no real CLIs required.

---

## File Structure

- **Modify:** `crates/roy/src/control.rs` — add `Spawning` and `Resuming` variants to `ServerEvent` + roundtrip tests in the inline `mod tests`.
- **Modify:** `crates/roy/src/daemon.rs` — emit ack at entry of `handle_spawn` and `handle_resume`; update existing tests to assert ack ordering.
- **Modify:** `docs/wire-protocol.md` — add the two new events to the `ServerEvent` table and a paragraph describing the ack-then-terminal ordering.

Spec reference: `docs/superpowers/specs/2026-05-24-spawn-resume-loading-ack-design.md`.

---

## Task 1 — Add `Spawning` / `Resuming` variants to `ServerEvent`

**Files:**
- Modify: `crates/roy/src/control.rs` (insert variants near the existing `Resumed` / `Spawned` variants; add roundtrip tests in `mod tests`)

- [ ] **Step 1.1: Add failing roundtrip tests for both variants**

Open `crates/roy/src/control.rs`. Find the `spawned_event_roundtrips` test (around line 573) and insert two new tests right after it:

```rust
    #[test]
    fn spawning_event_roundtrips() {
        roundtrip(&ServerEvent::Spawning {
            agent: "claude".into(),
            project_id: Some("pid".into()),
        });
        roundtrip(&ServerEvent::Spawning {
            agent: "opencode".into(),
            project_id: None,
        });
    }

    #[test]
    fn resuming_event_roundtrips() {
        roundtrip(&ServerEvent::Resuming {
            session: "sid".into(),
        });
    }

    #[test]
    fn spawning_event_wire_format() {
        let json = serde_json::to_string(&ServerEvent::Spawning {
            agent: "claude".into(),
            project_id: None,
        })
        .unwrap();
        assert_eq!(json, r#"{"kind":"spawning","agent":"claude"}"#);
    }

    #[test]
    fn resuming_event_wire_format() {
        let json = serde_json::to_string(&ServerEvent::Resuming {
            session: "sid".into(),
        })
        .unwrap();
        assert_eq!(json, r#"{"kind":"resuming","session":"sid"}"#);
    }
```

- [ ] **Step 1.2: Run tests to verify they fail to compile**

Run:

```bash
cargo test -p roy --lib control:: 2>&1 | tail -30
```

Expected: compilation error like `no variant or associated item named 'Spawning' found for enum 'ServerEvent'`.

- [ ] **Step 1.3: Add the two new variants to `ServerEvent`**

In `crates/roy/src/control.rs`, find the `Spawned { … }` variant (around line 287) and the `Resumed { … }` variant (around line 340). Insert the new variants directly after them. The resulting block:

```rust
    /// Response to `Spawn`. `project_id` is `Some` when the session was
    /// spawned inside a project, `None` for orphan sessions.
    Spawned {
        session: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume_cursor: Option<String>,
    },
    /// Emitted immediately upon receiving `Spawn`, before the agent process
    /// is started. Lets clients render a "spawning…" indicator during the
    /// process launch + ACP `initialize` + `session/new` round-trip. The
    /// session id is not yet known at this point — clients correlate by
    /// request order on their own connection.
    Spawning {
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
    },
```

And next to `Resumed`:

```rust
    /// Response to `Resume`. Same session id as requested; `resume_cursor`
    /// reflects what the transport reported after resuming.
    Resumed {
        session: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume_cursor: Option<String>,
    },
    /// Emitted immediately upon receiving `Resume`, before the agent process
    /// is re-started. Lets clients render a "resuming…" indicator during the
    /// process launch + ACP `session/load` round-trip.
    Resuming { session: String },
```

- [ ] **Step 1.4: Run tests to verify they pass**

Run:

```bash
cargo test -p roy --lib control::
```

Expected: all four new tests pass; nothing else breaks. (The existing tests already match on specific variants and use `..` wildcards or `other => panic!`, so they're unaffected.)

- [ ] **Step 1.5: Commit**

```bash
git add crates/roy/src/control.rs
git commit -m "feat(control): add Spawning / Resuming ServerEvent variants

Wire-protocol additions used by the daemon to ack Spawn / Resume commands
immediately, before the slow agent-process startup phase."
```

---

## Task 2 — Emit `Spawning` from `handle_spawn`

**Files:**
- Modify: `crates/roy/src/daemon.rs` — `handle_spawn` (around line 562), and the existing happy-path test `spawn_attach_send_round_trip_over_duplex` (line 1521).

- [ ] **Step 2.1: Update the existing spawn test to assert `Spawning` arrives before `Spawned`**

Open `crates/roy/src/daemon.rs`. Find the block at lines 1540-1555 inside `spawn_attach_send_round_trip_over_duplex`:

```rust
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Spawn {
                agent: "opencode".into(),
                project_id: None,
                model: None,
                permission: None,
                resume: None,
                tags: BTreeMap::new(),
            },
        )
        .await;
        let session = match next_event_line(&mut events).await {
            ServerEvent::Spawned { session, .. } => session,
            other => panic!("expected Spawned, got {other:?}"),
        };
```

Replace the post-send match with an ack assertion followed by the existing `Spawned` match:

```rust
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Spawn {
                agent: "opencode".into(),
                project_id: None,
                model: None,
                permission: None,
                resume: None,
                tags: BTreeMap::new(),
            },
        )
        .await;
        match next_event_line(&mut events).await {
            ServerEvent::Spawning { agent, project_id } => {
                assert_eq!(agent, "opencode");
                assert_eq!(project_id, None);
            }
            other => panic!("expected Spawning ack, got {other:?}"),
        }
        let session = match next_event_line(&mut events).await {
            ServerEvent::Spawned { session, .. } => session,
            other => panic!("expected Spawned, got {other:?}"),
        };
```

- [ ] **Step 2.2: Run the test to verify it fails**

Run:

```bash
cargo test -p roy --lib spawn_attach_send_round_trip_over_duplex -- --nocapture 2>&1 | tail -30
```

Expected: panic `expected Spawning ack, got Spawned { … }` — because the daemon does not yet emit `Spawning`.

- [ ] **Step 2.3: Emit `Spawning` at the entry of `handle_spawn`**

In `crates/roy/src/daemon.rs`, find `handle_spawn` (line 556). Insert a `send` call as the first statement of the function body, **before** `resolve_spawn_cwd`. The full method head becomes:

```rust
    async fn handle_spawn(
        self: &Arc<Self>,
        agent: AgentPreset,
        project_id: Option<String>,
        model: Option<String>,
        permission: Option<String>,
        resume: Option<String>,
        tags: BTreeMap<String, String>,
        event_tx: &EventTx,
    ) {
        let _ = event_tx.send(ServerEvent::Spawning {
            agent: agent.to_string(),
            project_id: project_id.clone(),
        });
        let (cwd, fixed_session_id) = match self.resolve_spawn_cwd(project_id.as_deref()) {
            Ok(pair) => pair,
            Err(e) => {
                // … existing body unchanged …
```

(`AgentPreset` implements `Display` — see `crates/roy/src/agents_config.rs:38` — so `agent.to_string()` yields `"claude" | "gemini" | "opencode" | "codex"`.)

- [ ] **Step 2.4: Run the test to verify it passes**

Run:

```bash
cargo test -p roy --lib spawn_attach_send_round_trip_over_duplex
```

Expected: PASS.

- [ ] **Step 2.5: Sanity-check the wider test surface**

Run:

```bash
cargo build --workspace --all-targets
cargo test --workspace --no-fail-fast 2>&1 | tail -40
```

Expected: green. Other spawn-using tests use `..` wildcards or do `next_event_line` reads with `_ = …`, so they're unaffected by the extra event — but if anything turned out to assert exact event sequences strictly, surface and fix it here. Likely candidates to skim:
- `spawn_attach_send_round_trip_over_websocket` (line 2178)
- `create_project_then_spawn_attaches` (line 2642)
- `spawn_without_project_creates_orphan_dir` (line 2713)
- `fire_combo_spawns_sends_and_waits` (line 2408) — `Fire` does **not** go through `handle_spawn`, so it should be unaffected; if it were, the spec is being violated.

If any of these match `ServerEvent::Spawned { … }` as the very next event after sending `Spawn`, update them the same way as Step 2.1. Show the actual fix in the commit message.

- [ ] **Step 2.6: Commit**

```bash
git add crates/roy/src/daemon.rs
git commit -m "feat(daemon): emit Spawning ack at entry of handle_spawn

Clients can now render a 'spawning…' indicator immediately after issuing
Spawn, distinguishing in-flight from hung handlers (notably the
claude-code-acp auth-hang case)."
```

---

## Task 3 — Emit `Resuming` from `handle_resume`

**Files:**
- Modify: `crates/roy/src/daemon.rs` — `handle_resume` (around line 600), and the existing test `close_then_resume_continues_the_journal` (line 1927).

- [ ] **Step 3.1: Update the existing resume test to assert `Resuming` arrives before `Resumed`**

Open `crates/roy/src/daemon.rs`. Find the block at lines 2040-2055 in `close_then_resume_continues_the_journal`:

```rust
        // 2. Resume.
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Resume {
                session: session.clone(),
                tags: None,
            },
        )
        .await;
        match next_event_line(&mut events).await {
            ServerEvent::Resumed {
                session: resumed_id,
                ..
            } => assert_eq!(resumed_id, session, "resume must keep the same session id"),
            other => panic!("expected Resumed, got {other:?}"),
        }
```

Replace with an ack assertion followed by the existing `Resumed` match:

```rust
        // 2. Resume.
        send_cmd_line(
            &mut client_wr,
            &ClientCommand::Resume {
                session: session.clone(),
                tags: None,
            },
        )
        .await;
        match next_event_line(&mut events).await {
            ServerEvent::Resuming {
                session: resuming_id,
            } => assert_eq!(resuming_id, session, "Resuming must echo the requested id"),
            other => panic!("expected Resuming ack, got {other:?}"),
        }
        match next_event_line(&mut events).await {
            ServerEvent::Resumed {
                session: resumed_id,
                ..
            } => assert_eq!(resumed_id, session, "resume must keep the same session id"),
            other => panic!("expected Resumed, got {other:?}"),
        }
```

- [ ] **Step 3.2: Run the test to verify it fails**

Run:

```bash
cargo test -p roy --lib close_then_resume_continues_the_journal -- --nocapture 2>&1 | tail -30
```

Expected: panic `expected Resuming ack, got Resumed { … }`.

- [ ] **Step 3.3: Emit `Resuming` at the entry of `handle_resume`**

In `crates/roy/src/daemon.rs`, find `handle_resume` (line 600). Insert a `send` call as the first statement of the function body, **before** `manager.resume`:

```rust
    async fn handle_resume(
        self: &Arc<Self>,
        session: String,
        tags: Option<BTreeMap<String, String>>,
        event_tx: &EventTx,
    ) {
        let _ = event_tx.send(ServerEvent::Resuming {
            session: session.clone(),
        });
        match self.manager.resume(&session, 256, 1024).await {
            Ok(engine) => {
                // … existing body unchanged …
```

- [ ] **Step 3.4: Run the test to verify it passes**

Run:

```bash
cargo test -p roy --lib close_then_resume_continues_the_journal
```

Expected: PASS.

- [ ] **Step 3.5: Sanity-check the wider test surface**

Run:

```bash
cargo build --workspace --all-targets
cargo test --workspace --no-fail-fast 2>&1 | tail -40
```

Expected: green. The only other test that issues `ClientCommand::Resume` directly should be `close_then_resume_continues_the_journal` — verify by:

```bash
grep -n "ClientCommand::Resume" crates/roy/src/daemon.rs crates/roy-cli/src/**/*.rs 2>/dev/null
```

If a second test surfaces, apply the same fix as Step 3.1 to it.

- [ ] **Step 3.6: Commit**

```bash
git add crates/roy/src/daemon.rs
git commit -m "feat(daemon): emit Resuming ack at entry of handle_resume

Clients can now render a 'resuming…' indicator immediately after issuing
Resume, distinguishing in-flight from hung handlers during the slow
session/load round-trip."
```

---

## Task 4 — Document the new events in `docs/wire-protocol.md`

**Files:**
- Modify: `docs/wire-protocol.md` — add the two new events to the `ServerEvent` table and add a short paragraph describing the ack contract.

- [ ] **Step 4.1: Update the ServerEvent table**

Open `docs/wire-protocol.md`. Find the table starting at line 133. Insert two new rows: `spawning` directly after `spawned`, and `resuming` directly after `resumed`. The result:

```markdown
| kind                | fields                                                                                                  |
|---------------------|---------------------------------------------------------------------------------------------------------|
| `spawned`           | `session`, optional `project_id`, optional `resume_cursor`                                              |
| `spawning`          | `agent`, optional `project_id` — ack emitted at start of `spawn` before agent process launch            |
| `attached`          | `session`, `seq_at_attach`                                                                              |
| `frame`             | `session`, `entry` (the `JournalEntry` shape above)                                                     |
| `input_acquired`    | `session`, `acquired: bool`                                                                             |
| `input_released`    | `session`                                                                                               |
| `detached`          | `session`                                                                                               |
| `closed`            | `session`                                                                                               |
| `listed`            | `sessions: [{id, project_id}]`                                                                          |
| `listed_archived`   | `sessions: [{id, project_id}]`                                                                          |
| `resumed`           | `session`, optional `resume_cursor`                                                                     |
| `resuming`          | `session` — ack emitted at start of `resume` before agent process re-launch                             |
| `journal_read`      | `session`, `entries: [JournalEntry]`, `next_seq`, `has_more: bool`                                       |
| `projects_listed`   | `projects: [Project]`                                                                                   |
| `project_created`   | `project: Project`                                                                                      |
| `project_deleted`   | `project_id: string`, `deleted_sessions: [string]`                                                      |
| `agents_list`       | `agents: [AgentInfo]`, `config_path: string`, `status: AgentsConfigStatus`                              |
| `error`             | optional `session`, typed `code` (see below), `message`                                                 |
```

- [ ] **Step 4.2: Add a paragraph describing the ack contract**

In `docs/wire-protocol.md`, find the line `spawned.project_id is null for an orphan session, a UUID string otherwise.` (around line 152). Add this paragraph immediately after the explanation block for `spawned.resume_cursor` (~line 168-169), before the `journal_read.next_seq` paragraph:

```markdown
For every accepted `spawn` and `resume` command the daemon emits an ack
event before the terminal one: `spawning → (spawned | error)` and
`resuming → (resumed | error)`. The ack lets clients render a loading
indicator during the slow agent-process startup phase and turns silent
hangs (e.g. an unauthenticated `claude-code-acp` blocking inside ACP
`initialize`) into a visible "started but never finished" state. Clients
clear the loading state on any terminal event for that command.
```

- [ ] **Step 4.3: Commit**

```bash
git add docs/wire-protocol.md
git commit -m "docs(wire-protocol): document Spawning / Resuming ack events"
```

---

## Task 5 — Final validation

- [ ] **Step 5.1: Run the full CI gate locally**

```bash
cargo fmt --all -- --check
cargo build --workspace --all-targets
cargo test --workspace --no-fail-fast
```

Expected: all three green. (`python3` must be on PATH for the integration tests that drive the fake ACP agent.)

- [ ] **Step 5.2: Smoke-test via real WebSocket (optional, manual)**

If a daemon is running locally, send a Resume from the user's browser client and confirm that two events arrive in order:

```
{"kind":"resuming","session":"<id>"}
{"kind":"resumed","session":"<id>","resume_cursor":"<cursor>"}
```

For Spawn, expect:

```
{"kind":"spawning","agent":"claude"}
{"kind":"spawned","session":"<new-id>",…}
```

- [ ] **Step 5.3: Confirm no dead branches or follow-up tasks**

Grep the repo for any `TODO`s introduced by this change:

```bash
git diff master -- crates docs | grep -E "TODO|FIXME"
```

Expected: empty output.
