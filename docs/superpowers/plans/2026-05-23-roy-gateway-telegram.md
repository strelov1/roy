# roy-gateway: Telegram (v1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** ship a `roy-gateway` bin that bridges Telegram DMs ↔ a roy session: on inbound message, resume (or spawn) a roy session bound to that chat and reply with the assistant's final text.

**Architecture:** New workspace crate `crates/roy-gateway` (lib + one bin). The bin holds a long-lived process that runs a teloxide bot, maps `chat_id → roy session_id` in a JSON file, and drives the roy daemon via `ClientCommand::Fire` over Unix socket. Orchestration logic is behind two thin traits (`Fire`, `Replier`) so the message-handling pipeline is unit-tested with `tokio::io::duplex` and an in-memory replier. No `Channel` trait extraction in this iteration — that comes when a second channel (Slack) is added.

**Tech Stack:** `roy` (in-workspace), `teloxide` 0.13, `tokio` 1, `serde` / `serde_json` / `toml`, `async-trait`, `tracing` + `tracing-subscriber`, `clap` 4, `anyhow`.

**Explicitly NOT in this iteration** (keep the diff small; revisit after end-to-end works):
- Streaming partial `AssistantText` via Telegram `editMessageText` (use `Fire` + final reply only).
- Inbound debouncing for fast typers.
- `chat-abort` (cancel turn from user `/cancel`).
- Slack / Discord / any second channel; therefore no `Channel` trait.
- `sled` / `sqlite`; the binder is a flat JSON file.

---

## File Structure

Created:
- `crates/roy-gateway/Cargo.toml`
- `crates/roy-gateway/src/lib.rs` — module re-exports + crate-level docs.
- `crates/roy-gateway/src/config.rs` — `GatewayConfig` struct + TOML loader.
- `crates/roy-gateway/src/binder.rs` — `SessionBinder`: `(chat_id) → session_id` persistent JSON map.
- `crates/roy-gateway/src/daemon.rs` — `DaemonClient` + `FireOutcome`; transport-agnostic `fire_via_stream` for tests.
- `crates/roy-gateway/src/orchestrator.rs` — `Fire` + `Replier` traits, `handle_message(...)` pure pipeline.
- `crates/roy-gateway/src/telegram.rs` — teloxide bot setup + `TeloxideReplier` impl + per-update dispatcher.
- `crates/roy-gateway/src/main.rs` — CLI args (`--config`), tracing init, build wiring, run.
- `crates/roy-gateway/README.md` — install, config sample, run instructions.
- `crates/roy-gateway/tests/binder_persistence.rs` — integration test for binder reload across processes (well, across `SessionBinder::load` calls).

Modified:
- `Cargo.toml` (workspace root) — already uses `members = ["crates/*"]`, no change needed; verify in task 1.

Each file has one responsibility. `daemon.rs` and `orchestrator.rs` carry their unit tests inline (`#[cfg(test)] mod tests`). `binder.rs` has both inline unit tests and a separate integration test for the reload path.

---

## Task 1: Scaffold the crate

**Files:**
- Create: `crates/roy-gateway/Cargo.toml`
- Create: `crates/roy-gateway/src/lib.rs`
- Create: `crates/roy-gateway/src/main.rs`

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "roy-gateway"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "roy-gateway"
path = "src/main.rs"
doc = false

[dependencies]
roy = { path = "../roy" }
tokio = { version = "1", features = ["macros", "rt-multi-thread", "io-util", "net", "sync", "time", "fs", "signal"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
async-trait = "0.1"
anyhow = "1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
clap = { version = "4", features = ["derive"] }
teloxide = { version = "0.13", default-features = false, features = ["macros", "ctrlc_handler", "rustls"] }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 2: Create minimal lib.rs**

```rust
//! Roy → chat-platform gateway. v1 supports a single channel: Telegram.
//!
//! Architecture: one long-lived process per gateway, talks to a running
//! `roy serve` daemon over its Unix socket using `ClientCommand::Fire`.
//! `(chat_id → roy session_id)` is persisted in a JSON file so chats
//! survive restarts.
//!
//! See `docs/superpowers/plans/2026-05-23-roy-gateway-telegram.md`.

pub mod binder;
pub mod config;
pub mod daemon;
pub mod orchestrator;
pub mod telegram;
```

- [ ] **Step 3: Create stub main.rs that compiles**

```rust
fn main() {
    eprintln!("roy-gateway: stub — see plan task 9 for wiring");
    std::process::exit(2);
}
```

- [ ] **Step 4: Create empty module stubs**

```bash
for f in binder config daemon orchestrator telegram; do
  echo "//! TODO: implemented in later tasks" > "crates/roy-gateway/src/$f.rs"
done
```

- [ ] **Step 5: Build the workspace to confirm crate is picked up**

Run: `cargo build -p roy-gateway`
Expected: PASS (`Compiling roy-gateway v0.1.0`). No warnings about an orphan crate.

- [ ] **Step 6: Commit**

```bash
git add crates/roy-gateway docs/superpowers/plans/2026-05-23-roy-gateway-telegram.md
git commit -m "feat(roy-gateway): scaffold crate"
```

---

## Task 2: Config + TOML loader

**Files:**
- Modify: `crates/roy-gateway/src/config.rs`

- [ ] **Step 1: Write the failing test**

Append at the bottom of `crates/roy-gateway/src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_config() {
        let raw = r#"
            [daemon]
            socket = "/tmp/roy.sock"

            [telegram]
            token = "1234:abc"
            allowed_user_ids = [1, 2]
            preset = "claude"
            cwd = "/Users/me/proj"
            turn_timeout_secs = 300

            [binder]
            path = "/tmp/binder.json"
        "#;
        let cfg: GatewayConfig = toml::from_str(raw).unwrap();
        assert_eq!(cfg.daemon.socket.as_deref(), Some("/tmp/roy.sock"));
        assert_eq!(cfg.telegram.token, "1234:abc");
        assert_eq!(cfg.telegram.allowed_user_ids, vec![1, 2]);
        assert_eq!(cfg.telegram.preset, "claude");
        assert_eq!(cfg.telegram.cwd.as_deref(), Some("/Users/me/proj"));
        assert_eq!(cfg.telegram.turn_timeout_secs, 300);
        assert_eq!(cfg.binder.path, "/tmp/binder.json");
    }

    #[test]
    fn parse_minimal_config_uses_defaults() {
        let raw = r#"
            [telegram]
            token = "x"
            preset = "claude"

            [binder]
            path = "/tmp/b.json"
        "#;
        let cfg: GatewayConfig = toml::from_str(raw).unwrap();
        assert!(cfg.daemon.socket.is_none());
        assert!(cfg.telegram.allowed_user_ids.is_empty());
        assert!(cfg.telegram.cwd.is_none());
        assert_eq!(cfg.telegram.turn_timeout_secs, 600);
    }
}
```

- [ ] **Step 2: Run test, confirm it fails (no `GatewayConfig` yet)**

Run: `cargo test -p roy-gateway --lib config::tests`
Expected: FAIL (`cannot find type GatewayConfig in this scope`).

- [ ] **Step 3: Implement the config**

Replace the contents of `crates/roy-gateway/src/config.rs` with:

```rust
//! TOML configuration loaded from a single file (typically
//! `~/.config/roy-gateway/config.toml`).

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct GatewayConfig {
    #[serde(default)]
    pub daemon: DaemonConfig,
    pub telegram: TelegramConfig,
    pub binder: BinderConfig,
}

#[derive(Debug, Default, Deserialize)]
pub struct DaemonConfig {
    /// Override for the roy daemon Unix socket. When `None`, fall back to
    /// `ROY_SOCKET` env var, then `~/.roy/daemon.sock`.
    #[serde(default)]
    pub socket: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramConfig {
    pub token: String,
    /// If empty, all users may DM the bot. Otherwise, only listed numeric
    /// Telegram user ids may.
    #[serde(default)]
    pub allowed_user_ids: Vec<u64>,
    /// roy preset to spawn for new chats (`claude` / `gemini` / `opencode` / `codex`).
    pub preset: String,
    /// Working directory for spawned sessions. `None` → daemon picks its
    /// default (`ROY_CWD` / daemon cwd).
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default = "default_turn_timeout_secs")]
    pub turn_timeout_secs: u64,
}

fn default_turn_timeout_secs() -> u64 {
    600
}

#[derive(Debug, Deserialize)]
pub struct BinderConfig {
    /// Path to the JSON file holding `chat_id → session_id`.
    pub path: String,
}

impl GatewayConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let cfg: Self =
            toml::from_str(&raw).with_context(|| format!("parsing config {}", path.display()))?;
        Ok(cfg)
    }
}
```

- [ ] **Step 4: Run tests, confirm pass**

Run: `cargo test -p roy-gateway --lib config::tests`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/roy-gateway/src/config.rs
git commit -m "feat(roy-gateway): config struct + TOML loader"
```

---

## Task 3: SessionBinder (JSON-backed persistent map)

**Files:**
- Modify: `crates/roy-gateway/src/binder.rs`
- Create: `crates/roy-gateway/tests/binder_persistence.rs`

- [ ] **Step 1: Write the failing inline unit tests**

Replace `crates/roy-gateway/src/binder.rs` contents with:

```rust
//! Persistent `chat_id → roy session_id` map, backed by one JSON file.
//!
//! Writes are serialized through an in-memory `tokio::sync::Mutex`; each
//! mutation rewrites the whole file (atomic via `tempfile` + rename).
//! That is fine at chat-bot scale (low write rate, few hundred entries
//! at most). If volume ever justifies it, swap in sled later — the
//! `SessionBinder` API surface is the migration boundary.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// chat_id → roy session_id
    bindings: HashMap<i64, String>,
}

#[derive(Debug)]
pub struct SessionBinder {
    path: PathBuf,
    state: Mutex<State>,
}

impl SessionBinder {
    /// Load existing bindings, or initialize empty if the file does not exist.
    pub async fn load(path: PathBuf) -> Result<Self> {
        let state = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice::<State>(&bytes)
                .with_context(|| format!("parsing {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => {
                return Err(anyhow::Error::new(e)
                    .context(format!("reading {}", path.display())));
            }
        };
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    pub async fn get(&self, chat_id: i64) -> Option<String> {
        self.state.lock().await.bindings.get(&chat_id).cloned()
    }

    pub async fn set(&self, chat_id: i64, session_id: String) -> Result<()> {
        let mut guard = self.state.lock().await;
        guard.bindings.insert(chat_id, session_id);
        Self::persist(&self.path, &*guard).await
    }

    pub async fn forget(&self, chat_id: i64) -> Result<()> {
        let mut guard = self.state.lock().await;
        guard.bindings.remove(&chat_id);
        Self::persist(&self.path, &*guard).await
    }

    async fn persist(path: &std::path::Path, state: &State) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(state).context("serializing binder")?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await.with_context(|| {
                    format!("creating binder dir {}", parent.display())
                })?;
            }
        }
        let tmp = path.with_extension("tmp");
        tokio::fs::write(&tmp, &bytes)
            .await
            .with_context(|| format!("writing {}", tmp.display()))?;
        tokio::fs::rename(&tmp, path)
            .await
            .with_context(|| format!("renaming into {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn empty_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let binder = SessionBinder::load(dir.path().join("missing.json"))
            .await
            .unwrap();
        assert!(binder.get(42).await.is_none());
    }

    #[tokio::test]
    async fn set_then_get() {
        let dir = tempfile::tempdir().unwrap();
        let binder = SessionBinder::load(dir.path().join("b.json"))
            .await
            .unwrap();
        binder.set(7, "sess-1".into()).await.unwrap();
        assert_eq!(binder.get(7).await.as_deref(), Some("sess-1"));
    }

    #[tokio::test]
    async fn forget_removes() {
        let dir = tempfile::tempdir().unwrap();
        let binder = SessionBinder::load(dir.path().join("b.json"))
            .await
            .unwrap();
        binder.set(7, "sess-1".into()).await.unwrap();
        binder.forget(7).await.unwrap();
        assert!(binder.get(7).await.is_none());
    }
}
```

- [ ] **Step 2: Write the persistence-across-load integration test**

Create `crates/roy-gateway/tests/binder_persistence.rs`:

```rust
use roy_gateway::binder::SessionBinder;

#[tokio::test]
async fn bindings_survive_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.json");

    {
        let binder = SessionBinder::load(path.clone()).await.unwrap();
        binder.set(1, "alpha".into()).await.unwrap();
        binder.set(2, "beta".into()).await.unwrap();
    }

    let reloaded = SessionBinder::load(path).await.unwrap();
    assert_eq!(reloaded.get(1).await.as_deref(), Some("alpha"));
    assert_eq!(reloaded.get(2).await.as_deref(), Some("beta"));
}
```

- [ ] **Step 3: Run all binder tests**

Run: `cargo test -p roy-gateway binder`
Expected: PASS (3 unit tests + 1 integration test).

- [ ] **Step 4: Commit**

```bash
git add crates/roy-gateway/src/binder.rs crates/roy-gateway/tests/binder_persistence.rs
git commit -m "feat(roy-gateway): persistent chat→session binder"
```

---

## Task 4: DaemonClient — Fire spawn returns Done

**Files:**
- Modify: `crates/roy-gateway/src/daemon.rs`

This task introduces the transport-agnostic core (`fire_via_stream`) and one happy path. Task 5 adds resume, task 6 adds error/timeout outcomes.

- [ ] **Step 1: Write the failing test**

Replace `crates/roy-gateway/src/daemon.rs` contents with:

```rust
//! Client to the roy daemon over its Unix socket. Wraps a single
//! `ClientCommand::Fire` (composite Spawn-or-Resume + WaitForResult)
//! so the gateway can stay synchronous-per-message at the daemon API.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use roy::control::{ClientCommand, ErrorCode, FireTarget, ServerEvent};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

const DEFAULT_TIMEOUT_MS: u64 = 600_000;

#[derive(Debug, Clone)]
pub enum FireOutcome {
    Done {
        session: String,
        assistant_text: String,
    },
    Timeout {
        session: Option<String>,
    },
    Error {
        session: Option<String>,
        code: ErrorCode,
        message: String,
    },
}

#[derive(Debug, Clone)]
pub struct DaemonClient {
    socket_path: PathBuf,
}

impl DaemonClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }
}

/// Transport-agnostic Fire. Public for tests; production code goes through
/// `DaemonClient::fire_spawn` / `fire_resume` below (added in tasks 4–5).
pub async fn fire_via_stream<S>(
    stream: S,
    target: FireTarget,
    prompt: String,
    tags: BTreeMap<String, String>,
    timeout: Duration,
) -> Result<FireOutcome>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut lines = BufReader::new(reader).lines();

    let cmd = ClientCommand::Fire {
        target,
        prompt,
        tags,
        timeout_ms: Some(timeout.as_millis() as u64),
    };
    let line = serde_json::to_string(&cmd).context("serializing Fire")?;
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;

    let Some(raw) = lines.next_line().await? else {
        return Err(anyhow!("daemon closed connection before Fire response"));
    };
    let evt: ServerEvent =
        serde_json::from_str(&raw).with_context(|| format!("parsing daemon line: {raw}"))?;
    match evt {
        ServerEvent::FireDone {
            session,
            assistant_text,
            ..
        } => Ok(FireOutcome::Done {
            session,
            assistant_text,
        }),
        ServerEvent::FireTimeout { session, .. } => Ok(FireOutcome::Timeout {
            session: Some(session),
        }),
        ServerEvent::FireError {
            session,
            code,
            message,
        } => Ok(FireOutcome::Error {
            session,
            code,
            message,
        }),
        ServerEvent::Error {
            session,
            code,
            message,
        } => Ok(FireOutcome::Error {
            session,
            code,
            message,
        }),
        other => Err(anyhow!("unexpected daemon event for Fire: {other:?}")),
    }
}

impl DaemonClient {
    pub async fn fire_spawn(
        &self,
        preset: &str,
        cwd: Option<String>,
        prompt: String,
        tags: BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<FireOutcome> {
        let stream = tokio::net::UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| format!("connecting to daemon at {}", self.socket_path.display()))?;
        fire_via_stream(
            stream,
            FireTarget::Spawn {
                preset: preset.into(),
                cwd,
            },
            prompt,
            tags,
            timeout,
        )
        .await
    }
}

#[allow(dead_code)]
fn _default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

#[cfg(test)]
mod tests {
    use super::*;
    use roy::event::{StopReason, TurnEvent};
    use tokio::io::AsyncWriteExt;

    /// Spawn a fake daemon on one half of a duplex pair. It reads exactly one
    /// JSON line, asserts via the caller-supplied closure, then writes the
    /// caller-supplied response.
    async fn fake_daemon<F>(server: tokio::io::DuplexStream, response: ServerEvent, assert_cmd: F)
    where
        F: FnOnce(ClientCommand) + Send + 'static,
    {
        let (r, mut w) = tokio::io::split(server);
        let mut lines = BufReader::new(r).lines();
        let raw = lines.next_line().await.unwrap().unwrap();
        let cmd: ClientCommand = serde_json::from_str(&raw).unwrap();
        assert_cmd(cmd);
        let line = serde_json::to_string(&response).unwrap();
        w.write_all(line.as_bytes()).await.unwrap();
        w.write_all(b"\n").await.unwrap();
        w.flush().await.unwrap();
    }

    #[tokio::test]
    async fn fire_spawn_returns_done() {
        let (client, server) = tokio::io::duplex(8192);
        tokio::spawn(fake_daemon(
            server,
            ServerEvent::FireDone {
                session: "abc".into(),
                seq_range: (0, 5),
                result: TurnEvent::Result {
                    cost_usd: None,
                    stop_reason: StopReason::EndTurn,
                },
                assistant_text: "hello world".into(),
            },
            |cmd| match cmd {
                ClientCommand::Fire {
                    target: FireTarget::Spawn { preset, cwd },
                    prompt,
                    timeout_ms,
                    ..
                } => {
                    assert_eq!(preset, "claude");
                    assert_eq!(cwd.as_deref(), Some("/tmp/proj"));
                    assert_eq!(prompt, "ping");
                    assert_eq!(timeout_ms, Some(30_000));
                }
                other => panic!("expected Fire::Spawn, got {other:?}"),
            },
        ));

        let out = fire_via_stream(
            client,
            FireTarget::Spawn {
                preset: "claude".into(),
                cwd: Some("/tmp/proj".into()),
            },
            "ping".into(),
            BTreeMap::new(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();

        match out {
            FireOutcome::Done {
                session,
                assistant_text,
            } => {
                assert_eq!(session, "abc");
                assert_eq!(assistant_text, "hello world");
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run the test, confirm it fails to compile**

Run: `cargo test -p roy-gateway --lib daemon::tests`
Expected: build error mentioning `roy::control::FireTarget` already exists but maybe `roy::event` export missing. Confirm whether `roy::event` is public.

- [ ] **Step 3: If `roy::event` is not re-exported, expose it**

Check `crates/roy/src/lib.rs`. If `event` is not `pub mod event;`, make it `pub`. Same for `control`. Confirm both compile:

Run: `cargo build -p roy`
Expected: PASS.

- [ ] **Step 4: Run the daemon test, confirm PASS**

Run: `cargo test -p roy-gateway --lib daemon::tests::fire_spawn_returns_done`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-gateway/src/daemon.rs crates/roy/src/lib.rs
git commit -m "feat(roy-gateway): DaemonClient.fire_spawn (Done path)"
```

---

## Task 5: DaemonClient — Fire resume

**Files:**
- Modify: `crates/roy-gateway/src/daemon.rs`

- [ ] **Step 1: Add the failing test**

Append to the `tests` mod in `crates/roy-gateway/src/daemon.rs`:

```rust
    #[tokio::test]
    async fn fire_resume_returns_done() {
        let (client, server) = tokio::io::duplex(8192);
        tokio::spawn(fake_daemon(
            server,
            ServerEvent::FireDone {
                session: "abc".into(),
                seq_range: (10, 15),
                result: TurnEvent::Result {
                    cost_usd: None,
                    stop_reason: StopReason::EndTurn,
                },
                assistant_text: "resumed reply".into(),
            },
            |cmd| match cmd {
                ClientCommand::Fire {
                    target: FireTarget::Resume { session_id },
                    prompt,
                    ..
                } => {
                    assert_eq!(session_id, "abc");
                    assert_eq!(prompt, "follow-up");
                }
                other => panic!("expected Fire::Resume, got {other:?}"),
            },
        ));

        let out = fire_via_stream(
            client,
            FireTarget::Resume {
                session_id: "abc".into(),
            },
            "follow-up".into(),
            BTreeMap::new(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();

        assert!(matches!(out, FireOutcome::Done { session, assistant_text }
            if session == "abc" && assistant_text == "resumed reply"));
    }
```

- [ ] **Step 2: Add `fire_resume` to `DaemonClient`**

Append to the `impl DaemonClient` block:

```rust
    pub async fn fire_resume(
        &self,
        session_id: &str,
        prompt: String,
        tags: BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<FireOutcome> {
        let stream = tokio::net::UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| format!("connecting to daemon at {}", self.socket_path.display()))?;
        fire_via_stream(
            stream,
            FireTarget::Resume {
                session_id: session_id.into(),
            },
            prompt,
            tags,
            timeout,
        )
        .await
    }
```

- [ ] **Step 3: Run daemon tests, confirm PASS**

Run: `cargo test -p roy-gateway --lib daemon::tests`
Expected: PASS (both spawn and resume).

- [ ] **Step 4: Commit**

```bash
git add crates/roy-gateway/src/daemon.rs
git commit -m "feat(roy-gateway): DaemonClient.fire_resume"
```

---

## Task 6: DaemonClient — error + timeout outcomes

**Files:**
- Modify: `crates/roy-gateway/src/daemon.rs`

- [ ] **Step 1: Add tests for FireError, FireTimeout, ServerEvent::Error**

Append to the `tests` mod:

```rust
    #[tokio::test]
    async fn fire_error_is_returned_verbatim() {
        let (client, server) = tokio::io::duplex(8192);
        tokio::spawn(fake_daemon(
            server,
            ServerEvent::FireError {
                session: Some("sid".into()),
                code: ErrorCode::SpawnFailed,
                message: "agent crashed".into(),
            },
            |_| {},
        ));
        let out = fire_via_stream(
            client,
            FireTarget::Spawn {
                preset: "claude".into(),
                cwd: None,
            },
            "x".into(),
            BTreeMap::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        match out {
            FireOutcome::Error {
                session,
                code,
                message,
            } => {
                assert_eq!(session.as_deref(), Some("sid"));
                assert_eq!(code, ErrorCode::SpawnFailed);
                assert_eq!(message, "agent crashed");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fire_timeout_is_mapped() {
        let (client, server) = tokio::io::duplex(8192);
        tokio::spawn(fake_daemon(
            server,
            ServerEvent::FireTimeout {
                session: "sid".into(),
                partial_seq_range: (0, 0),
            },
            |_| {},
        ));
        let out = fire_via_stream(
            client,
            FireTarget::Resume {
                session_id: "sid".into(),
            },
            "x".into(),
            BTreeMap::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(matches!(out, FireOutcome::Timeout { session } if session.as_deref() == Some("sid")));
    }

    #[tokio::test]
    async fn generic_error_event_is_mapped() {
        let (client, server) = tokio::io::duplex(8192);
        tokio::spawn(fake_daemon(
            server,
            ServerEvent::Error {
                session: None,
                code: ErrorCode::BadRequest,
                message: "nope".into(),
            },
            |_| {},
        ));
        let out = fire_via_stream(
            client,
            FireTarget::Spawn {
                preset: "claude".into(),
                cwd: None,
            },
            "x".into(),
            BTreeMap::new(),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(matches!(out, FireOutcome::Error { code: ErrorCode::BadRequest, .. }));
    }
```

- [ ] **Step 2: Run tests, confirm PASS**

Run: `cargo test -p roy-gateway --lib daemon::tests`
Expected: PASS (5 tests now).

> Note: no implementation change is needed — `fire_via_stream` from task 4 already covers these branches. If any test fails because of a missing branch, fix `fire_via_stream` to cover it before moving on.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-gateway/src/daemon.rs
git commit -m "test(roy-gateway): cover Fire error and timeout outcomes"
```

---

## Task 7: Orchestrator (traits + handle_message)

**Files:**
- Modify: `crates/roy-gateway/src/orchestrator.rs`

This is the pure pipeline that the Telegram layer will call. `Fire` and `Replier` traits keep it testable without a real daemon or a real bot.

- [ ] **Step 1: Write the failing test**

Replace `crates/roy-gateway/src/orchestrator.rs` contents with:

```rust
//! The pipeline that turns one inbound chat message into one outbound reply.
//!
//! Stateless except for the `SessionBinder`. The two traits below are the
//! seams against which we mock for unit tests; production wires
//! `DaemonClient` to `Fire` and `TeloxideReplier` to `Replier`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

use crate::binder::SessionBinder;
use crate::daemon::FireOutcome;

#[async_trait]
pub trait Fire: Send + Sync {
    async fn fire_spawn(
        &self,
        preset: &str,
        cwd: Option<String>,
        prompt: String,
        tags: BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<FireOutcome>;

    async fn fire_resume(
        &self,
        session_id: &str,
        prompt: String,
        tags: BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<FireOutcome>;
}

#[async_trait]
pub trait Replier: Send + Sync {
    async fn send(&self, chat_id: i64, text: &str) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    pub preset: String,
    pub cwd: Option<String>,
    pub turn_timeout: Duration,
}

pub async fn handle_message<F, R>(
    cfg: &OrchestratorConfig,
    binder: &Arc<SessionBinder>,
    fire: &F,
    replier: &R,
    chat_id: i64,
    prompt: String,
) -> Result<()>
where
    F: Fire,
    R: Replier,
{
    let mut tags = BTreeMap::new();
    tags.insert("channel".into(), "telegram".into());
    tags.insert("chat_id".into(), chat_id.to_string());

    let outcome = match binder.get(chat_id).await {
        Some(session_id) => {
            fire.fire_resume(&session_id, prompt, tags, cfg.turn_timeout)
                .await?
        }
        None => {
            fire.fire_spawn(
                &cfg.preset,
                cfg.cwd.clone(),
                prompt,
                tags,
                cfg.turn_timeout,
            )
            .await?
        }
    };

    match outcome {
        FireOutcome::Done {
            session,
            assistant_text,
        } => {
            binder.set(chat_id, session).await?;
            let text = if assistant_text.is_empty() {
                "(empty reply)".to_string()
            } else {
                assistant_text
            };
            replier.send(chat_id, &text).await?;
        }
        FireOutcome::Timeout { session } => {
            if let Some(s) = session {
                binder.set(chat_id, s).await?;
            }
            replier
                .send(chat_id, "⏱ turn timed out — send another message to continue")
                .await?;
        }
        FireOutcome::Error {
            session,
            code,
            message,
        } => {
            if let Some(s) = session {
                binder.set(chat_id, s).await?;
            }
            replier
                .send(chat_id, &format!("⚠ {code}: {message}"))
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use roy::control::ErrorCode;
    use std::sync::Mutex;
    use tempfile::TempDir;

    struct MockFire {
        on_spawn: Mutex<Option<FireOutcome>>,
        on_resume: Mutex<Option<FireOutcome>>,
        last_spawn: Mutex<Option<(String, Option<String>, String)>>,
        last_resume: Mutex<Option<(String, String)>>,
    }

    impl MockFire {
        fn new() -> Self {
            Self {
                on_spawn: Mutex::new(None),
                on_resume: Mutex::new(None),
                last_spawn: Mutex::new(None),
                last_resume: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl Fire for MockFire {
        async fn fire_spawn(
            &self,
            preset: &str,
            cwd: Option<String>,
            prompt: String,
            _tags: BTreeMap<String, String>,
            _timeout: Duration,
        ) -> Result<FireOutcome> {
            *self.last_spawn.lock().unwrap() = Some((preset.into(), cwd, prompt));
            Ok(self.on_spawn.lock().unwrap().take().expect("on_spawn not set"))
        }
        async fn fire_resume(
            &self,
            session_id: &str,
            prompt: String,
            _tags: BTreeMap<String, String>,
            _timeout: Duration,
        ) -> Result<FireOutcome> {
            *self.last_resume.lock().unwrap() = Some((session_id.into(), prompt));
            Ok(self.on_resume.lock().unwrap().take().expect("on_resume not set"))
        }
    }

    #[derive(Default)]
    struct MockReplier {
        sent: tokio::sync::Mutex<Vec<(i64, String)>>,
    }

    #[async_trait]
    impl Replier for MockReplier {
        async fn send(&self, chat_id: i64, text: &str) -> Result<()> {
            self.sent.lock().await.push((chat_id, text.into()));
            Ok(())
        }
    }

    async fn fresh_binder(dir: &TempDir) -> Arc<SessionBinder> {
        Arc::new(SessionBinder::load(dir.path().join("b.json")).await.unwrap())
    }

    fn cfg() -> OrchestratorConfig {
        OrchestratorConfig {
            preset: "claude".into(),
            cwd: Some("/tmp/proj".into()),
            turn_timeout: Duration::from_secs(60),
        }
    }

    #[tokio::test]
    async fn unbound_chat_spawns_and_persists_session() {
        let dir = tempfile::tempdir().unwrap();
        let binder = fresh_binder(&dir).await;
        let fire = MockFire::new();
        *fire.on_spawn.lock().unwrap() = Some(FireOutcome::Done {
            session: "sess-new".into(),
            assistant_text: "hi".into(),
        });
        let replier = MockReplier::default();

        handle_message(&cfg(), &binder, &fire, &replier, 42, "hello".into())
            .await
            .unwrap();

        let last = fire.last_spawn.lock().unwrap().clone().unwrap();
        assert_eq!(last.0, "claude");
        assert_eq!(last.1.as_deref(), Some("/tmp/proj"));
        assert_eq!(last.2, "hello");
        assert_eq!(binder.get(42).await.as_deref(), Some("sess-new"));
        let sent = replier.sent.lock().await.clone();
        assert_eq!(sent, vec![(42, "hi".to_string())]);
    }

    #[tokio::test]
    async fn bound_chat_resumes_existing_session() {
        let dir = tempfile::tempdir().unwrap();
        let binder = fresh_binder(&dir).await;
        binder.set(42, "sess-old".into()).await.unwrap();

        let fire = MockFire::new();
        *fire.on_resume.lock().unwrap() = Some(FireOutcome::Done {
            session: "sess-old".into(),
            assistant_text: "continued".into(),
        });
        let replier = MockReplier::default();

        handle_message(&cfg(), &binder, &fire, &replier, 42, "more".into())
            .await
            .unwrap();

        let last = fire.last_resume.lock().unwrap().clone().unwrap();
        assert_eq!(last.0, "sess-old");
        assert_eq!(last.1, "more");
        let sent = replier.sent.lock().await.clone();
        assert_eq!(sent, vec![(42, "continued".to_string())]);
    }

    #[tokio::test]
    async fn fire_error_is_reported_to_chat() {
        let dir = tempfile::tempdir().unwrap();
        let binder = fresh_binder(&dir).await;
        let fire = MockFire::new();
        *fire.on_spawn.lock().unwrap() = Some(FireOutcome::Error {
            session: None,
            code: ErrorCode::SpawnFailed,
            message: "boom".into(),
        });
        let replier = MockReplier::default();

        handle_message(&cfg(), &binder, &fire, &replier, 42, "hi".into())
            .await
            .unwrap();

        let sent = replier.sent.lock().await.clone();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].1.contains("spawn_failed"));
        assert!(sent[0].1.contains("boom"));
        assert!(binder.get(42).await.is_none());
    }

    #[tokio::test]
    async fn empty_assistant_text_falls_back_to_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let binder = fresh_binder(&dir).await;
        let fire = MockFire::new();
        *fire.on_spawn.lock().unwrap() = Some(FireOutcome::Done {
            session: "s".into(),
            assistant_text: "".into(),
        });
        let replier = MockReplier::default();

        handle_message(&cfg(), &binder, &fire, &replier, 1, "hi".into())
            .await
            .unwrap();

        let sent = replier.sent.lock().await.clone();
        assert_eq!(sent, vec![(1, "(empty reply)".to_string())]);
    }
}
```

- [ ] **Step 2: Run tests, confirm PASS**

Run: `cargo test -p roy-gateway --lib orchestrator::tests`
Expected: PASS (4 tests).

- [ ] **Step 3: Commit**

```bash
git add crates/roy-gateway/src/orchestrator.rs
git commit -m "feat(roy-gateway): handle_message orchestration with mockable seams"
```

---

## Task 8: Telegram glue (teloxide impl)

**Files:**
- Modify: `crates/roy-gateway/src/telegram.rs`

There is no clean unit test for the teloxide handler itself (it owns a global dispatcher tree). This task wires the bot loop, implements `Replier` and `Fire` against the production types, and per-message calls `handle_message` (which IS unit-tested in task 7). Verification is a build + manual smoke (task 10).

- [ ] **Step 1: Implement the telegram module**

Replace `crates/roy-gateway/src/telegram.rs` contents with:

```rust
//! Teloxide bot loop: receive text DMs, route through the orchestrator,
//! reply with the assistant's final text.
//!
//! Only direct messages from text are handled in v1. Group chats,
//! commands, photos, edits — all ignored. Bot owner can add the bot
//! to a group; it will simply do nothing there until v2.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use teloxide::prelude::*;
use teloxide::types::{ChatId, ParseMode};

use crate::binder::SessionBinder;
use crate::daemon::{DaemonClient, FireOutcome};
use crate::orchestrator::{handle_message, Fire, OrchestratorConfig, Replier};

#[async_trait]
impl Fire for DaemonClient {
    async fn fire_spawn(
        &self,
        preset: &str,
        cwd: Option<String>,
        prompt: String,
        tags: std::collections::BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<FireOutcome> {
        DaemonClient::fire_spawn(self, preset, cwd, prompt, tags, timeout).await
    }
    async fn fire_resume(
        &self,
        session_id: &str,
        prompt: String,
        tags: std::collections::BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<FireOutcome> {
        DaemonClient::fire_resume(self, session_id, prompt, tags, timeout).await
    }
}

pub struct TeloxideReplier {
    bot: Bot,
}

impl TeloxideReplier {
    pub fn new(bot: Bot) -> Self {
        Self { bot }
    }
}

#[async_trait]
impl Replier for TeloxideReplier {
    async fn send(&self, chat_id: i64, text: &str) -> Result<()> {
        self.bot.send_message(ChatId(chat_id), text).await?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct BotDeps {
    pub cfg: Arc<OrchestratorConfig>,
    pub binder: Arc<SessionBinder>,
    pub daemon: Arc<DaemonClient>,
    pub replier: Arc<TeloxideReplier>,
    pub allowed_user_ids: Arc<HashSet<u64>>,
}

/// Spawn the bot and block until ctrl-C / hangup.
pub async fn run(token: String, deps: BotDeps) -> Result<()> {
    let bot = Bot::new(token);
    tracing::info!("starting teloxide dispatcher");

    let handler = Update::filter_message().endpoint(
        move |bot: Bot, msg: Message, deps: BotDeps| async move {
            if let Err(e) = on_message(&bot, &msg, &deps).await {
                tracing::warn!(?e, chat_id = msg.chat.id.0, "message handler failed");
            }
            respond(())
        },
    );

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![deps])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    Ok(())
}

async fn on_message(_bot: &Bot, msg: &Message, deps: &BotDeps) -> Result<()> {
    let Some(text) = msg.text() else {
        return Ok(());
    };
    let Some(from) = msg.from.as_ref() else {
        return Ok(());
    };
    let user_id = from.id.0;
    if !deps.allowed_user_ids.is_empty() && !deps.allowed_user_ids.contains(&user_id) {
        tracing::debug!(user_id, "rejecting non-allowlisted sender");
        return Ok(());
    }

    handle_message(
        &deps.cfg,
        &deps.binder,
        deps.daemon.as_ref(),
        deps.replier.as_ref(),
        msg.chat.id.0,
        text.to_string(),
    )
    .await
}

// silence unused-imports warning if teloxide bumps ParseMode out of usage
#[allow(dead_code)]
const _PARSE_MODE_REF: ParseMode = ParseMode::MarkdownV2;
```

- [ ] **Step 2: Build, confirm everything compiles**

Run: `cargo build -p roy-gateway`
Expected: PASS (one teloxide dep download on first build).

- [ ] **Step 3: Run full crate tests, confirm nothing regressed**

Run: `cargo test -p roy-gateway`
Expected: PASS (all earlier tests + clean build).

- [ ] **Step 4: Commit**

```bash
git add crates/roy-gateway/src/telegram.rs
git commit -m "feat(roy-gateway): teloxide bot loop wired to orchestrator"
```

---

## Task 9: main.rs wiring

**Files:**
- Modify: `crates/roy-gateway/src/main.rs`

- [ ] **Step 1: Replace stub main with the real wiring**

Replace `crates/roy-gateway/src/main.rs` contents with:

```rust
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use teloxide::Bot;
use tracing_subscriber::EnvFilter;

use roy_gateway::binder::SessionBinder;
use roy_gateway::config::GatewayConfig;
use roy_gateway::daemon::DaemonClient;
use roy_gateway::orchestrator::OrchestratorConfig;
use roy_gateway::telegram::{run, BotDeps, TeloxideReplier};

#[derive(Parser, Debug)]
#[command(name = "roy-gateway")]
struct Args {
    /// Path to the gateway TOML config.
    #[arg(long)]
    config: PathBuf,
}

fn default_socket() -> PathBuf {
    if let Ok(s) = std::env::var("ROY_SOCKET") {
        return PathBuf::from(s);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".roy/daemon.sock")
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("roy_gateway=info,warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    let cfg = GatewayConfig::load(&args.config)
        .with_context(|| format!("loading {}", args.config.display()))?;

    let socket_path = cfg
        .daemon
        .socket
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(default_socket);
    tracing::info!(socket = %socket_path.display(), "daemon socket");

    let binder_path = PathBuf::from(&cfg.binder.path);
    let binder = Arc::new(SessionBinder::load(binder_path.clone()).await.with_context(|| {
        format!("loading binder {}", binder_path.display())
    })?);

    let daemon = Arc::new(DaemonClient::new(socket_path));

    let orch_cfg = Arc::new(OrchestratorConfig {
        preset: cfg.telegram.preset.clone(),
        cwd: cfg.telegram.cwd.clone(),
        turn_timeout: Duration::from_secs(cfg.telegram.turn_timeout_secs),
    });

    let bot = Bot::new(cfg.telegram.token.clone());
    let replier = Arc::new(TeloxideReplier::new(bot.clone()));

    let allowed: HashSet<u64> = cfg.telegram.allowed_user_ids.iter().copied().collect();
    let deps = BotDeps {
        cfg: orch_cfg,
        binder,
        daemon,
        replier,
        allowed_user_ids: Arc::new(allowed),
    };

    run(cfg.telegram.token, deps).await
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p roy-gateway --bin roy-gateway`
Expected: PASS.

- [ ] **Step 3: Smoke `--help`**

Run: `cargo run -p roy-gateway -- --help`
Expected: clap usage text mentioning `--config <CONFIG>`.

- [ ] **Step 4: Commit**

```bash
git add crates/roy-gateway/src/main.rs
git commit -m "feat(roy-gateway): main.rs wiring + clap + tracing"
```

---

## Task 10: README + manual smoke test

**Files:**
- Create: `crates/roy-gateway/README.md`

- [ ] **Step 1: Write README with config sample and run steps**

Create `crates/roy-gateway/README.md`:

````markdown
# roy-gateway

Bridges chat platforms ↔ a running `roy serve` daemon. v1 supports
**Telegram only**.

## How it works

1. `roy serve` is running and you have a working preset (`claude` /
   `gemini` / `opencode` / `codex`) pre-authenticated.
2. `roy-gateway` runs as a long-lived process. On every inbound text DM:
   - If the chat is new, `Fire { Spawn { preset, cwd } }` is sent to the
     daemon. The returned `session_id` is bound to `chat_id` in a JSON file.
   - If the chat is known, `Fire { Resume { session_id } }` is sent. The
     daemon hands the prompt back through ACP `session/load`.
3. When `FireDone` lands, the assistant's final text is sent to the chat
   as a single Telegram message.

Streaming partials, message edits, debouncing, and `/cancel` are deferred
to v2 — see the plan doc.

## Config

```toml
# ~/.config/roy-gateway/telegram.toml — DO NOT COMMIT (contains bot token)

[daemon]
# Optional; falls back to ROY_SOCKET, then ~/.roy/daemon.sock
# socket = "/Users/me/.roy/daemon.sock"

[telegram]
token = "1234567890:AA…"            # from @BotFather
allowed_user_ids = [123456789]      # empty list = allow anyone
preset = "claude"
cwd = "/Users/me/Projects/scratch"  # optional; daemon default otherwise
turn_timeout_secs = 600

[binder]
path = "/Users/me/.roy/gateway-telegram.json"
```

## Run

```bash
# 1. start the daemon (separately, in its own terminal)
roy serve

# 2. start the gateway
RUST_LOG=roy_gateway=info,info \
  cargo run -p roy-gateway -- --config ~/.config/roy-gateway/telegram.toml
```

## Manual smoke checklist

- [ ] DM your bot. Wait for a reply. Confirm the binder file has one entry.
- [ ] Send a follow-up. Confirm the same `session_id` is reused
      (`jq < ~/.roy/gateway-telegram.json`).
- [ ] Stop the gateway (Ctrl-C). Restart. Send another message. Confirm
      the conversation continues.
- [ ] Stop the daemon. Send a message. Expect a `⚠ …` error reply in
      the chat, gateway keeps running.
- [ ] (If `allowed_user_ids` set) DM from a non-allowlisted account.
      Expect silence and a `rejecting non-allowlisted sender` debug log.
````

- [ ] **Step 2: Run the full workspace test suite, confirm nothing broke**

Run: `cargo test --workspace --no-fail-fast`
Expected: PASS (existing roy tests + new roy-gateway tests).

- [ ] **Step 3: Run fmt + build to mirror CI**

Run: `cargo fmt --all -- --check && cargo build --workspace --all-targets`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/roy-gateway/README.md
git commit -m "docs(roy-gateway): README + manual smoke checklist"
```

---

## Self-review notes

**Spec coverage:**
- Resume-on-message (not keep-alive) → tasks 4, 5, 7 (orchestrator picks Spawn vs Resume).
- Single-crate-with-future-feature-flags shape → task 1 (one crate); explicit "no Channel trait, no second channel" in header.
- Daemon talked to via existing `Fire` composite → tasks 4–6.
- chat_id ↔ session persistence across restarts → tasks 3 + integration test.
- Per-user allowlist → task 8 (`on_message`).
- Tracing + graceful Ctrl-C → task 8 (`enable_ctrlc_handler`) + task 9 (subscriber).
- End-to-end verifiability → task 10 manual checklist.

**Deferred (intentionally absent):**
- Streaming partials, edit-message UX, debouncing, `/cancel`, second channel, `Channel` trait extraction, `sled` migration.

**Placeholder scan:** none — every step has concrete code or a concrete command.

**Type consistency:** `FireOutcome::Done { session, assistant_text }` used identically in tasks 4, 5, 6, 7. `OrchestratorConfig { preset, cwd, turn_timeout }` defined in task 7 and used unchanged in task 9. `Fire` trait signatures in task 7 match `DaemonClient::fire_*` signatures from tasks 4–5; the `impl Fire for DaemonClient` in task 8 just delegates.
