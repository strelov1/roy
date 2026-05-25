# Plan A — Core: inline system prompt + injection

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let any trigger pass an inline `system_prompt` on spawn; the daemon injects it as a real ACP system prompt (`_meta.systemPrompt` for claude/opencode) or as a first journaled turn (gemini/codex), and snapshots it into session metadata so it survives resume.

**Architecture:** `system_prompt` rides on `ClientCommand::Spawn` / `FireTarget::Spawn` → `SessionSpawnConfig` → `Transport::open`. `AcpConfig` declares a per-preset `SystemPromptChannel`. For `Meta`, `AcpTransport` sets `_meta.systemPrompt = {append: …}` on `session/new` + `session/load`. For `FirstTurn`, the handle stashes the persona and the engine injects it as a `Cmd::Persona` first turn (journaled as `System`). `SessionMetadata` snapshots the body; resume re-threads it.

**Tech Stack:** Rust, tokio, `agent-client-protocol` SDK (`Meta = serde_json::Map<String, Value>`), serde, the python fake ACP agent for tests.

**Spec:** `docs/superpowers/specs/2026-05-24-agents-personas-design.md` (Part A).

---

### Task 1: Wire — add `system_prompt` to `Spawn` and `FireTarget::Spawn`

**Files:**
- Modify: `crates/roy/src/control.rs` (`ClientCommand::Spawn` ~153-166, `FireTarget::Spawn` ~269-274)
- Test: `crates/roy/src/control.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `control.rs`:

```rust
    #[test]
    fn spawn_with_system_prompt_roundtrips() {
        roundtrip(&ClientCommand::Spawn {
            agent: "claude".into(),
            project_id: None,
            model: None,
            permission: None,
            resume: None,
            tags: BTreeMap::new(),
            system_prompt: Some("You are terse.".into()),
        });
    }

    #[test]
    fn spawn_omits_system_prompt_when_none() {
        let s = serde_json::to_string(&ClientCommand::Spawn {
            agent: "claude".into(),
            project_id: None,
            model: None,
            permission: None,
            resume: None,
            tags: BTreeMap::new(),
            system_prompt: None,
        })
        .unwrap();
        assert!(!s.contains("system_prompt"), "None must be skipped: {s}");
    }

    #[test]
    fn fire_target_spawn_with_system_prompt_roundtrips() {
        roundtrip(&FireTarget::Spawn {
            preset: "claude".into(),
            project_id: None,
            system_prompt: Some("persona".into()),
        });
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p roy --lib control:: 2>&1 | tail -20`
Expected: FAIL — `ClientCommand::Spawn` has no field `system_prompt` / `FireTarget::Spawn` has no field `system_prompt`.

- [ ] **Step 3: Add the fields**

In `ClientCommand::Spawn`, after the `tags` field (line ~165):

```rust
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        tags: BTreeMap<String, String>,
        /// Inline system/persona prompt. The daemon injects it (ACP
        /// `_meta.systemPrompt` where the preset supports it, else as a first
        /// journaled turn) and snapshots it into `SessionMetadata`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
    },
```

In `FireTarget::Spawn` (line ~269-274):

```rust
    Spawn {
        preset: String,
        /// `Some(project_id)` to spawn inside a project's dir; `None` for orphan.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        /// Inline system/persona prompt (see `ClientCommand::Spawn`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
    },
```

- [ ] **Step 4: Fix existing construction sites in this file**

The existing `spawn_command_roundtrips` test (and any other `Spawn`/`FireTarget::Spawn` literal in `control.rs`) now needs `system_prompt: None`. Add `system_prompt: None,` to each `ClientCommand::Spawn { … }` literal in the test module (there are two in `spawn_command_roundtrips`).

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p roy --lib control:: 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/roy/src/control.rs
git commit -m "feat(control): add inline system_prompt to Spawn and FireTarget::Spawn"
```

---

### Task 2: `SystemPromptChannel` + per-preset `AcpConfig` field

**Files:**
- Modify: `crates/roy/src/transport/acp/mod.rs` (`AcpConfig` ~56-69, builders ~71-123)
- Test: `crates/roy/src/transport/acp/mod.rs` (`#[cfg(test)] mod tests` near line 654)

- [ ] **Step 1: Write the failing test**

Add to the acp `mod tests`:

```rust
    #[test]
    fn presets_declare_system_prompt_channel() {
        use super::SystemPromptChannel::*;
        assert_eq!(AcpConfig::claude().system_prompt_channel, Meta);
        assert_eq!(AcpConfig::opencode().system_prompt_channel, Meta);
        assert_eq!(AcpConfig::gemini().system_prompt_channel, FirstTurn);
        assert_eq!(AcpConfig::codex().system_prompt_channel, FirstTurn);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p roy --lib transport::acp 2>&1 | tail -20`
Expected: FAIL — no `SystemPromptChannel` / no field `system_prompt_channel`.

- [ ] **Step 3: Add the enum and field**

Above `pub struct AcpConfig` (line ~55):

```rust
/// How a preset accepts a system/persona prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemPromptChannel {
    /// Sent via ACP `_meta.systemPrompt = { append }` on `session/new` and
    /// `session/load`. A real system prompt, outside history, survives resume.
    Meta,
    /// The preset ignores `_meta`; the engine injects the persona as the first
    /// journaled turn instead.
    FirstTurn,
}
```

Add the field to `AcpConfig` (after `env_remove`):

```rust
    pub env_remove: Vec<String>,
    /// Which channel carries the persona prompt for this preset.
    pub system_prompt_channel: SystemPromptChannel,
}
```

- [ ] **Step 4: Set it in each builder**

In `gemini()` and `codex()` add `system_prompt_channel: SystemPromptChannel::FirstTurn,`. In `opencode()` and `claude()` add `system_prompt_channel: SystemPromptChannel::Meta,`. (Add the line inside each returned `Self { … }`.)

- [ ] **Step 5: Export the enum**

In `crates/roy/src/transport/mod.rs` line 13, extend the re-export:

```rust
pub use acp::{AcpConfig, AcpTransport, PermissionPolicy, SystemPromptChannel};
```

- [ ] **Step 6: Run to verify pass**

Run: `cargo test -p roy --lib transport::acp 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/roy/src/transport/acp/mod.rs crates/roy/src/transport/mod.rs
git commit -m "feat(transport): declare per-preset SystemPromptChannel"
```

---

### Task 3: `Transport::open` gains `system_prompt`; `Handle::take_pending_persona`

This is the cascade task: changing the trait signature forces edits across
`AcpTransport`, the engine, the manager, the daemon, and every test that calls
`open`/constructs a `Handle`. The tree will not compile mid-task; it compiles
again at Step 8.

**Files:**
- Modify: `crates/roy/src/transport/mod.rs` (trait `Transport::open` 26-31, trait `Handle` 37-44)
- Modify: `crates/roy/src/transport/acp/mod.rs` (`open` 149-271, `setup_session` 331-374, `run_session` 276-329, `AcpHandle`, the `Handle` impl)

- [ ] **Step 1: Change the `Transport` and `Handle` traits**

In `transport/mod.rs`:

```rust
#[async_trait]
pub trait Transport: Send + Sync {
    async fn open(
        &self,
        session_id: &str,
        resume_cursor: Option<&str>,
        cwd: PathBuf,
        system_prompt: Option<&str>,
    ) -> Result<Box<dyn Handle>>;
}

#[async_trait]
pub trait Handle: Send {
    async fn send(&mut self, prompt: &str) -> Result<(TurnStream, CancelSignal)>;
    fn resume_cursor(&self) -> Option<String>;
    /// Persona to inject as the first turn, set only when the transport could
    /// not apply it natively (FirstTurn channel) AND this was a fresh open.
    /// Drains on first call. `None` for Meta channels and all resumes.
    fn take_pending_persona(&mut self) -> Option<String>;
    async fn close(&mut self) -> Result<()>;
}
```

- [ ] **Step 2: Thread `system_prompt` into `AcpTransport::open` and `setup_session`**

In `acp/mod.rs` `open` signature (149-154) add `system_prompt: Option<&str>,`. Compute the channel + pending persona near the top of `open` (after `let cwd = …`):

```rust
        let channel = self.config.system_prompt_channel;
        let system_prompt = system_prompt.map(str::to_string);
        // Meta channel sends the prompt inside the session request; FirstTurn
        // defers it to the engine. On resume (resume_cursor.is_some) the agent
        // reloads history, so never defer a fresh first-turn persona then.
        let meta_prompt = match channel {
            SystemPromptChannel::Meta => system_prompt.clone(),
            SystemPromptChannel::FirstTurn => None,
        };
        let pending_persona = match channel {
            SystemPromptChannel::FirstTurn if resume_cursor.is_none() => system_prompt.clone(),
            _ => None,
        };
```

Import `SystemPromptChannel` is in-module (same file). Capture `meta_prompt` into the spawned task alongside `resume`/`mode_id`:

```rust
        let resume = resume_cursor.map(str::to_string);
        let meta_prompt_for_task = meta_prompt.clone();
```

Pass `meta_prompt_for_task` into `run_session(...)` (add a parameter) and onward to `setup_session`. Update `run_session`'s signature (276-285) to take `meta_prompt: Option<String>` and forward it to `setup_session`.

Change `setup_session` (331-374) signature to add `meta_prompt: Option<String>,` and build the requests with `_meta`:

```rust
    let (session_id, modes) = match resume {
        Some(sid) => {
            let mut req = LoadSessionRequest::new(sid.clone(), cwd);
            apply_system_prompt_meta(&mut req.meta, meta_prompt.as_deref());
            cx.send_request(req).block_task().await?;
            (SessionId::from(sid), None)
        }
        None => {
            let mut req = NewSessionRequest::new(cwd);
            apply_system_prompt_meta(&mut req.meta, meta_prompt.as_deref());
            let response = cx.send_request(req).block_task().await?;
            (response.session_id, response.modes)
        }
    };
```

- [ ] **Step 3: Add the `_meta` builder helper**

Add near `setup_session` in `acp/mod.rs`:

```rust
/// Set `_meta.systemPrompt = { "append": <prompt> }` on a request's meta map.
/// No-op when `prompt` is `None`. claude-code-acp appends this to its
/// `claude_code` preset; honored on both `session/new` and `session/load`.
fn apply_system_prompt_meta(meta: &mut Option<agent_client_protocol::schema::Meta>, prompt: Option<&str>) {
    let Some(prompt) = prompt else { return };
    let map = meta.get_or_insert_with(serde_json::Map::new);
    map.insert(
        "systemPrompt".to_string(),
        serde_json::json!({ "append": prompt }),
    );
}
```

Add `Meta` to the schema import block (27-33): add `Meta` to the `use agent_client_protocol::schema::{ … }` list.

- [ ] **Step 4: Add `pending_persona` to `AcpHandle` and implement `take_pending_persona`**

Find the `AcpHandle` struct + its `impl Handle`. Add a field:

```rust
struct AcpHandle {
    cmd_tx: mpsc::UnboundedSender<SessionCommand>,
    acp_sid: String,
    pending_persona: Option<String>,
}
```

Construct it with the persona at line ~261:

```rust
            Ok(Ok(acp_sid)) => Ok(Box::new(AcpHandle {
                cmd_tx,
                acp_sid,
                pending_persona,
            })),
```

In `impl Handle for AcpHandle`, add:

```rust
    fn take_pending_persona(&mut self) -> Option<String> {
        self.pending_persona.take()
    }
```

- [ ] **Step 5: Update the engine call site**

In `crates/roy/src/engine.rs` `start` (147-149):

```rust
        let handle = transport
            .open(
                &session_id,
                cfg.resume_cursor.as_deref(),
                cfg.cwd.clone(),
                cfg.system_prompt.as_deref(),
            )
            .await?;
```

(`cfg.system_prompt` is added in Task 4 Step 3; if implementing strictly in order, temporarily pass `None` here and switch to `cfg.system_prompt.as_deref()` in Task 4. To avoid churn, do Task 4 Step 3's `SessionSpawnConfig` field addition now.)

- [ ] **Step 6: Fix all other `open(...)` callers and `Handle` impls**

Run `cargo build -p roy --all-targets 2>&1 | grep -E "error" | head -40` and fix each:
- Every `transport.open(a, b, c)` call → add a 4th arg (`None` in tests that don't exercise personas).
- Every test `Handle` impl (search `impl Handle for`) → add `fn take_pending_persona(&mut self) -> Option<String> { None }`.
- Look in `crates/roy/src/transport/acp/mod.rs` tests, `crates/roy/tests/acp_transport.rs`, and any in-crate fake transport in `engine.rs`/`manager.rs`/`daemon.rs` test modules.

- [ ] **Step 7: Build to verify the tree compiles**

Run: `cargo build -p roy --all-targets 2>&1 | tail -20`
Expected: builds clean (warnings about unused `system_prompt` plumbing are fine until Task 4/6 wire it through).

- [ ] **Step 8: Commit**

```bash
git add crates/roy/src/transport crates/roy/src/engine.rs
git commit -m "feat(transport): thread system_prompt through open(); Meta injection + pending persona"
```

---

### Task 4: Engine — `SessionSpawnConfig.system_prompt`, `Cmd::Persona`, first-turn injection

**Files:**
- Modify: `crates/roy/src/engine.rs` (`SessionSpawnConfig` 52-68, `start` 137-197, `Cmd` 93-101, `run_actor` 434-483)
- Test: `crates/roy/src/engine.rs` (`#[cfg(test)] mod tests` — uses the in-crate fake transport)

- [ ] **Step 1: Write the failing test**

In the engine test module, add a test that uses the existing fake transport but with a `FirstTurn`-style handle that returns a pending persona. (Pattern: the engine tests already build a fake `Transport`/`Handle`. Mirror it; make the fake `Handle::take_pending_persona` return `Some("PERSONA".into())` once.) Then assert the first journaled event is `System { subtype: "persona" }`:

```rust
    #[tokio::test]
    async fn first_turn_persona_is_journaled_as_system() {
        // fake transport whose handle returns Some("PERSONA") from
        // take_pending_persona() and records prompts it receives.
        let engine = SessionEngine::spawn(
            fake_transport_with_pending_persona("PERSONA"),
            EngineOpts::with_journal_dir(tmpdir()),
            spawn_cfg_with_system_prompt(Some("PERSONA")),
        )
        .await
        .unwrap();
        // give the actor a moment to process the injected Cmd::Persona
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let entries = engine.snapshot(0).await.unwrap();
        assert!(matches!(
            entries.first().map(|e| &e.event),
            Some(TurnEvent::System { subtype }) if subtype == "persona"
        ), "first entry should be the persona System marker, got {entries:?}");
    }
```

(Define the two `fake_*` helpers in the test module alongside the existing fakes.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p roy --lib engine::tests::first_turn_persona 2>&1 | tail -20`
Expected: FAIL — `SessionSpawnConfig` has no `system_prompt`; no persona journaled.

- [ ] **Step 3: Add the field to `SessionSpawnConfig`**

After `tags` (line 67):

```rust
    pub tags: BTreeMap<String, String>,
    /// Inline persona prompt. Forwarded to `Transport::open`; snapshotted into
    /// `SessionMetadata`. For FirstTurn presets it is injected as the first turn.
    pub system_prompt: Option<String>,
}
```

- [ ] **Step 4: Add `Cmd::Persona`**

In `enum Cmd` (93-101):

```rust
enum Cmd {
    Prompt(String),
    /// Persona/system prompt injected as the first turn (FirstTurn presets).
    /// Journaled as `System { subtype: "persona" }` rather than `UserPrompt`.
    Persona(String),
    Cancel,
    Close,
}
```

- [ ] **Step 5: Enqueue the persona after open, before the actor runs**

In `start`, after the `engine` `Arc` is built and `write_metadata` succeeds, before/after `tokio::spawn(run_actor(...))` — enqueue it via `input_tx` so it is the first command the actor sees. Insert right before `let engine_for_actor = …`:

```rust
        // FirstTurn presets: the transport deferred the persona. Inject it as
        // the very first turn so the agent assumes it before any user prompt.
        // `handle` is moved into the actor below, so drain the pending persona
        // here while we still hold it.
```

`handle` is consumed by `run_actor`. Drain the persona just before spawning the actor:

```rust
        let mut handle = handle;
        if let Some(persona) = handle.take_pending_persona() {
            // Unbounded channel; this send precedes any external prompt.
            let _ = engine.input_tx.send(Cmd::Persona(persona));
        }
        let engine_for_actor = Arc::clone(&engine);
        tokio::spawn(run_actor(engine_for_actor, handle, input_rx));
```

(Remove the prior `let handle` immutability by making it `mut` at the `transport.open` binding, or rebind as shown.)

- [ ] **Step 6: Handle `Cmd::Persona` in `run_actor`**

Refactor the `Cmd::Prompt` arm body into a helper and call it for both, differing only in the journaled event. Replace the `Cmd::Prompt(text) => { … }` arm and add a `Cmd::Persona` arm:

```rust
            Cmd::Prompt(text) => {
                run_input_turn(&engine, handle.as_mut(), &text, &mut input_rx, false).await;
            }
            Cmd::Persona(text) => {
                run_input_turn(&engine, handle.as_mut(), &text, &mut input_rx, true).await;
            }
```

Add the helper (extracted from the current Prompt arm, parameterised by `as_system`):

```rust
async fn run_input_turn(
    engine: &Arc<SessionEngine>,
    handle: &mut dyn Handle,
    text: &str,
    input_rx: &mut mpsc::UnboundedReceiver<Cmd>,
    as_system: bool,
) {
    engine.touch_activity();
    let pre_event = if as_system {
        TurnEvent::System { subtype: "persona".to_string() }
    } else {
        TurnEvent::UserPrompt { text: text.to_string() }
    };
    if let Err(e) = publish(engine, pre_event).await {
        tracing::error!(session = %engine.session_id, error = %e, "failed to journal turn prelude; turn still dispatched");
    }
    drive_turn(engine, handle, text, input_rx).await;
    if let Some(cursor) = handle.resume_cursor() {
        *engine.resume_cursor.lock().unwrap() = Some(cursor);
        if let Err(e) = engine.persist_metadata().await {
            tracing::warn!(session = %engine.session_id, error = %e, "failed to persist session metadata after turn");
        }
    }
}
```

(`drive_turn` takes `&SessionEngine`; pass `engine` — `&Arc<SessionEngine>` derefs. Adjust the signature to `engine: &SessionEngine` and call `run_input_turn(&engine, …)` deref if the borrow checker prefers; keep whichever compiles.)

- [ ] **Step 7: Run to verify pass**

Run: `cargo test -p roy --lib engine:: 2>&1 | tail -30`
Expected: PASS (new test + existing engine tests).

- [ ] **Step 8: Commit**

```bash
git add crates/roy/src/engine.rs
git commit -m "feat(engine): inject FirstTurn persona as a System first turn"
```

---

### Task 5: Metadata — snapshot `system_prompt` (+ `agent_name`); read back on resume

**Files:**
- Modify: `crates/roy/src/session_meta.rs` (`SessionMetadata` 21-36)
- Modify: `crates/roy/src/engine.rs` (`start` `write_metadata` 172-185; `metadata_snapshot` 388-399)
- Modify: `crates/roy/src/manager.rs` (`resume` cfg 103-112)
- Test: `crates/roy/src/session_meta.rs`, `crates/roy/src/manager.rs`

- [ ] **Step 1: Write the failing test (metadata roundtrip)**

In `session_meta.rs` tests, extend `write_and_read_roundtrip`'s `meta` literal with `system_prompt: Some("You are terse.".into()), agent_name: Some("Reviewer".into()),` and assert equality already covers it. Add a focused test:

```rust
    #[tokio::test]
    async fn system_prompt_snapshot_roundtrips() {
        let dir = tmpdir();
        let meta = SessionMetadata {
            session_id: "sid".into(),
            agent: "claude".into(),
            cwd: PathBuf::from("/tmp"),
            project_id: None,
            model: None,
            permission: None,
            resume_cursor: None,
            tags: BTreeMap::new(),
            system_prompt: Some("PERSONA".into()),
            agent_name: None,
        };
        write_metadata(&dir, &meta).await.unwrap();
        assert_eq!(read_metadata(&dir, "sid").await.unwrap().system_prompt.as_deref(), Some("PERSONA"));
        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p roy --lib session_meta 2>&1 | tail -20`
Expected: FAIL — no field `system_prompt` / `agent_name`.

- [ ] **Step 3: Add the fields**

In `SessionMetadata` after `tags` (line 35):

```rust
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tags: BTreeMap<String, String>,
    /// Snapshot of the persona prompt the session was spawned with. Re-applied
    /// on resume; editing/deleting the source agent never mutates a live session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Optional display label of the agent that spawned this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
}
```

- [ ] **Step 4: Write the snapshot at spawn**

In `engine.rs` `start`, the `write_metadata` `SessionMetadata { … }` literal (172-184): add `system_prompt: cfg.system_prompt.clone(), agent_name: None,`. In `metadata_snapshot` (388-399): add `system_prompt: ???`. The engine must keep the snapshot to re-persist after each turn. Add a field to `SessionEngine`:

```rust
    permission: Option<String>,
    system_prompt: Option<String>,
```

Set it in the `Arc::new(Self { … })` (152-167): `system_prompt: cfg.system_prompt.clone(),`. Then in `metadata_snapshot`: `system_prompt: self.system_prompt.clone(), agent_name: None,`. In the `start` `write_metadata` literal use `cfg.system_prompt.clone()`.

- [ ] **Step 5: Read it back on resume**

In `manager.rs` `resume`, the `SessionSpawnConfig { … }` literal (103-112): add `system_prompt: meta.system_prompt,`. (`meta.system_prompt` is consumed once; `meta.tags`/`meta.model` are already moved similarly.)

- [ ] **Step 6: Fix remaining `SessionMetadata` / `SessionSpawnConfig` literals**

Run `cargo build -p roy --all-targets 2>&1 | grep error | head -30`. Add the new fields to every literal the compiler flags:
- `manager.rs` test `orphan_cfg` (380) → `system_prompt: None,`.
- daemon.rs `handle_spawn`/`handle_fire` cfg literals (592, 803) → `system_prompt: …` (wired in Task 6; use `None` placeholder now).
- Any test `SessionMetadata`/`SessionSpawnConfig` literals.

- [ ] **Step 7: Run to verify pass**

Run: `cargo test -p roy --lib 2>&1 | tail -20`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/roy/src/session_meta.rs crates/roy/src/engine.rs crates/roy/src/manager.rs
git commit -m "feat(session): snapshot system_prompt into metadata; re-apply on resume"
```

---

### Task 6: Daemon — thread `system_prompt` from the wire to `SessionSpawnConfig`

**Files:**
- Modify: `crates/roy/src/daemon.rs` (dispatch 428-447, `handle_spawn` 566-612, `handle_fire` spawn block 775-815)
- Test: `crates/roy/src/daemon.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

The daemon tests already spawn over a duplex with a fake transport that records what it receives. Add a test that sends `ClientCommand::Spawn { …, system_prompt: Some("PERSONA") }` and asserts the resulting `<sid>.meta.json` contains `system_prompt: "PERSONA"`:

```rust
    #[tokio::test]
    async fn spawn_persists_system_prompt_in_metadata() {
        // build daemon with the test fake-transport factory + temp journal dir
        // (mirror an existing spawn_* test's setup), send Spawn with
        // system_prompt: Some("PERSONA"), read Spawned { session }, then:
        let meta = crate::session_meta::read_metadata(&journal_dir, &session).await.unwrap();
        assert_eq!(meta.system_prompt.as_deref(), Some("PERSONA"));
    }
```

(Clone the setup from `spawn_attach_send_round_trip_over_duplex` at line 1538.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p roy --lib daemon::tests::spawn_persists_system_prompt 2>&1 | tail -20`
Expected: FAIL — `system_prompt` is `None` in metadata (dispatch drops it).

- [ ] **Step 3: Thread through dispatch**

In the `ClientCommand::Spawn { … }` match (428-435) add `system_prompt,` to the destructure, and pass it to `handle_spawn`:

```rust
            ClientCommand::Spawn {
                agent,
                project_id,
                model,
                permission,
                resume,
                tags,
                system_prompt,
            } => {
                let preset: AgentPreset = match agent.parse() {
                    Ok(p) => p,
                    Err(e) => { send_error(event_tx, None, ErrorCode::SpawnFailed, e); return; }
                };
                self.handle_spawn(
                    preset, project_id, model, permission, resume, tags, system_prompt, event_tx,
                )
                .await
            }
```

- [ ] **Step 4: Accept + use it in `handle_spawn`**

Add `system_prompt: Option<String>,` to `handle_spawn`'s params (after `tags`, before `event_tx`). In the `SessionSpawnConfig { … }` literal (592-601) add `system_prompt,`.

- [ ] **Step 5: Use it in the Fire spawn path**

In `handle_fire`, destructure `system_prompt` from `FireTarget::Spawn { preset, project_id, system_prompt }` (775) and add `system_prompt,` to that block's `SessionSpawnConfig` literal (803-812).

- [ ] **Step 6: Fix the placeholder from Task 5 Step 6**

Where Task 5 left `system_prompt: None` in `handle_spawn`/`handle_fire` cfg literals, ensure they now use the real `system_prompt` variable.

- [ ] **Step 7: Run to verify pass**

Run: `cargo test -p roy --lib daemon:: 2>&1 | tail -30`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/roy/src/daemon.rs
git commit -m "feat(daemon): thread inline system_prompt from Spawn/Fire into the session"
```

---

### Task 7: fake ACP agent echoes `_meta`; transport integration test for Meta channel

**Files:**
- Modify: `crates/roy/tests/scripts/fake-acp-agent.py`
- Modify: `crates/roy/tests/acp_transport.rs`

- [ ] **Step 1: Make the fake agent echo received `_meta`**

In `fake-acp-agent.py`, when handling `session/new` (and `session/load`), capture the request's `_meta` and expose it: write it to a file path given by env `FAKE_ACP_META_OUT` if set, e.g.:

```python
# inside the session/new handler, params = request["params"]
meta = params.get("_meta")
out = os.environ.get("FAKE_ACP_META_OUT")
if out and meta is not None:
    with open(out, "w") as f:
        json.dump(meta, f)
```

(Do the same in the `session/load` handler so resume is observable.)

- [ ] **Step 2: Write the failing test**

In `acp_transport.rs`, add a test that builds an `AcpConfig` for the fake agent with `system_prompt_channel: Meta`, sets `FAKE_ACP_META_OUT` to a temp file, calls `transport.open(sid, None, cwd, Some("PERSONA"))`, then reads the temp file and asserts it contains `{"systemPrompt":{"append":"PERSONA"}}`:

```rust
    #[tokio::test]
    async fn meta_channel_sends_system_prompt_on_session_new() {
        let out = std::env::temp_dir().join(format!("roy-meta-{}.json", uuid::Uuid::new_v4()));
        std::env::set_var("FAKE_ACP_META_OUT", &out);
        let cfg = fake_acp_config(SystemPromptChannel::Meta); // helper building AcpConfig for fake-acp-agent.py
        let transport = AcpTransport::new(cfg);
        let _handle = transport.open("sid", None, std::env::current_dir().unwrap(), Some("PERSONA")).await.unwrap();
        let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(meta["systemPrompt"]["append"], "PERSONA");
        let _ = std::fs::remove_file(&out);
    }
```

(Add `fake_acp_config(channel)` helper next to the existing fake-agent config builder in this test file; reuse the python-agent command the file already uses for its other tests.)

- [ ] **Step 3: Run to verify it fails, then passes**

Run: `cargo test -p roy --test acp_transport meta_channel 2>&1 | tail -20`
Expected: FAIL first (helper/echo absent), then PASS after Steps 1-2.

- [ ] **Step 4: Commit**

```bash
git add crates/roy/tests/scripts/fake-acp-agent.py crates/roy/tests/acp_transport.rs
git commit -m "test(acp): assert _meta.systemPrompt is sent on session/new for Meta channel"
```

---

### Task 8: CLI + MCP convenience flags

**Files:**
- Modify: `crates/roy-cli/src/main.rs` (the `run` subcommand args + the `Spawn` it builds)
- Modify: `crates/roy-cli/src/mcp.rs` (`roy_run` / `roy_run_detached` tool input schema + the `Spawn` they build)

- [ ] **Step 1: Add CLI flags**

In the `run` subcommand args struct add:

```rust
    /// Inline system/persona prompt for the session.
    #[arg(long)]
    system_prompt: Option<String>,
    /// Read the system/persona prompt from a file (overrides --system-prompt).
    #[arg(long)]
    system_prompt_file: Option<std::path::PathBuf>,
```

Resolve it before building the `Spawn`:

```rust
    let system_prompt = match (system_prompt_file, system_prompt) {
        (Some(path), _) => Some(std::fs::read_to_string(&path)
            .with_context(|| format!("reading --system-prompt-file {}", path.display()))?),
        (None, inline) => inline,
    };
```

Add `system_prompt,` to the `ClientCommand::Spawn { … }` the `run` command sends.

- [ ] **Step 2: Add the MCP argument**

In `mcp.rs`, add an optional `system_prompt: Option<String>` to the `roy_run` / `roy_run_detached` argument structs and JSON input schema, and pass it into the `ClientCommand::Spawn` those tools construct.

- [ ] **Step 3: Build + smoke**

Run: `cargo build --workspace --all-targets 2>&1 | tail -10`
Expected: clean.
Run: `cargo run -p roy-cli -- run --help 2>&1 | grep system-prompt`
Expected: shows `--system-prompt` and `--system-prompt-file`.

- [ ] **Step 4: Commit**

```bash
git add crates/roy-cli/src/main.rs crates/roy-cli/src/mcp.rs
git commit -m "feat(cli,mcp): --system-prompt / system_prompt arg for run"
```

---

### Task 9: Full gate + docs

**Files:**
- Modify: `docs/wire-protocol.md`, `docs/persistence.md` (note the new field)

- [ ] **Step 1: Run the full CI gate locally**

```bash
cargo fmt --all -- --check
cargo build --workspace --all-targets
cargo test --workspace --no-fail-fast
```
Expected: all green (real-CLI smoke tests self-skip).

- [ ] **Step 2: Document the field**

In `docs/wire-protocol.md` add `system_prompt` to the `Spawn` / `FireTarget::Spawn` description. In `docs/persistence.md` note `SessionMetadata.system_prompt` is a spawn-time snapshot re-applied on resume.

- [ ] **Step 3: Commit**

```bash
git add docs/wire-protocol.md docs/persistence.md
git commit -m "docs: document inline system_prompt and its metadata snapshot"
```

---

## Self-review

- **Spec coverage (Part A):** A1 wire → Task 1; A2 channel/transport → Tasks 2-3; A3 engine first-turn → Task 4; A4 metadata snapshot → Task 5; A5 daemon wiring → Task 6; A6 CLI/MCP → Task 8; A7 testing → Tasks 1-7 + Task 9 gate. Real-CLI smoke is listed in the spec; add it opportunistically (an `#[ignore]` test mirroring the existing `real_claude` test with a `system_prompt`).
- **Placeholder scan:** the only deliberately deferred values are the `system_prompt: None` placeholders in Task 5 Step 6, explicitly replaced in Task 6 Step 6. No `TBD`s.
- **Type consistency:** `system_prompt: Option<String>` everywhere; trait `take_pending_persona(&mut self) -> Option<String>`; `SystemPromptChannel { Meta, FirstTurn }`; helper `apply_system_prompt_meta(&mut Option<Meta>, Option<&str>)`; engine helper `run_input_turn(..., as_system: bool)`. Names match across tasks.
