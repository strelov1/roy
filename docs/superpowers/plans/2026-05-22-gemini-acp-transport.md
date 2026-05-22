# Gemini ACP Transport Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Gemini support to `roy` via a persistent `AcpTransport` that speaks Agent Client Protocol (JSON-RPC 2.0 over stdio) to `gemini --acp`, with multi-turn and resume, without respawning per turn.

**Architecture:** Keep `Session`/`TurnEvent` as the stable core. First refactor the transport seam (drop the `Provider` param from `Transport::open`; add `Handle::resume_cursor()` so each transport reports its own resume token), then split `transport.rs` into a module. Then add `AcpTransport`: a JSON-RPC client over the child's stdio that maps ACP `session/update` notifications and the `session/prompt` result into `TurnEvent`s.

**Tech Stack:** Rust, tokio (process/io/sync), async-stream, serde_json, async-trait. No new dependencies.

---

## Empirically confirmed ACP protocol (gemini 0.43.0 — do not re-derive)

Launch: `gemini --acp --skip-trust`. Read stdout only (stderr is noise). One JSON object per line.

```
→ {"id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}
← {"id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true,...}}}
→ {"id":2,"method":"session/new","params":{"cwd":"<abs>","mcpServers":[]}}
← {"id":2,"result":{"sessionId":"<uuid>","modes":{...},"models":{...}}}
→ {"id":3,"method":"session/set_mode","params":{"sessionId":"<uuid>","modeId":"yolo"}}
← {"id":3,"result":{}}
→ {"id":4,"method":"session/prompt","params":{"sessionId":"<uuid>","prompt":[{"type":"text","text":"<p>"}]}}
← {"method":"session/update","params":{"sessionId":"<uuid>","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello"}}}}
← {"id":4,"result":{"stopReason":"end_turn","_meta":{"quota":{...}}}}    ← turn end
```

Resume: `session/load {"sessionId","cwd","mcpServers":[]}` instead of `session/new` (verified: context retained).
Agent→client request `session/request_permission` (id+params) → answer `{"result":{"outcome":{"outcome":"selected","optionId":"allow"}}}`.

## File structure

- `src/error.rs` — add `RoyError::Protocol(String)`.
- `src/session.rs` — drop `provider`; ctors take only transport; cursor from `handle.resume_cursor()`.
- `src/transport.rs` → **split into module**:
  - `src/transport/mod.rs` — `Transport` + `Handle` traits; re-export `PrintTransport`, `AcpTransport`, `AcpConfig`.
  - `src/transport/print.rs` — `PrintTransport`/`PrintHandle` (holds `Provider`).
  - `src/transport/acp/mod.rs` — `AcpTransport`/`AcpHandle` + `AcpConfig`.
  - `src/transport/acp/protocol.rs` — ACP→`TurnEvent` mapping (pure functions).
  - `src/transport/acp/client.rs` — `JsonRpcClient` (request/response + notification routing).
- `tests/scripts/fake-acp-agent.py` — minimal ACP agent for hermetic tests.
- `tests/acp_transport.rs` — integration tests against the fake ACP agent.
- `examples/demo_gemini.rs` — manual gemini driver.
- `tests/fake_provider.rs`, `examples/demo.rs` — updated for the refactored signatures.

---

## Task 1: Add RoyError::Protocol

**Files:**
- Modify: `src/error.rs`

- [ ] **Step 1: Add the variant**

In `src/error.rs`, add inside the `enum RoyError`:
```rust
    #[error("protocol error: {0}")]
    Protocol(String),
```

- [ ] **Step 2: Verify build**

Run: `. "$HOME/.cargo/env" && cargo build`
Expected: compiles.

- [ ] **Step 3: Commit**

```bash
git add src/error.rs
git commit -m "feat: RoyError::Protocol variant"
```

---

## Task 2: Add Handle::resume_cursor(), source the cursor from the handle

**Files:**
- Modify: `src/transport.rs`
- Modify: `src/session.rs`

- [ ] **Step 1: Add `resume_cursor` to the `Handle` trait**

In `src/transport.rs`, change the `Handle` trait to:
```rust
#[async_trait]
pub trait Handle: Send {
    async fn send(
        &mut self,
        prompt: &str,
    ) -> Result<std::pin::Pin<Box<dyn Stream<Item = TurnEvent> + Send + '_>>>;
    /// Opaque token to resume THIS session on the next `open`. claude: the
    /// session id; gemini: the ACP sessionId from session/new.
    fn resume_cursor(&self) -> Option<String>;
    async fn close(&mut self) -> Result<()>;
}
```

- [ ] **Step 2: Store the session id on `PrintHandle` and implement `resume_cursor`**

In `src/transport.rs`, add a field to `PrintHandle`:
```rust
pub struct PrintHandle {
    child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<TurnEvent>,
    provider: Arc<dyn Provider>,
    session_id: String,
}
```

In `PrintTransport::open`, set it when constructing the handle (replace the `Ok(Box::new(PrintHandle { ... }))`):
```rust
        Ok(Box::new(PrintHandle {
            child,
            stdin,
            rx,
            provider,
            session_id: session_id.to_string(),
        }))
```

In the `impl Handle for PrintHandle`, add the method (before `close`):
```rust
    fn resume_cursor(&self) -> Option<String> {
        Some(self.session_id.clone())
    }
```

- [ ] **Step 3: Use the handle's cursor in `Session::send`**

In `src/session.rs`, replace the body of the `if self.handle.is_none()` block in `send` with:
```rust
        if self.handle.is_none() {
            let handle = self
                .transport
                .open(
                    Arc::clone(&self.provider),
                    &self.id,
                    self.resume_cursor.as_deref(),
                    self.cwd.clone(),
                )
                .await?;
            self.resume_cursor = handle.resume_cursor();
            self.handle = Some(handle);
        }
```

- [ ] **Step 4: Run the full suite**

Run: `. "$HOME/.cargo/env" && cargo test`
Expected: all tests PASS (cursor now comes from `handle.resume_cursor()`, which returns the session id — same value as before).

- [ ] **Step 5: Commit**

```bash
git add src/transport.rs src/session.rs
git commit -m "refactor: Handle reports its own resume_cursor"
```

---

## Task 3: Drop the Provider param from Transport::open

**Files:**
- Modify: `src/transport.rs`
- Modify: `src/session.rs`
- Modify: `examples/demo.rs`
- Modify: `tests/fake_provider.rs`

- [ ] **Step 1: Change the `Transport` trait + `PrintTransport` to hold the provider**

In `src/transport.rs`:

Trait:
```rust
#[async_trait]
pub trait Transport: Send + Sync {
    async fn open(
        &self,
        session_id: &str,
        resume_cursor: Option<&str>,
        cwd: PathBuf,
    ) -> Result<Box<dyn Handle>>;
}
```

Replace the `PrintTransport` struct + `new` + `Default` with:
```rust
pub struct PrintTransport {
    provider: Arc<dyn Provider>,
}

impl PrintTransport {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }
}
```

Change `impl Transport for PrintTransport`'s `open` signature and its first two lines to use `self.provider`:
```rust
    async fn open(
        &self,
        session_id: &str,
        resume_cursor: Option<&str>,
        cwd: PathBuf,
    ) -> Result<Box<dyn Handle>> {
        let provider = Arc::clone(&self.provider);
        let cmd_name = provider.command().to_string();
        let args = provider.spawn_args(session_id, resume_cursor);
```
(The rest of `open` is unchanged — it already uses the local `provider`.)

- [ ] **Step 2: Update `Session` to not hold a provider**

In `src/session.rs`, replace the struct + `new` + `resume` + the `open` call in `send`:

Struct:
```rust
pub struct Session {
    id: String,
    cwd: PathBuf,
    resume_cursor: Option<String>,
    transport: Arc<dyn Transport>,
    handle: Option<Box<dyn Handle>>,
}
```

`new`:
```rust
    pub fn new(transport: Arc<dyn Transport>, cwd: PathBuf) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            cwd,
            resume_cursor: None,
            transport,
            handle: None,
        }
    }
```

`resume`:
```rust
    pub fn resume(transport: Arc<dyn Transport>, cwd: PathBuf, session_id: String) -> Self {
        Self {
            id: session_id.clone(),
            cwd,
            resume_cursor: Some(session_id),
            transport,
            handle: None,
        }
    }
```

`send` open call:
```rust
        if self.handle.is_none() {
            let handle = self
                .transport
                .open(&self.id, self.resume_cursor.as_deref(), self.cwd.clone())
                .await?;
            self.resume_cursor = handle.resume_cursor();
            self.handle = Some(handle);
        }
```

Remove the now-unused `use crate::provider::Provider;` import from `src/session.rs`.

- [ ] **Step 3: Update `examples/demo.rs`**

Replace the construction block:
```rust
    let provider: Arc<dyn Provider> = Arc::new(ClaudeProvider::new(Some(
        "claude-haiku-4-5-20251001".to_string(),
    )));
    let transport: Arc<dyn Transport> = Arc::new(PrintTransport::new(provider));
    let mut session = Session::new(transport, std::env::current_dir()?);
```

- [ ] **Step 4: Update `tests/fake_provider.rs`**

Replace every `PrintTransport::new()` with `PrintTransport::new(Arc::new(FakeProvider))` and drop the provider argument and `Session::new(provider, transport, …)` provider arg. Concretely:

- `open_spawns_process`:
```rust
#[tokio::test]
async fn open_spawns_process() {
    let transport = PrintTransport::new(Arc::new(FakeProvider));
    let handle = transport
        .open("fake-session", None, std::env::current_dir().unwrap())
        .await
        .expect("open should spawn the fake agent");
    drop(handle);
}
```

- `send_streams_until_turn_end`:
```rust
#[tokio::test]
async fn send_streams_until_turn_end() {
    let transport = PrintTransport::new(Arc::new(FakeProvider));
    let mut handle = transport
        .open("fake-session", None, std::env::current_dir().unwrap())
        .await
        .unwrap();
    // ... rest unchanged ...
```

- `session_send_sets_resume_cursor`:
```rust
#[tokio::test]
async fn session_send_sets_resume_cursor() {
    let transport: Arc<dyn roy::transport::Transport> =
        Arc::new(PrintTransport::new(Arc::new(FakeProvider)));
    let mut session = Session::new(transport, std::env::current_dir().unwrap());
    // ... rest unchanged ...
```

- `resume_existing_session_keeps_id_and_cursor`:
```rust
#[tokio::test]
async fn resume_existing_session_keeps_id_and_cursor() {
    let transport: Arc<dyn roy::transport::Transport> =
        Arc::new(PrintTransport::new(Arc::new(FakeProvider)));
    let mut session = Session::resume(
        transport,
        std::env::current_dir().unwrap(),
        "prior-session-id".to_string(),
    );
    // ... rest unchanged ...
```

- `real_claude_spawn_and_turn` (the `#[ignore]` test):
```rust
    let provider: Arc<dyn Provider> =
        Arc::new(roy::provider::ClaudeProvider::new(Some("claude-haiku-4-5-20251001".into())));
    let transport: Arc<dyn roy::transport::Transport> = Arc::new(PrintTransport::new(provider));
    let mut session = Session::new(transport, std::env::current_dir().unwrap());
```

The top-level `use` lines stay (`Provider`, `PrintTransport`, `Transport`, `Session`, `TurnEvent`).

- [ ] **Step 5: Run the full suite**

Run: `. "$HOME/.cargo/env" && cargo test`
Expected: all hermetic tests PASS.

- [ ] **Step 6: Commit**

```bash
git add src/transport.rs src/session.rs examples/demo.rs tests/fake_provider.rs
git commit -m "refactor: transports own their config; open() drops provider param"
```

---

## Task 4: Split transport.rs into a module

**Files:**
- Create: `src/transport/mod.rs`
- Create: `src/transport/print.rs`
- Delete: `src/transport.rs`

- [ ] **Step 1: Create `src/transport/print.rs`**

Move ALL of the current `src/transport.rs` EXCEPT the trait definitions into `src/transport/print.rs`. That file should contain: the imports, `PrintTransport`, its `impl Transport`, `PrintHandle`, its `impl Handle`. Add `use super::{Handle, Transport};` and adjust imports. Full file:

```rust
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::mpsc;
use tokio_stream::Stream;

use super::{Handle, Transport};
use crate::error::{Result, RoyError};
use crate::event::TurnEvent;
use crate::provider::Provider;

pub struct PrintTransport {
    provider: Arc<dyn Provider>,
}

impl PrintTransport {
    pub fn new(provider: Arc<dyn Provider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl Transport for PrintTransport {
    async fn open(
        &self,
        session_id: &str,
        resume_cursor: Option<&str>,
        cwd: PathBuf,
    ) -> Result<Box<dyn Handle>> {
        let provider = Arc::clone(&self.provider);
        let cmd_name = provider.command().to_string();
        let args = provider.spawn_args(session_id, resume_cursor);

        let mut child = tokio::process::Command::new(&cmd_name)
            .args(&args)
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| RoyError::Spawn { cmd: cmd_name, source })?;

        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");

        let (tx, rx) = mpsc::channel::<TurnEvent>(256);
        let reader_provider = Arc::clone(&provider);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(ev) = reader_provider.parse_line(&line) {
                    if tx.send(ev).await.is_err() {
                        break;
                    }
                }
            }
        });

        Ok(Box::new(PrintHandle {
            child,
            stdin,
            rx,
            provider,
            session_id: session_id.to_string(),
        }))
    }
}

pub struct PrintHandle {
    child: Child,
    stdin: ChildStdin,
    rx: mpsc::Receiver<TurnEvent>,
    provider: Arc<dyn Provider>,
    session_id: String,
}

#[async_trait]
impl Handle for PrintHandle {
    async fn send(
        &mut self,
        prompt: &str,
    ) -> Result<std::pin::Pin<Box<dyn Stream<Item = TurnEvent> + Send + '_>>> {
        let line = self.provider.encode_user_message(prompt);
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;

        let provider = Arc::clone(&self.provider);
        let rx = &mut self.rx;
        let stream = async_stream::stream! {
            while let Some(ev) = rx.recv().await {
                let end = provider.is_turn_end(&ev);
                yield ev;
                if end {
                    break;
                }
            }
        };
        Ok(Box::pin(stream))
    }

    fn resume_cursor(&self) -> Option<String> {
        Some(self.session_id.clone())
    }

    async fn close(&mut self) -> Result<()> {
        let _ = self.child.start_kill();
        Ok(())
    }
}
```

- [ ] **Step 2: Create `src/transport/mod.rs` with the traits + re-exports**

```rust
use std::path::PathBuf;

use async_trait::async_trait;
use tokio_stream::Stream;

use crate::error::Result;
use crate::event::TurnEvent;

pub mod print;

pub use print::PrintTransport;

/// How bytes move between us and the agent process.
#[async_trait]
pub trait Transport: Send + Sync {
    async fn open(
        &self,
        session_id: &str,
        resume_cursor: Option<&str>,
        cwd: PathBuf,
    ) -> Result<Box<dyn Handle>>;
}

/// A live agent process. `send` writes one user turn and streams its events
/// until turn end.
#[async_trait]
pub trait Handle: Send {
    async fn send(
        &mut self,
        prompt: &str,
    ) -> Result<std::pin::Pin<Box<dyn Stream<Item = TurnEvent> + Send + '_>>>;
    /// Opaque token to resume THIS session on the next `open`. claude: the
    /// session id; gemini: the ACP sessionId from session/new.
    fn resume_cursor(&self) -> Option<String>;
    async fn close(&mut self) -> Result<()>;
}
```

- [ ] **Step 3: Delete the old file**

Run: `rm src/transport.rs`

- [ ] **Step 4: Run the full suite**

Run: `. "$HOME/.cargo/env" && cargo test`
Expected: all tests PASS. `src/lib.rs` already says `pub mod transport;` and `pub use transport::{Handle, PrintTransport, Transport};` — those still resolve.

- [ ] **Step 5: Commit**

```bash
git add -A src/transport src/transport.rs
git commit -m "refactor: split transport into a module"
```

---

## Task 5: ACP event mapping (acp/protocol.rs)

**Files:**
- Create: `src/transport/acp/protocol.rs`
- Create: `src/transport/acp/mod.rs` (stub for now)

- [ ] **Step 1: Create the acp module stub so the protocol file has a parent**

Create `src/transport/acp/mod.rs`:
```rust
pub mod protocol;
```

Add to `src/transport/mod.rs` after `pub mod print;`:
```rust
pub mod acp;
```

- [ ] **Step 2: Write the failing tests (real gemini fixtures)**

Create `src/transport/acp/protocol.rs`:
```rust
use serde_json::Value;

use crate::event::TurnEvent;

/// Map an ACP `session/update` params object to a TurnEvent, or None to drop
/// (noise / unmodeled). `Raw` preserves unknown update kinds.
pub fn update_to_event(params: &Value) -> Option<TurnEvent> {
    let update = params.get("update")?;
    match update.get("sessionUpdate").and_then(Value::as_str)? {
        "agent_message_chunk" => {
            let content = update.get("content")?;
            if content.get("type").and_then(Value::as_str) == Some("text") {
                let text = content.get("text").and_then(Value::as_str).unwrap_or("").to_string();
                Some(TurnEvent::AssistantText { text })
            } else {
                None
            }
        }
        "tool_call" => {
            let name = update
                .get("title")
                .and_then(Value::as_str)
                .or_else(|| update.get("kind").and_then(Value::as_str))
                .unwrap_or("")
                .to_string();
            let input = update.get("rawInput").cloned().unwrap_or(Value::Null);
            Some(TurnEvent::ToolUse { name, input })
        }
        "available_commands_update" => None,
        _ => Some(TurnEvent::Raw(update.clone())),
    }
}

/// Map a `session/prompt` result object to the terminal Result event.
pub fn prompt_result_to_event(result: &Value) -> TurnEvent {
    let stop = result.get("stopReason").and_then(Value::as_str).unwrap_or("");
    let is_error = !(stop == "end_turn" || stop == "max_tokens");
    TurnEvent::Result { cost_usd: None, is_error }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_agent_message_chunk_text() {
        let p: Value = serde_json::from_str(
            r#"{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello"}}}"#,
        ).unwrap();
        assert_eq!(update_to_event(&p), Some(TurnEvent::AssistantText { text: "hello".into() }));
    }

    #[test]
    fn drops_available_commands_update() {
        let p: Value = serde_json::from_str(
            r#"{"sessionId":"s","update":{"sessionUpdate":"available_commands_update","availableCommands":[]}}"#,
        ).unwrap();
        assert_eq!(update_to_event(&p), None);
    }

    #[test]
    fn maps_tool_call() {
        let p: Value = serde_json::from_str(
            r#"{"sessionId":"s","update":{"sessionUpdate":"tool_call","title":"Bash","rawInput":{"command":"ls"}}}"#,
        ).unwrap();
        match update_to_event(&p) {
            Some(TurnEvent::ToolUse { name, input }) => {
                assert_eq!(name, "Bash");
                assert_eq!(input["command"], "ls");
            }
            other => panic!("expected ToolUse, got {other:?}"),
        }
    }

    #[test]
    fn unknown_update_is_raw() {
        let p: Value = serde_json::from_str(
            r#"{"sessionId":"s","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"hmm"}}}"#,
        ).unwrap();
        assert!(matches!(update_to_event(&p), Some(TurnEvent::Raw(_))));
    }

    #[test]
    fn prompt_result_end_turn_is_success() {
        let r: Value = serde_json::from_str(r#"{"stopReason":"end_turn"}"#).unwrap();
        assert_eq!(prompt_result_to_event(&r), TurnEvent::Result { cost_usd: None, is_error: false });
    }

    #[test]
    fn prompt_result_refusal_is_error() {
        let r: Value = serde_json::from_str(r#"{"stopReason":"refusal"}"#).unwrap();
        assert_eq!(prompt_result_to_event(&r), TurnEvent::Result { cost_usd: None, is_error: true });
    }
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test --lib acp::protocol`
Expected: PASS (6 tests).

- [ ] **Step 4: Commit**

```bash
git add src/transport/mod.rs src/transport/acp
git commit -m "feat: ACP session/update -> TurnEvent mapping"
```

---

## Task 6: JSON-RPC client (acp/client.rs)

**Files:**
- Create: `src/transport/acp/client.rs`
- Modify: `src/transport/acp/mod.rs`

- [ ] **Step 1: Declare the client module**

In `src/transport/acp/mod.rs`, add:
```rust
pub mod client;
```

- [ ] **Step 2: Write `src/transport/acp/client.rs`**

```rust
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::error::{Result, RoyError};
use crate::event::TurnEvent;

use super::protocol::{prompt_result_to_event, update_to_event};

type Writer = Box<dyn AsyncWrite + Send + Unpin>;
type Reader = Box<dyn AsyncRead + Send + Unpin>;

struct Shared {
    pending: HashMap<i64, oneshot::Sender<std::result::Result<Value, Value>>>,
    turn_tx: Option<mpsc::Sender<TurnEvent>>,
    active_prompt_id: Option<i64>,
}

/// Minimal JSON-RPC 2.0 peer over a child's stdio. Handshake calls
/// (`request`) await their response; the prompt turn (`begin_prompt`) routes
/// `session/update` notifications and the terminal `session/prompt` result
/// into a per-turn channel.
pub struct JsonRpcClient {
    writer: Arc<Mutex<Writer>>,
    shared: Arc<Mutex<Shared>>,
    next_id: AtomicI64,
}

impl JsonRpcClient {
    pub fn new(reader: Reader, writer: Writer) -> Arc<Self> {
        let shared = Arc::new(Mutex::new(Shared {
            pending: HashMap::new(),
            turn_tx: None,
            active_prompt_id: None,
        }));
        let writer = Arc::new(Mutex::new(writer));
        let client = Arc::new(Self {
            writer: Arc::clone(&writer),
            shared: Arc::clone(&shared),
            next_id: AtomicI64::new(1),
        });

        let r_shared = Arc::clone(&shared);
        let r_writer = Arc::clone(&writer);
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let msg: Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                Self::route(&r_shared, &r_writer, msg).await;
            }
            // stdout closed: end any active turn.
            let mut s = r_shared.lock().await;
            s.turn_tx = None;
            s.active_prompt_id = None;
        });

        client
    }

    async fn route(shared: &Arc<Mutex<Shared>>, writer: &Arc<Mutex<Writer>>, msg: Value) {
        let id = msg.get("id").and_then(Value::as_i64);
        let method = msg.get("method").and_then(Value::as_str).map(str::to_string);
        let has_result = msg.get("result").is_some() || msg.get("error").is_some();

        // Response to one of our requests.
        if let (Some(id), true) = (id, has_result) {
            let mut s = shared.lock().await;
            if s.active_prompt_id == Some(id) {
                s.active_prompt_id = None;
                let ev = msg
                    .get("result")
                    .map(prompt_result_to_event)
                    .unwrap_or(TurnEvent::Result { cost_usd: None, is_error: true });
                if let Some(tx) = s.turn_tx.take() {
                    let _ = tx.send(ev).await;
                }
            } else if let Some(send) = s.pending.remove(&id) {
                if let Some(err) = msg.get("error") {
                    let _ = send.send(Err(err.clone()));
                } else {
                    let _ = send.send(Ok(msg.get("result").cloned().unwrap_or(Value::Null)));
                }
            }
            return;
        }

        // Incoming agent->client request.
        if let (Some(id), Some(method)) = (id, method.clone()) {
            let response = if method.contains("request_permission") {
                json!({"jsonrpc":"2.0","id":id,"result":{"outcome":{"outcome":"selected","optionId":"allow"}}})
            } else {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"method not found"}})
            };
            let mut w = writer.lock().await;
            let _ = w.write_all(format!("{response}\n").as_bytes()).await;
            let _ = w.flush().await;
            return;
        }

        // Notification.
        if method.as_deref() == Some("session/update") {
            if let Some(params) = msg.get("params") {
                if let Some(ev) = update_to_event(params) {
                    let s = shared.lock().await;
                    if let Some(tx) = &s.turn_tx {
                        let _ = tx.send(ev).await;
                    }
                }
            }
        }
    }

    async fn write_msg(&self, msg: Value) -> Result<()> {
        let mut w = self.writer.lock().await;
        w.write_all(format!("{msg}\n").as_bytes()).await?;
        w.flush().await?;
        Ok(())
    }

    /// Send a request and await its response. Used for the open handshake.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        {
            self.shared.lock().await.pending.insert(id, tx);
        }
        self.write_msg(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        match rx.await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(RoyError::Protocol(e.to_string())),
            Err(_) => Err(RoyError::ProcessExited),
        }
    }

    /// Begin a turn: install the event sink and fire `session/prompt`. Returns
    /// the receiver the caller streams until `TurnEvent::Result`.
    pub async fn begin_prompt(&self, params: Value) -> Result<mpsc::Receiver<TurnEvent>> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel::<TurnEvent>(256);
        {
            let mut s = self.shared.lock().await;
            s.turn_tx = Some(tx);
            s.active_prompt_id = Some(id);
        }
        self.write_msg(json!({"jsonrpc":"2.0","id":id,"method":"session/prompt","params":params}))
            .await?;
        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    // Drives the client against an in-memory "agent" over duplex pipes.
    #[tokio::test]
    async fn request_correlates_response_by_id() {
        let (client_side, agent_side) = tokio::io::duplex(8192);
        let (agent_read, agent_write) = tokio::io::split(agent_side);
        let (client_read, client_write) = tokio::io::split(client_side);
        let client = JsonRpcClient::new(Box::new(client_read), Box::new(client_write));

        // Fake agent: read one request, reply with a result echoing its id.
        tokio::spawn(async move {
            let mut lines = BufReader::new(agent_read).lines();
            let mut w = agent_write;
            if let Ok(Some(line)) = lines.next_line().await {
                let req: Value = serde_json::from_str(&line).unwrap();
                let id = req["id"].as_i64().unwrap();
                let resp = json!({"jsonrpc":"2.0","id":id,"result":{"ok":true}});
                w.write_all(format!("{resp}\n").as_bytes()).await.unwrap();
                w.flush().await.unwrap();
            }
        });

        let res = client.request("initialize", json!({"protocolVersion":1})).await.unwrap();
        assert_eq!(res["ok"], true);
    }

    #[tokio::test]
    async fn begin_prompt_streams_updates_then_result() {
        let (client_side, agent_side) = tokio::io::duplex(8192);
        let (agent_read, agent_write) = tokio::io::split(agent_side);
        let (client_read, client_write) = tokio::io::split(client_side);
        let client = JsonRpcClient::new(Box::new(client_read), Box::new(client_write));

        // Fake agent: on the prompt request, emit one update notification then
        // the terminal result with the same id.
        tokio::spawn(async move {
            let mut lines = BufReader::new(agent_read).lines();
            let mut w = agent_write;
            if let Ok(Some(line)) = lines.next_line().await {
                let req: Value = serde_json::from_str(&line).unwrap();
                let id = req["id"].as_i64().unwrap();
                let upd = json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"ack"}}}});
                w.write_all(format!("{upd}\n").as_bytes()).await.unwrap();
                let done = json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":"end_turn"}});
                w.write_all(format!("{done}\n").as_bytes()).await.unwrap();
                w.flush().await.unwrap();
            }
        });

        let mut rx = client.begin_prompt(json!({"sessionId":"s","prompt":[]})).await.unwrap();
        let mut got = Vec::new();
        while let Some(ev) = rx.recv().await {
            let end = matches!(ev, TurnEvent::Result { .. });
            got.push(ev);
            if end {
                break;
            }
        }
        assert!(got.iter().any(|e| matches!(e, TurnEvent::AssistantText { text } if text == "ack")));
        assert!(matches!(got.last(), Some(TurnEvent::Result { is_error: false, .. })));
    }
}
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `. "$HOME/.cargo/env" && cargo test --lib acp::client`
Expected: PASS (2 tests).

- [ ] **Step 4: Commit**

```bash
git add src/transport/acp/mod.rs src/transport/acp/client.rs
git commit -m "feat: ACP JSON-RPC client (request/response + turn streaming)"
```

---

## Task 7: AcpTransport + AcpHandle + hermetic integration tests

**Files:**
- Modify: `src/transport/acp/mod.rs`
- Modify: `src/transport/mod.rs`
- Modify: `src/lib.rs`
- Create: `tests/scripts/fake-acp-agent.py`
- Create: `tests/acp_transport.rs`

- [ ] **Step 1: Write the fake ACP agent**

Create `tests/scripts/fake-acp-agent.py`:
```python
#!/usr/bin/env python3
"""Minimal ACP agent for hermetic AcpTransport tests. Speaks JSON-RPC over
stdio. With --permission, asks for permission before finishing a turn and only
finishes after the client auto-allows."""
import sys, json

permission = "--permission" in sys.argv

def out(o):
    sys.stdout.write(json.dumps(o) + "\n")
    sys.stdout.flush()

pending = None  # (prompt_id, session_id) awaiting the client's allow

def finish_turn(prompt_id, sid):
    out({"jsonrpc": "2.0", "method": "session/update",
         "params": {"sessionId": sid,
                    "update": {"sessionUpdate": "agent_message_chunk",
                               "content": {"type": "text", "text": "ack"}}}})
    out({"jsonrpc": "2.0", "id": prompt_id, "result": {"stopReason": "end_turn"}})

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        m = json.loads(line)
    except Exception:
        continue
    mid = m.get("id")
    method = m.get("method")
    if method == "initialize":
        out({"jsonrpc": "2.0", "id": mid,
             "result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": True}}})
    elif method == "session/new":
        out({"jsonrpc": "2.0", "id": mid, "result": {"sessionId": "fake-acp-sid"}})
    elif method == "session/load":
        out({"jsonrpc": "2.0", "id": mid, "result": {}})
    elif method == "session/set_mode":
        out({"jsonrpc": "2.0", "id": mid, "result": {}})
    elif method == "session/prompt":
        sid = m["params"]["sessionId"]
        if permission:
            out({"jsonrpc": "2.0", "id": 9001, "method": "session/request_permission",
                 "params": {"sessionId": sid, "toolCall": {"title": "Bash"},
                            "options": [{"optionId": "allow", "name": "Allow"}]}})
            pending = (mid, sid)
        else:
            finish_turn(mid, sid)
    elif mid == 9001 and "result" in m and pending is not None:
        # client allowed the tool; complete the turn
        finish_turn(pending[0], pending[1])
        pending = None
```

Make executable:
Run: `chmod +x tests/scripts/fake-acp-agent.py`

- [ ] **Step 2: Implement `AcpTransport`/`AcpHandle`/`AcpConfig` in `src/transport/acp/mod.rs`**

Replace `src/transport/acp/mod.rs` with:
```rust
pub mod client;
pub mod protocol;

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::process::Child;
use tokio_stream::Stream;

use crate::error::{Result, RoyError};
use crate::event::TurnEvent;

use super::{Handle, Transport};
use client::JsonRpcClient;

/// Launch + behaviour config for an ACP agent.
pub struct AcpConfig {
    pub command: String,
    pub args: Vec<String>,
    /// ACP mode to set after the session opens (e.g. "yolo" to auto-approve).
    pub mode_id: Option<String>,
}

impl AcpConfig {
    /// gemini --acp --skip-trust, auto-approving tools via yolo mode.
    pub fn gemini() -> Self {
        Self {
            command: "gemini".to_string(),
            args: vec!["--acp".to_string(), "--skip-trust".to_string()],
            mode_id: Some("yolo".to_string()),
        }
    }
}

pub struct AcpTransport {
    config: AcpConfig,
}

impl AcpTransport {
    pub fn new(config: AcpConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Transport for AcpTransport {
    async fn open(
        &self,
        _session_id: &str,
        resume_cursor: Option<&str>,
        cwd: PathBuf,
    ) -> Result<Box<dyn Handle>> {
        let mut child = tokio::process::Command::new(&self.config.command)
            .args(&self.config.args)
            .current_dir(&cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| RoyError::Spawn {
                cmd: self.config.command.clone(),
                source,
            })?;

        let stdin = Box::new(child.stdin.take().expect("stdin piped"));
        let stdout = Box::new(child.stdout.take().expect("stdout piped"));
        let client = JsonRpcClient::new(stdout, stdin);

        client
            .request("initialize", json!({"protocolVersion":1,"clientCapabilities":{}}))
            .await?;

        let cwd_str = cwd.to_string_lossy().to_string();
        let acp_sid = match resume_cursor {
            Some(sid) => {
                client
                    .request("session/load", json!({"sessionId":sid,"cwd":cwd_str,"mcpServers":[]}))
                    .await?;
                sid.to_string()
            }
            None => {
                let res = client
                    .request("session/new", json!({"cwd":cwd_str,"mcpServers":[]}))
                    .await?;
                res.get("sessionId")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .ok_or_else(|| RoyError::Protocol("session/new returned no sessionId".into()))?
            }
        };

        if let Some(mode) = &self.config.mode_id {
            client
                .request("session/set_mode", json!({"sessionId":acp_sid,"modeId":mode}))
                .await?;
        }

        Ok(Box::new(AcpHandle { child, client, acp_sid }))
    }
}

pub struct AcpHandle {
    child: Child,
    client: Arc<JsonRpcClient>,
    acp_sid: String,
}

#[async_trait]
impl Handle for AcpHandle {
    async fn send(
        &mut self,
        prompt: &str,
    ) -> Result<std::pin::Pin<Box<dyn Stream<Item = TurnEvent> + Send + '_>>> {
        let params = json!({
            "sessionId": self.acp_sid,
            "prompt": [{"type":"text","text":prompt}]
        });
        let mut rx = self.client.begin_prompt(params).await?;
        let stream = async_stream::stream! {
            while let Some(ev) = rx.recv().await {
                let end = matches!(ev, TurnEvent::Result { .. });
                yield ev;
                if end {
                    break;
                }
            }
        };
        Ok(Box::pin(stream))
    }

    fn resume_cursor(&self) -> Option<String> {
        Some(self.acp_sid.clone())
    }

    async fn close(&mut self) -> Result<()> {
        let _ = self.child.start_kill();
        Ok(())
    }
}
```

- [ ] **Step 3: Re-export from `src/transport/mod.rs` and `src/lib.rs`**

In `src/transport/mod.rs`, ensure the acp module + re-exports exist (replace the `pub mod acp;` line region):
```rust
pub mod acp;
pub mod print;

pub use acp::{AcpConfig, AcpTransport};
pub use print::PrintTransport;
```

In `src/lib.rs`, change the transport re-export line to:
```rust
pub use transport::{AcpConfig, AcpTransport, Handle, PrintTransport, Transport};
```

- [ ] **Step 4: Write the hermetic integration tests**

Create `tests/acp_transport.rs`:
```rust
use std::sync::Arc;

use futures::StreamExt;
use roy::event::TurnEvent;
use roy::session::Session;
use roy::transport::{AcpConfig, AcpTransport, Transport};

fn fake_config(extra: &[&str]) -> AcpConfig {
    let mut args = vec!["tests/scripts/fake-acp-agent.py".to_string()];
    args.extend(extra.iter().map(|s| s.to_string()));
    AcpConfig {
        command: "python3".to_string(),
        args,
        mode_id: Some("yolo".to_string()),
    }
}

#[tokio::test]
async fn open_send_streams_until_result() {
    let transport = AcpTransport::new(fake_config(&[]));
    let mut handle = transport
        .open("ignored", None, std::env::current_dir().unwrap())
        .await
        .unwrap();

    let mut events = Vec::new();
    {
        let mut stream = handle.send("hello").await.unwrap();
        while let Some(ev) = stream.next().await {
            events.push(ev);
        }
    }
    assert!(events.iter().any(|e| matches!(e, TurnEvent::AssistantText { text } if text == "ack")));
    assert!(matches!(events.last(), Some(TurnEvent::Result { is_error: false, .. })));

    // ACP sessionId is exposed as the resume cursor.
    assert_eq!(handle.resume_cursor(), Some("fake-acp-sid".to_string()));

    // Multi-turn on the same live process.
    let mut events2 = Vec::new();
    {
        let mut stream = handle.send("again").await.unwrap();
        while let Some(ev) = stream.next().await {
            events2.push(ev);
        }
    }
    assert!(matches!(events2.last(), Some(TurnEvent::Result { .. })));

    handle.close().await.unwrap();
}

#[tokio::test]
async fn auto_allows_permission_requests() {
    let transport = AcpTransport::new(fake_config(&["--permission"]));
    let mut handle = transport
        .open("ignored", None, std::env::current_dir().unwrap())
        .await
        .unwrap();

    // The fake agent only completes the turn after the client auto-allows the
    // permission request; reaching Result proves the auto-allow happened.
    let mut events = Vec::new();
    {
        let mut stream = handle.send("do a thing").await.unwrap();
        while let Some(ev) = stream.next().await {
            events.push(ev);
        }
    }
    assert!(matches!(events.last(), Some(TurnEvent::Result { .. })));
    handle.close().await.unwrap();
}

#[tokio::test]
async fn session_via_transport_records_acp_cursor() {
    let transport: Arc<dyn Transport> = Arc::new(AcpTransport::new(fake_config(&[])));
    let mut session = Session::new(transport, std::env::current_dir().unwrap());

    {
        let mut stream = session.send("hi").await.unwrap();
        while stream.next().await.is_some() {}
    }
    // Session stored the ACP sessionId (NOT its own uuid) as the resume cursor.
    assert_eq!(session.resume_cursor(), Some("fake-acp-sid"));
    session.close().await.unwrap();
}
```

- [ ] **Step 5: Run the tests**

Run: `. "$HOME/.cargo/env" && cargo test --test acp_transport`
Expected: PASS (3 tests).

- [ ] **Step 6: Run the whole suite (nothing else broke)**

Run: `. "$HOME/.cargo/env" && cargo test`
Expected: all hermetic tests PASS.

- [ ] **Step 7: Commit**

```bash
git add src/transport/acp/mod.rs src/transport/mod.rs src/lib.rs tests/scripts/fake-acp-agent.py tests/acp_transport.rs
git commit -m "feat: AcpTransport (gemini --acp) with multi-turn and resume"
```

---

## Task 8: Gemini demo + ignored real-gemini smoke test

**Files:**
- Create: `examples/demo_gemini.rs`
- Modify: `Cargo.toml`
- Modify: `tests/acp_transport.rs`

- [ ] **Step 1: Register the example in `Cargo.toml`**

Add after the existing `[[example]]` block:
```toml
[[example]]
name = "demo_gemini"
path = "examples/demo_gemini.rs"
```

- [ ] **Step 2: Write `examples/demo_gemini.rs`**

```rust
use std::sync::Arc;

use roy::session::Session;
use roy::transport::{AcpConfig, AcpTransport, Transport};
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let transport: Arc<dyn Transport> = Arc::new(AcpTransport::new(AcpConfig::gemini()));
    let mut session = Session::new(transport, std::env::current_dir()?);

    for prompt in ["reply with exactly: hello", "now reply with exactly: world"] {
        println!("\n>>> {prompt}");
        let mut stream = session.send(prompt).await?;
        while let Some(ev) = stream.next().await {
            println!("  {ev:?}");
        }
    }
    println!("\nresume_cursor (ACP sessionId) = {:?}", session.resume_cursor());
    session.close().await?;
    Ok(())
}
```

- [ ] **Step 3: Run the demo manually (requires gemini + Google login)**

Run:
```bash
. "$HOME/.cargo/env" && cargo run --example demo_gemini
```
Expected: turn 1 prints `AssistantText { text: "hello" }` + `Result`, turn 2 prints `world`, then a non-null `resume_cursor`. (Requires `gemini` installed and logged in.)

- [ ] **Step 4: Add the ignored real-gemini smoke test**

Append to `tests/acp_transport.rs`:
```rust
// Real gemini. Ignored by default: needs the `gemini` binary, logged in.
// Run with: cargo test --test acp_transport -- --ignored real_gemini
#[tokio::test]
#[ignore]
async fn real_gemini_spawn_and_turn() {
    if which_gemini().is_none() {
        eprintln!("skipping: gemini not on PATH");
        return;
    }
    let transport: Arc<dyn Transport> = Arc::new(AcpTransport::new(AcpConfig::gemini()));
    let mut session = Session::new(transport, std::env::current_dir().unwrap());

    let mut answer = String::new();
    {
        let mut stream = session.send("reply with exactly the word: hello").await.unwrap();
        while let Some(ev) = stream.next().await {
            if let TurnEvent::AssistantText { text } = ev {
                answer.push_str(&text);
            }
        }
    }
    assert!(answer.to_lowercase().contains("hello"), "got: {answer:?}");
    assert!(session.resume_cursor().is_some());
    session.close().await.unwrap();
}

fn which_gemini() -> Option<()> {
    std::process::Command::new("gemini")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()
        .filter(|s| s.success())
        .map(|_| ())
}
```

- [ ] **Step 5: Run the ignored smoke test (requires gemini)**

Run:
```bash
. "$HOME/.cargo/env" && cargo test --test acp_transport -- --ignored real_gemini_spawn_and_turn
```
Expected: PASS (answer contains "hello"). If gemini is absent the test prints "skipping" and passes.

- [ ] **Step 6: Run the full hermetic suite**

Run: `. "$HOME/.cargo/env" && cargo test`
Expected: all non-ignored tests PASS.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml examples/demo_gemini.rs tests/acp_transport.rs
git commit -m "feat: gemini demo + ignored real-gemini smoke test"
```

---

## Notes / risks for the implementer

- **`stderr` is noise:** gemini prints YOLO/Ripgrep/Skill warnings to stderr; we set `Stdio::null()`, so stdout stays clean JSON-RPC. The reader skips any non-JSON line defensively anyway.
- **Events during `open` are dropped:** `session/set_mode` and `session/load` can emit `agent_message_chunk` notifications (e.g. "[MODE_UPDATE] yolo", replayed history). They arrive before any turn sink is installed, so the client drops them. Correct — only real-turn events reach the stream.
- **`session_id` is ignored by ACP:** ACP mints its own `sessionId` at `session/new`. That value (not roy's uuid) is the resume cursor, surfaced via `AcpHandle::resume_cursor()` and stored by `Session`. To resume a gemini session later, pass that ACP sessionId to `Session::resume`.
- **Auth:** if `session/new` errors (not logged in), `RoyError::Protocol` carries the agent's error text. Full interactive OAuth is out of scope — the user logs in once via `gemini`.
- **No model selection / attachments / cost:** out of scope this iteration (see spec).
