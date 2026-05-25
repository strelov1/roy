# WebSocket gateway Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the WebSocket transport out of the core daemon into `roy-gateway` as a transparent relay, leaving the daemon with a single Unix-socket API surface.

**Architecture:** A new `ws` module in `roy-gateway` accepts authenticated WebSocket connections and, per connection, opens a dedicated Unix-socket connection to the daemon and pumps lines verbatim in both directions (WS `Message::Text` ↔ `\n`-delimited line). The gateway binary becomes config-driven: it starts the Telegram adapter and/or the WS relay based on which TOML sections are present. The daemon loses all WS code.

**Tech Stack:** Rust, tokio, `tokio-tungstenite` 0.24, `futures-util`, `serde`/`serde_json`, `toml`, `clap`.

**Spec:** `docs/superpowers/specs/2026-05-24-ws-gateway-design.md`

---

## File structure

| File | Responsibility |
|------|----------------|
| `crates/roy-gateway/src/ws.rs` | **new** — token load/create + owner-only write helper, WS auth callback, per-connection relay (`relay_connection`), and the listener accept loop (`run_ws_relay`). |
| `crates/roy-gateway/src/config.rs` | `telegram` optional; add `websocket` section; validation. |
| `crates/roy-gateway/src/main.rs` | config-driven startup: spawn Telegram and/or WS relay concurrently. |
| `crates/roy-gateway/src/lib.rs` | `pub mod ws;` |
| `crates/roy-gateway/Cargo.toml` | add `tokio-tungstenite`, `futures-util`, `uuid`, `http` (via tungstenite re-export). |
| `crates/roy/src/daemon.rs` | remove all WS code + WS tests; fix imports. |
| `crates/roy/Cargo.toml` | drop `tokio-tungstenite`. |
| `crates/roy-cli/src/main.rs` | drop `--port` flag and WS wiring. |
| `crates/roy/CLAUDE.md`, `docs/architecture.md`, `docs/wire-protocol.md` | doc updates. |

The plan builds the new gateway WS path **first** (Tasks 1–6, all green and committed) and only then removes WS from the daemon (Tasks 7–9). This keeps the tree compiling at every commit.

---

### Task 1: Add gateway dependencies and the `ws` module skeleton

**Files:**
- Modify: `crates/roy-gateway/Cargo.toml:11-23`
- Create: `crates/roy-gateway/src/ws.rs`
- Modify: `crates/roy-gateway/src/lib.rs:11-19`

- [ ] **Step 1: Add dependencies**

In `crates/roy-gateway/Cargo.toml`, under `[dependencies]`, add three lines after the `teloxide` line:

```toml
tokio-tungstenite = "0.24"
futures-util = "0.3"
uuid = { version = "1", features = ["v4"] }
```

- [ ] **Step 2: Create the module file with the token helper**

Create `crates/roy-gateway/src/ws.rs` with the owner-only write helper and token loader (ported from the daemon's `pid_lock::create_owner_only_file` + `load_or_create_ws_token`, which are `pub(crate)` in `roy` and not re-exported):

```rust
//! WebSocket relay: a transparent bridge between WS clients and the roy
//! daemon's Unix socket. The protocol is identical on both sides (the same
//! control-protocol JSON), so this never parses a `ClientCommand` — it pumps
//! lines verbatim. Each WS connection gets its own dedicated daemon
//! connection, so input leases and subscriptions live in the daemon exactly as
//! for a direct client.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixStream};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http;
use tokio_tungstenite::tungstenite::Message;

/// Browsers can't set arbitrary headers on `new WebSocket(url, [protocols])`,
/// so the auth token rides the subprotocol slot instead.
const WS_TOKEN_HEADER: &str = "sec-websocket-protocol";

/// Atomically create `path` with `0600`, write `content` + `\n`, fsync.
/// Errors with `AlreadyExists` if the file is already there.
fn create_owner_only_file(path: &Path, content: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(content)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}

/// Load the WS auth token from `token_path`, or mint a fresh UUID and write it
/// owner-only (`0600`).
pub fn load_or_create_ws_token(token_path: &Path) -> Result<String> {
    if let Some(parent) = token_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating token dir {}", parent.display()))?;
    }
    match std::fs::read_to_string(token_path) {
        Ok(s) => Ok(s.trim().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let token = uuid::Uuid::new_v4().to_string();
            create_owner_only_file(token_path, token.as_bytes())
                .with_context(|| format!("writing token {}", token_path.display()))?;
            Ok(token)
        }
        Err(e) => Err(e).with_context(|| format!("reading token {}", token_path.display())),
    }
}
```

- [ ] **Step 3: Export the module**

In `crates/roy-gateway/src/lib.rs`, add `pub mod ws;` in alphabetical position (after `pub mod typing;` — actually keep the existing alpha order, insert before `pub mod typing;` is wrong; `ws` sorts last):

```rust
pub mod binder;
pub mod cancel;
pub mod config;
pub mod daemon;
pub mod draft_stream;
pub mod formatting;
pub mod orchestrator;
pub mod telegram;
pub mod typing;
pub mod ws;
```

- [ ] **Step 4: Build**

Run: `cargo build -p roy-gateway`
Expected: PASS (the helper functions are unused → `dead_code` warnings are fine for now; they get used in Task 3).

- [ ] **Step 5: Commit**

```bash
git add crates/roy-gateway/Cargo.toml crates/roy-gateway/src/ws.rs crates/roy-gateway/src/lib.rs
git commit -m "feat(roy-gateway): add ws module with token loader"
```

---

### Task 2: Token loader test

**Files:**
- Modify: `crates/roy-gateway/src/ws.rs` (append `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Append to `crates/roy-gateway/src/ws.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn token_is_minted_once_then_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let token_path = dir.path().join("ws.token");

        let t1 = load_or_create_ws_token(&token_path).unwrap();
        assert!(!t1.is_empty(), "token must not be empty");

        let mode = std::fs::metadata(&token_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token file must be 0600, got {mode:o}");

        let t2 = load_or_create_ws_token(&token_path).unwrap();
        assert_eq!(t1, t2, "second call must return the persisted token");
    }
}
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test -p roy-gateway token_is_minted_once_then_persisted`
Expected: PASS (the implementation already exists from Task 1; this locks in behavior).

- [ ] **Step 3: Commit**

```bash
git add crates/roy-gateway/src/ws.rs
git commit -m "test(roy-gateway): cover ws token mint/persist"
```

---

### Task 3: The auth callback and per-connection relay

**Files:**
- Modify: `crates/roy-gateway/src/ws.rs`

- [ ] **Step 1: Add the auth callback**

Insert into `crates/roy-gateway/src/ws.rs` after `load_or_create_ws_token`:

```rust
/// Validate the shared-secret token presented via `Sec-WebSocket-Protocol`.
/// Returns HTTP 401 on missing/invalid; echoes the subprotocol back on success
/// (required by the WS spec when the server selects a subprotocol).
fn ws_auth_callback(
    expected: Arc<String>,
) -> impl FnOnce(&Request, Response) -> std::result::Result<Response, ErrorResponse> {
    move |req, mut resp| {
        let provided = req
            .headers()
            .get(WS_TOKEN_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if provided != expected.as_str() {
            let body = if provided.is_empty() {
                "missing roy ws token (set Sec-WebSocket-Protocol)"
            } else {
                "invalid roy ws token"
            };
            return Err(http::Response::builder()
                .status(http::StatusCode::UNAUTHORIZED)
                .body(Some(body.into()))
                .expect("valid http response"));
        }
        resp.headers_mut().insert(
            WS_TOKEN_HEADER,
            http::HeaderValue::from_str(provided).expect("token is ascii uuid"),
        );
        Ok(resp)
    }
}
```

- [ ] **Step 2: Add the relay**

Append the relay function. It dials the daemon, splits both ends, and runs two pump loops under `select!`; whichever ends first tears down the other.

```rust
/// Bridge one accepted WS stream to a fresh daemon Unix-socket connection.
/// Pumps lines verbatim in both directions until either side closes.
async fn relay_connection<S>(
    ws: tokio_tungstenite::WebSocketStream<S>,
    socket_path: &Path,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let daemon = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connecting to daemon at {}", socket_path.display()))?;
    let (daemon_read, mut daemon_write) = daemon.into_split();
    let mut daemon_lines = BufReader::new(daemon_read).lines();

    let (mut ws_sink, mut ws_stream) = ws.split();

    // WS client → daemon: forward each text frame as a line.
    let inbound = async {
        while let Some(msg) = ws_stream.next().await {
            let text = match msg {
                Ok(Message::Text(t)) => t,
                Ok(Message::Close(_)) => break,
                // tungstenite answers ping/pong itself; ignore binary too.
                Ok(_) => continue,
                Err(_) => break,
            };
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            if daemon_write.write_all(text.as_bytes()).await.is_err()
                || daemon_write.write_all(b"\n").await.is_err()
                || daemon_write.flush().await.is_err()
            {
                break;
            }
        }
    };

    // daemon → WS client: forward each line as a text frame.
    let outbound = async {
        while let Ok(Some(line)) = daemon_lines.next_line().await {
            if ws_sink.send(Message::Text(line.into())).await.is_err() {
                break;
            }
        }
        let _ = ws_sink.close().await;
    };

    tokio::select! {
        _ = inbound => {}
        _ = outbound => {}
    }
    Ok(())
}
```

- [ ] **Step 3: Build**

Run: `cargo build -p roy-gateway`
Expected: PASS (`ws_auth_callback` and `relay_connection` are still unused → `dead_code` warnings OK until Task 5).

- [ ] **Step 4: Commit**

```bash
git add crates/roy-gateway/src/ws.rs
git commit -m "feat(roy-gateway): ws auth callback and transparent relay"
```

---

### Task 4: Relay round-trip integration test (fake daemon)

**Files:**
- Modify: `crates/roy-gateway/src/ws.rs` (extend `mod tests`)

- [ ] **Step 1: Write the failing test**

This stands up a fake daemon on a `UnixListener` that reads one line and replies with one scripted line, connects a real WS client through `relay_connection`, and asserts the round trip. Add inside `mod tests`:

```rust
    use futures_util::{SinkExt, StreamExt};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio_tungstenite::tungstenite::Message;

    #[tokio::test]
    async fn relay_round_trips_a_line_each_way() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("daemon.sock");

        // Fake daemon: accept one connection, read one line, echo a reply line.
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let fake_daemon = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let got = lines.next_line().await.unwrap().unwrap();
            write.write_all(b"{\"reply\":\"pong\"}\n").await.unwrap();
            write.flush().await.unwrap();
            got
        });

        // WS server side: accept one upgrade, hand it to relay_connection.
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let sock_for_relay = sock.clone();
        let relay = tokio::spawn(async move {
            let (stream, _) = tcp.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            relay_connection(ws, &sock_for_relay).await.unwrap();
        });

        // WS client: send a command line, expect the daemon's reply back.
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();
        ws.send(Message::Text("{\"cmd\":\"ping\"}".into())).await.unwrap();

        let reply = loop {
            match ws.next().await.expect("ws closed").unwrap() {
                Message::Text(t) => break t.to_string(),
                _ => continue,
            }
        };
        assert_eq!(reply, "{\"reply\":\"pong\"}");

        let daemon_saw = fake_daemon.await.unwrap();
        assert_eq!(daemon_saw, "{\"cmd\":\"ping\"}");

        let _ = ws.close(None).await;
        let _ = relay.await;
    }
```

- [ ] **Step 2: Run test to verify it passes**

Run: `cargo test -p roy-gateway relay_round_trips_a_line_each_way`
Expected: PASS.

- [ ] **Step 3: Add the cleanup test**

Append inside `mod tests`: dropping the WS client must cause the relay to drop its daemon connection (observable as EOF on the fake-daemon read side).

```rust
    #[tokio::test]
    async fn ws_drop_closes_daemon_connection() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("daemon.sock");

        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        // Fake daemon: accept, then read until EOF; return whether EOF was seen.
        let fake_daemon = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, _write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            // Returns None at EOF.
            lines.next_line().await.unwrap()
        });

        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let sock_for_relay = sock.clone();
        let relay = tokio::spawn(async move {
            let (stream, _) = tcp.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            relay_connection(ws, &sock_for_relay).await.unwrap();
        });

        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();
        // Close immediately without sending anything.
        ws.close(None).await.unwrap();

        let eof = fake_daemon.await.unwrap();
        assert!(eof.is_none(), "daemon must observe EOF after WS closes");
        let _ = relay.await;
    }
```

- [ ] **Step 4: Run both tests**

Run: `cargo test -p roy-gateway ws_drop_closes_daemon_connection relay_round_trips_a_line_each_way`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-gateway/src/ws.rs
git commit -m "test(roy-gateway): relay round-trip and disconnect cleanup"
```

---

### Task 5: Listener accept loop

**Files:**
- Modify: `crates/roy-gateway/src/ws.rs`

- [ ] **Step 1: Add the accept loop**

Append the public entry point that binds the TCP listener and accepts WS upgrades forever. Insert after `relay_connection`:

```rust
/// Bind `addr` and accept authenticated WS connections forever, relaying each
/// to the daemon at `socket_path`. One spawned task per connection.
pub async fn run_ws_relay(
    addr: SocketAddr,
    token: Arc<String>,
    socket_path: Arc<Path>,
) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding ws listener on {addr}"))?;
    tracing::info!(%addr, "websocket relay listener up");
    loop {
        let (stream, peer) = listener.accept().await.context("ws accept")?;
        let token = Arc::clone(&token);
        let socket_path = Arc::clone(&socket_path);
        tokio::spawn(async move {
            let callback = ws_auth_callback(token);
            let ws = match tokio_tungstenite::accept_hdr_async(stream, callback).await {
                Ok(ws) => ws,
                Err(e) => {
                    tracing::warn!(%peer, error = %e, "ws handshake rejected");
                    return;
                }
            };
            tracing::debug!(%peer, "ws connection accepted");
            if let Err(e) = relay_connection(ws, &socket_path).await {
                tracing::warn!(%peer, error = %e, "ws relay ended with error");
            }
        });
    }
}
```

Note `socket_path: Arc<Path>` — construct in `main.rs` (Task 6) with
`Arc::from(some_path_buf)` (std provides `impl From<PathBuf> for Arc<Path>`).

- [ ] **Step 2: Add the auth-rejection test**

Add inside `mod tests` (uses the real listener + callback, mirroring the daemon's old handshake test):

```rust
    #[tokio::test]
    async fn handshake_rejects_missing_or_wrong_token() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let token = Arc::new("the-real-token".to_string());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let token_for_server = Arc::clone(&token);
        let server = tokio::spawn(async move {
            let mut results = Vec::new();
            for _ in 0..3 {
                let (stream, _) = listener.accept().await.unwrap();
                let cb = ws_auth_callback(Arc::clone(&token_for_server));
                results.push(tokio_tungstenite::accept_hdr_async(stream, cb).await.is_ok());
            }
            results
        });

        // 1. No token → reject.
        let url = format!("ws://{addr}");
        assert!(tokio_tungstenite::connect_async(&url).await.is_err());

        // 2. Wrong token → reject.
        let mut req = url.as_str().into_client_request().unwrap();
        req.headers_mut().insert(
            WS_TOKEN_HEADER,
            http::HeaderValue::from_static("nope"),
        );
        assert!(tokio_tungstenite::connect_async(req).await.is_err());

        // 3. Correct token → accept.
        let mut req = url.as_str().into_client_request().unwrap();
        req.headers_mut().insert(
            WS_TOKEN_HEADER,
            http::HeaderValue::from_static("the-real-token"),
        );
        let ok = tokio_tungstenite::connect_async(req).await;
        assert!(ok.is_ok(), "correct token must be accepted");

        let results = server.await.unwrap();
        assert_eq!(results, vec![false, false, true]);
    }
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p roy-gateway --lib ws::`
Expected: PASS (all ws tests).

- [ ] **Step 4: Commit**

```bash
git add crates/roy-gateway/src/ws.rs
git commit -m "feat(roy-gateway): ws relay listener accept loop"
```

---

### Task 6: Make the gateway binary config-driven

**Files:**
- Modify: `crates/roy-gateway/src/config.rs:9-50`
- Modify: `crates/roy-gateway/src/config.rs:62-108` (tests)
- Modify: `crates/roy-gateway/src/main.rs`

- [ ] **Step 1: Write failing config tests**

Replace the `#[cfg(test)] mod tests` block in `config.rs` with tests for the new shape (telegram optional, websocket added, validation):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_telegram_and_websocket() {
        let raw = r#"
            [daemon]
            socket = "/tmp/roy.sock"

            [telegram]
            token = "1234:abc"
            preset = "claude"

            [binder]
            path = "/tmp/binder.json"

            [websocket]
            bind = "127.0.0.1:9001"
            token_path = "/tmp/ws.token"
        "#;
        let cfg: GatewayConfig = toml::from_str(raw).unwrap();
        assert_eq!(cfg.daemon.socket.as_deref(), Some("/tmp/roy.sock"));
        let tg = cfg.telegram.as_ref().unwrap();
        assert_eq!(tg.token, "1234:abc");
        let ws = cfg.websocket.as_ref().unwrap();
        assert_eq!(ws.bind, "127.0.0.1:9001");
        assert_eq!(ws.token_path.as_deref(), Some("/tmp/ws.token"));
    }

    #[test]
    fn websocket_only_config_parses() {
        let raw = r#"
            [websocket]
            bind = "127.0.0.1:8787"
        "#;
        let cfg: GatewayConfig = toml::from_str(raw).unwrap();
        assert!(cfg.telegram.is_none());
        assert!(cfg.binder.is_none());
        let ws = cfg.websocket.as_ref().unwrap();
        assert_eq!(ws.bind, "127.0.0.1:8787");
        assert!(ws.token_path.is_none());
        cfg.validate().expect("ws-only is valid");
    }

    #[test]
    fn websocket_bind_defaults_when_omitted() {
        let raw = r#"
            [websocket]
        "#;
        let cfg: GatewayConfig = toml::from_str(raw).unwrap();
        assert_eq!(cfg.websocket.unwrap().bind, "127.0.0.1:8787");
    }

    #[test]
    fn no_adapter_is_an_error() {
        let raw = r#"
            [daemon]
            socket = "/tmp/roy.sock"
        "#;
        let cfg: GatewayConfig = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err(), "must require at least one adapter");
    }

    #[test]
    fn telegram_without_binder_is_an_error() {
        let raw = r#"
            [telegram]
            token = "x"
            preset = "claude"
        "#;
        let cfg: GatewayConfig = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err(), "telegram requires a binder");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p roy-gateway --lib config::`
Expected: FAIL — `telegram` is not `Option`, `websocket`/`validate`/`WebsocketConfig` don't exist (compile errors).

- [ ] **Step 3: Update the config types**

Replace the type definitions in `config.rs` (lines 9–50) with:

```rust
#[derive(Debug, Deserialize)]
pub struct GatewayConfig {
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub telegram: Option<TelegramConfig>,
    #[serde(default)]
    pub binder: Option<BinderConfig>,
    #[serde(default)]
    pub websocket: Option<WebsocketConfig>,
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
    /// `Some(project_id)` to scope spawned sessions to that project; `None`
    /// to use the daemon's default cwd.
    #[serde(default)]
    pub project_id: Option<String>,
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

#[derive(Debug, Deserialize)]
pub struct WebsocketConfig {
    /// Address to bind the WS listener on. Loopback-only by default; set an
    /// external address only behind your own TLS termination.
    #[serde(default = "default_ws_bind")]
    pub bind: String,
    /// Path to the shared-secret token file. When `None`, defaults to
    /// `~/.local/state/roy-gateway/ws.token`.
    #[serde(default)]
    pub token_path: Option<String>,
}

fn default_ws_bind() -> String {
    "127.0.0.1:8787".to_string()
}

impl GatewayConfig {
    /// At least one adapter must be configured; Telegram needs a binder.
    pub fn validate(&self) -> Result<()> {
        if self.telegram.is_none() && self.websocket.is_none() {
            anyhow::bail!("config must enable at least one of [telegram] or [websocket]");
        }
        if self.telegram.is_some() && self.binder.is_none() {
            anyhow::bail!("[telegram] requires a [binder] section");
        }
        Ok(())
    }
}
```

Keep the existing `impl GatewayConfig { pub fn load … }` — merge `validate` into the same `impl` block or add a second `impl`; either compiles. Call `validate()` inside `load` after parsing:

```rust
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let cfg: Self =
            toml::from_str(&raw).with_context(|| format!("parsing config {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }
```

- [ ] **Step 4: Run config tests**

Run: `cargo test -p roy-gateway --lib config::`
Expected: PASS.

- [ ] **Step 5: Rewrite `main.rs` to be config-driven**

Replace `crates/roy-gateway/src/main.rs` entirely with:

```rust
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use teloxide::Bot;
use tracing_subscriber::EnvFilter;

use roy_gateway::binder::SessionBinder;
use roy_gateway::cancel::CancelRegistry;
use roy_gateway::config::GatewayConfig;
use roy_gateway::daemon::RealConnFactory;
use roy_gateway::orchestrator::OrchestratorConfig;
use roy_gateway::telegram::{run, BotDeps, TeloxideReplier};
use roy_gateway::ws;

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

fn default_ws_token_path() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/roy-gateway/ws.token")
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

    let telegram_task = build_telegram_task(&cfg, &socket_path).await?;
    let ws_task = build_ws_task(&cfg, &socket_path)?;

    // At least one is Some (validated in GatewayConfig::load).
    match (telegram_task, ws_task) {
        (Some(tg), Some(ws)) => {
            tokio::select! {
                r = tg => r.context("telegram task")?,
                r = ws => r.context("ws task")?,
            }
        }
        (Some(tg), None) => tg.await.context("telegram task")?,
        (None, Some(ws)) => ws.await.context("ws task")?,
        (None, None) => unreachable!("validate() guarantees at least one adapter"),
    }
}

async fn build_telegram_task(
    cfg: &GatewayConfig,
    socket_path: &Path,
) -> Result<Option<tokio::task::JoinHandle<Result<()>>>> {
    let Some(tg) = &cfg.telegram else {
        return Ok(None);
    };
    let binder_cfg = cfg
        .binder
        .as_ref()
        .expect("validate() guarantees binder when telegram is set");
    let binder_path = PathBuf::from(&binder_cfg.path);
    let binder = Arc::new(
        SessionBinder::load(binder_path.clone())
            .await
            .with_context(|| format!("loading binder {}", binder_path.display()))?,
    );
    let conn_factory = Arc::new(RealConnFactory::new(socket_path.to_path_buf()));
    let orch_cfg = Arc::new(OrchestratorConfig {
        preset: tg.preset.clone(),
        project_id: tg.project_id.clone(),
        turn_timeout: Duration::from_secs(tg.turn_timeout_secs),
        typing_interval: Duration::from_secs(4),
    });
    let bot = Bot::new(tg.token.clone());
    let replier = Arc::new(TeloxideReplier::new(bot.clone()));
    let allowed: HashSet<u64> = tg.allowed_user_ids.iter().copied().collect();
    let deps = BotDeps {
        cfg: orch_cfg,
        binder,
        conn_factory,
        replier,
        cancel_registry: CancelRegistry::new(),
        allowed_user_ids: Arc::new(allowed),
    };
    Ok(Some(tokio::spawn(async move { run(bot, deps).await })))
}

fn build_ws_task(
    cfg: &GatewayConfig,
    socket_path: &Path,
) -> Result<Option<tokio::task::JoinHandle<Result<()>>>> {
    let Some(ws_cfg) = &cfg.websocket else {
        return Ok(None);
    };
    let addr: std::net::SocketAddr = ws_cfg
        .bind
        .parse()
        .with_context(|| format!("parsing websocket.bind '{}'", ws_cfg.bind))?;
    let token_path = ws_cfg
        .token_path
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(default_ws_token_path);
    let token = Arc::new(ws::load_or_create_ws_token(&token_path)?);
    tracing::info!(path = %token_path.display(), %addr, "ws auth token / bind");
    let socket: Arc<Path> = Arc::from(socket_path.to_path_buf());
    Ok(Some(tokio::spawn(async move {
        ws::run_ws_relay(addr, token, socket).await
    })))
}
```

- [ ] **Step 6: Build and test the whole gateway**

Run: `cargo build -p roy-gateway && cargo test -p roy-gateway`
Expected: PASS. (Telegram still works unchanged; WS relay is wired.)

- [ ] **Step 7: Commit**

```bash
git add crates/roy-gateway/src/config.rs crates/roy-gateway/src/main.rs
git commit -m "feat(roy-gateway): config-driven startup for telegram and/or ws relay"
```

---

### Task 7: Remove WS from the daemon

**Files:**
- Modify: `crates/roy/src/daemon.rs` (imports 10-23; remove `load_or_create_ws_token` 46-62, `ws_auth_callback` 64-92, `WS_TOKEN_HEADER` 42-44, `run_ws` 287-311, `serve_ws_connection` 327-338, `dispatch_ws` 363-398, `ws_writer_loop` 1344-1360; `ServeOpts.ws_port` 151; the WS branch in `run_with_opts` 216-240)

- [ ] **Step 1: Remove the WS functions**

Delete these items from `daemon.rs`:
- `WS_TOKEN_HEADER` const (lines 42-44)
- `load_or_create_ws_token` (lines 46-62)
- `ws_auth_callback` (lines 64-92)
- `run_ws` (lines 287-311)
- `serve_ws_connection` (lines 327-338)
- `dispatch_ws` (lines 363-398)
- `ws_writer_loop` (lines 1344-1360)

- [ ] **Step 2: Drop `ws_port` from `ServeOpts`**

In `ServeOpts` remove the field and update the doc comment:

```rust
#[derive(Debug, Clone)]
pub struct ServeOpts {
    pub socket_path: PathBuf,
    /// Auto-close any session quiet past this threshold. `None` disables GC.
    pub idle_timeout: Option<std::time::Duration>,
    /// Resurrect every archived session in `journal_dir` at startup.
    pub resume_all: bool,
}
```

- [ ] **Step 3: Collapse `run_with_opts` to the Unix listener only**

Replace the listener-spawning tail of `run_with_opts` (the `let unix = …; let ws = …; match ws { … }` block, lines ~211-240) with:

```rust
        self.run_unix(&opts.socket_path).await
```

Remove the `tokio::spawn`/`tokio::join!` plumbing entirely — `run_unix` already loops forever and returns only on error.

- [ ] **Step 4: Fix imports**

In `daemon.rs` header (lines 10-23):
- Delete `use std::net::SocketAddr;` (line 11).
- Change `use futures_util::{SinkExt, StreamExt};` → `use futures_util::StreamExt;` (StreamExt still used at the attach pump; SinkExt was WS-only).
- Change `use tokio::net::{TcpListener, UnixListener};` → `use tokio::net::UnixListener;` (TcpListener was WS/test only).
- Delete the four `tokio_tungstenite::…` import lines (20-23): `ErrorResponse/Request/Response`, `http`, `Message`, `WebSocketStream`.

Update the module doc comment at the top (lines 1-8) to drop the WebSocket mentions:

```rust
//! `roy serve` daemon: owns one `SessionManager` and serves connections from
//! triggers over a Unix socket, speaking the control protocol defined in
//! `crate::control`. WebSocket clients are served by `roy-gateway`, which
//! relays them to this socket.
//!
//! Each connection gets its own writer task that drains a per-connection
//! `mpsc<ServerEvent>` and serializes events as `\n`-delimited JSON lines.
//! The command-dispatch loop is shared.
```

- [ ] **Step 5: Remove the three WS tests**

Delete from `#[cfg(test)] mod tests`:
- `spawn_attach_send_round_trip_over_websocket` (lines ~2200-2336)
- `ws_handshake_rejects_missing_or_wrong_token` (lines ~1428-end of that fn)
- the `load_or_create_ws_token` token test (the `#[test]` ending around line 1426 — search for `load_or_create_ws_token` inside `mod tests`)

- [ ] **Step 6: Build the daemon crate**

Run: `cargo build -p roy --all-targets`
Expected: PASS, no unused-import warnings for the touched imports. If `cargo build` reports `unused import: StreamExt`, that means the attach pump uses a different `StreamExt` — investigate before proceeding (the spec assumes `futures_util::StreamExt` at `daemon.rs:1298`).

- [ ] **Step 7: Run daemon tests**

Run: `cargo test -p roy`
Expected: PASS (the Unix-socket round-trip and all other tests; the WS tests are gone).

- [ ] **Step 8: Commit**

```bash
git add crates/roy/src/daemon.rs
git commit -m "refactor(roy): remove WebSocket transport from the daemon"
```

---

### Task 8: Drop the WS dependency and the CLI `--port` flag

**Files:**
- Modify: `crates/roy/Cargo.toml:20`
- Modify: `crates/roy-cli/src/main.rs:87-89` (flag), `:310-316` (eprintlns), `:322-327` (ServeOpts)

- [ ] **Step 1: Remove `tokio-tungstenite` from `roy`**

In `crates/roy/Cargo.toml` delete the line `tokio-tungstenite = "0.24"` (line 20). Keep `futures-util = "0.3"` (still used by `StreamExt`).

- [ ] **Step 2: Remove the `--port` flag**

In `crates/roy-cli/src/main.rs`, delete from `ServeArgs` (lines 87-89):

```rust
    /// Enable WebSocket listener on this port (in addition to the Unix socket).
    #[arg(long)]
    port: Option<u16>,
```

- [ ] **Step 3: Remove the WS eprintlns and `ws_port` wiring in `cmd_serve`**

Delete the `if let Some(port) = args.port { … }` block (lines 310-316) and the `ws_port: args.port,` line inside the `ServeOpts { … }` literal (line 324). Resulting `ServeOpts` literal:

```rust
        .run_with_opts(ServeOpts {
            socket_path: socket.clone(),
            idle_timeout,
            resume_all: args.resume_all,
        })
```

- [ ] **Step 4: Build the whole workspace**

Run: `cargo build --workspace --all-targets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/roy/Cargo.toml crates/roy-cli/src/main.rs
git commit -m "refactor(roy-cli): drop --port; daemon is Unix-socket only"
```

---

### Task 9: Docs and full CI gate

**Files:**
- Modify: `crates/roy/CLAUDE.md`
- Modify: `docs/architecture.md`
- Modify: `docs/wire-protocol.md`

- [ ] **Step 1: Update `crates/roy/CLAUDE.md`**

Find the daemon description that says it "accepts Unix-socket and WebSocket connections" and the warning "The WebSocket listener (when enabled via `--port`) is currently unauthenticated". Rewrite to: the daemon accepts Unix-socket connections only; WebSocket clients are served by `roy-gateway`'s WS relay (token-authenticated, loopback-by-default), which bridges to this socket. Remove the `--port` mention.

- [ ] **Step 2: Update `docs/architecture.md`**

Change the daemon layer description to Unix-socket-only and add the WS relay as a peer bridge alongside Telegram and the scheduler (all talk to the daemon over the Unix socket). Search for "WebSocket" / "run_ws" references and update them.

- [ ] **Step 3: Update `docs/wire-protocol.md`**

Note that WS framing is now provided by `roy-gateway`'s relay, not the daemon, but the JSON shape is unchanged and identical on Unix socket and WS.

- [ ] **Step 4: Run the full CI gate locally**

Run:
```bash
cargo fmt --all -- --check
cargo build --workspace --all-targets
cargo test --workspace --no-fail-fast
```
Expected: all PASS. (`cargo fmt` may need a prior `cargo fmt --all` to fix formatting from the edits — run it, then re-check.)

- [ ] **Step 5: Commit**

```bash
git add crates/roy/CLAUDE.md docs/architecture.md docs/wire-protocol.md
git commit -m "docs: daemon is Unix-socket only; WS lives in roy-gateway"
```

---

## Manual verification (after all tasks)

Not automatable in CI — confirm the relay end-to-end against a real daemon:

```bash
# 1. Start the daemon (no --port anymore).
cargo run -p roy-cli -- serve &

# 2. Write a ws-only gateway config.
cat > /tmp/ws-gw.toml <<'EOF'
[daemon]
socket = "~/.roy/daemon.sock"

[websocket]
bind = "127.0.0.1:8787"
token_path = "/tmp/ws.token"
EOF

# 3. Start the gateway; note the token it prints / writes to /tmp/ws.token.
cargo run -p roy-gateway -- --config /tmp/ws-gw.toml &

# 4. Connect a WS client presenting the token as the subprotocol, send a
#    {"List":{}} command, and confirm a Listed event comes back. Use any WS
#    client (e.g. websocat):
TOKEN=$(cat /tmp/ws.token)
echo '{"List":{}}' | websocat -H="Sec-WebSocket-Protocol: $TOKEN" ws://127.0.0.1:8787
# Expected: a JSON line `{"Listed":{"sessions":[...]}}`.
```

Confirm: bad/missing token → 401; killing the gateway leaves the daemon healthy (`roy status` exit 0); restarting the gateway reconnects cleanly.
