# Inbound Event Bus Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `roy-inbound` — a new crate hosting an in-process event bus for inbound channels, with HTTP webhook as the first concrete channel. Telegram-gateway remains untouched.

**Architecture:** Channels are pure `Publisher`s that normalize external events into `InboundEvent`s and push them onto a `tokio::mpsc` bus. A single `InboundDispatcher` consumes the bus, asks a `Router` to produce a `FireSpec`, asks `SessionResolver` to translate `SessionStrategy` into a `FireTarget`, runs the fire over the existing roy daemon Unix socket, and hands the outcome to a per-channel `ReplyHook` which delivers the result back through a typed `ReplyHandle` carried on the event.

**Tech Stack:** Rust 2021, `tokio`, `axum`, `sqlx`/SQLite, `serde_json`, `anyhow`, `tracing`, `async-trait`, `clap`, `uuid`, `chrono`. Reuses `roy::{ClientCommand, ServerEvent, FireTarget, TurnEvent, StopReason}` for the wire protocol. Spec: `docs/superpowers/specs/2026-05-25-inbound-event-bus-design.md`.

---

## File structure

```
crates/roy-inbound/
  Cargo.toml
  src/
    lib.rs              # pub use re-exports, run()
    bus.rs              # InboundEvent, ReplyHandle, ReplyKind
    config.rs           # InboundConfig (TOML)
    template.rs         # render(template, payload) → String
    router.rs           # Router trait + ConfigRouter
    session.rs          # SessionStrategy, PendingBinding, SessionResolver
    reply.rs            # ReplyHook trait, FireOutcome, ReplyHookFactory
    daemon_client.rs    # fire_with_hook() — fire + stream → hook
    dispatcher.rs       # InboundDispatcher::run loop
    cli.rs              # Args + run(args)
    store/
      mod.rs
      db.rs             # open() + MIGRATOR
      bindings.rs       # BindingStore CRUD
    channels/
      mod.rs            # Publisher trait
      webhook/
        mod.rs          # WebhookPublisher (axum)
        config.rs       # WebhookConfig
        reply.rs        # WebhookReplyHook + factory
    main.rs             # standalone binary (thin)
  migrations/sqlite/
    0001_initial.sql    # bindings table
  tests/
    integration.rs      # end-to-end with mock daemon
```

Modifications to existing files:
- `crates/roy-cli/Cargo.toml` — add `roy-inbound` dep
- `crates/roy-cli/src/main.rs` — add `Inbound { … }` subcommand
- `CLAUDE.md` — add `roy-inbound` entry to crate list (final task)

---

### Task 1: Workspace scaffold — create `roy-inbound` crate that builds

**Files:**
- Create: `crates/roy-inbound/Cargo.toml`
- Create: `crates/roy-inbound/src/lib.rs`
- Create: `crates/roy-inbound/src/main.rs`

The workspace `Cargo.toml` uses `members = ["crates/*"]` (see project root) so a new crate is picked up automatically — no edit there.

- [ ] **Step 1: Create the Cargo manifest**

`crates/roy-inbound/Cargo.toml`:

```toml
[package]
name = "roy-inbound"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"

[[bin]]
name = "roy-inbound"
path = "src/main.rs"

[dependencies]
roy = { path = "../roy" }

# DB
sqlx = { version = "0.8", default-features = false, features = [
  "runtime-tokio",
  "sqlite",
  "chrono",
  "macros",
  "migrate",
] }

# HTTP server (workspace pins axum 0.8 in roy-management)
axum = { version = "0.8", features = ["macros"] }

# Async runtime
tokio = { version = "1", features = ["full"] }
tokio-util = { version = "0.7", features = ["rt"] }

# Serde / errors / logging / ids / time
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
anyhow = "1"
async-trait = "0.1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
uuid = { version = "1", features = ["v4", "serde"] }
chrono = { version = "0.4", default-features = false, features = ["serde", "clock"] }

# CLI
clap = { version = "4.5", features = ["derive", "env"] }

# Crypto for webhook HMAC
hmac = "0.12"
sha2 = "0.10"
subtle = "2"
hex = "0.4"

[dev-dependencies]
tempfile = "3"
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
```

- [ ] **Step 2: Create the placeholder lib + bin**

`crates/roy-inbound/src/lib.rs`:

```rust
//! roy-inbound — event-bus substrate for inbound channels.
//! Spec: docs/superpowers/specs/2026-05-25-inbound-event-bus-design.md
```

`crates/roy-inbound/src/main.rs`:

```rust
fn main() {
    eprintln!("roy-inbound binary stub — real entry comes in Task 14");
}
```

- [ ] **Step 3: Run the workspace build to confirm the crate is picked up**

Run: `cargo build -p roy-inbound`
Expected: compiles cleanly. If sqlx or axum versions are stale in the workspace lockfile, `cargo update -p roy-inbound` may be needed.

- [ ] **Step 4: Commit**

```bash
git add crates/roy-inbound/Cargo.toml crates/roy-inbound/src/lib.rs crates/roy-inbound/src/main.rs
git commit -m "chore(roy-inbound): scaffold empty crate with deps"
```

---

### Task 2: Template renderer — `{{payload.foo.bar}}` substitution

**Files:**
- Create: `crates/roy-inbound/src/template.rs`
- Modify: `crates/roy-inbound/src/lib.rs` (add `pub mod template;`)

Tiny hand-rolled renderer. No conditionals, no loops — just dotted-path lookup against a `serde_json::Value`. Missing keys render as empty string and emit a `tracing::warn`. Spec §"Open questions" — pluggable later; first iteration is intentionally minimal.

- [ ] **Step 1: Write the failing test**

`crates/roy-inbound/src/template.rs`:

```rust
//! Minimal template renderer. Replaces `{{payload.a.b.c}}` substrings with
//! the corresponding nested value from a `serde_json::Value`. Non-existent
//! paths render as empty string (with a `tracing::warn`). Non-string scalars
//! are rendered via `Display`-equivalent JSON serialization (so a number
//! becomes "42", a boolean "true", an object/array its JSON form).

use serde_json::Value;

pub fn render(template: &str, payload: &Value) -> String {
    // TODO impl
    let _ = (template, payload);
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flat_string_substitutes() {
        let out = render("hello {{payload.name}}", &json!({"name": "world"}));
        assert_eq!(out, "hello world");
    }

    #[test]
    fn nested_path_substitutes() {
        let out = render(
            "order {{payload.body.id}} from {{payload.body.user.email}}",
            &json!({"body": {"id": 42, "user": {"email": "a@b"}}}),
        );
        assert_eq!(out, "order 42 from a@b");
    }

    #[test]
    fn missing_path_renders_empty() {
        let out = render("hi {{payload.absent}}", &json!({}));
        assert_eq!(out, "hi ");
    }

    #[test]
    fn no_placeholders_passthrough() {
        let out = render("static text", &json!({"x": 1}));
        assert_eq!(out, "static text");
    }

    #[test]
    fn boolean_and_object_serialize() {
        let out = render(
            "active={{payload.active}} meta={{payload.meta}}",
            &json!({"active": true, "meta": {"a": 1}}),
        );
        assert_eq!(out, "active=true meta={\"a\":1}");
    }
}
```

- [ ] **Step 2: Wire the module into lib.rs**

Append to `crates/roy-inbound/src/lib.rs`:

```rust
pub mod template;
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p roy-inbound template::tests`
Expected: all five tests FAIL (`render` returns empty string).

- [ ] **Step 4: Implement `render`**

Replace the body of `render` in `crates/roy-inbound/src/template.rs`:

```rust
pub fn render(template: &str, payload: &Value) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find("}}") else {
            out.push_str("{{");
            rest = after_open;
            continue;
        };
        let expr = after_open[..end].trim();
        let value_str = resolve_path(expr, payload);
        out.push_str(&value_str);
        rest = &after_open[end + 2..];
    }
    out.push_str(rest);
    out
}

fn resolve_path(expr: &str, payload: &Value) -> String {
    let Some(rest) = expr.strip_prefix("payload") else {
        tracing::warn!(expr, "template path missing `payload.` prefix");
        return String::new();
    };
    let mut node = payload;
    for segment in rest.split('.').filter(|s| !s.is_empty()) {
        match node {
            Value::Object(map) => match map.get(segment) {
                Some(child) => node = child,
                None => {
                    tracing::warn!(expr, segment, "template path missing in payload");
                    return String::new();
                }
            },
            _ => {
                tracing::warn!(expr, segment, "template path tried to descend into non-object");
                return String::new();
            }
        }
    }
    match node {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
```

- [ ] **Step 5: Verify tests pass**

Run: `cargo test -p roy-inbound template::tests`
Expected: 5 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/roy-inbound/src/template.rs crates/roy-inbound/src/lib.rs
git commit -m "feat(roy-inbound): template renderer for payload paths"
```

---

### Task 3: Config parsing — TOML → `InboundConfig`

**Files:**
- Create: `crates/roy-inbound/src/config.rs`
- Create: `crates/roy-inbound/src/channels/mod.rs` (empty for now)
- Create: `crates/roy-inbound/src/channels/webhook/config.rs`
- Modify: `crates/roy-inbound/src/lib.rs` (add modules)

Top-level structure mirrors the spec's TOML example. `kind`-specific table is parsed into a typed enum (`ChannelConfig::Webhook(WebhookConfig)`).

- [ ] **Step 1: Write the failing test**

`crates/roy-inbound/src/config.rs`:

```rust
//! TOML config for roy-inbound. One global `[bus]`, one `[server]`, and
//! N `[[sources]]` blocks. Each source declares a `kind` and a matching
//! `[sources.<kind>]` sub-table.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::channels::webhook::config::WebhookConfig;
use crate::session::SessionStrategyConfig;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboundConfig {
    #[serde(default)]
    pub bus: BusConfig,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusConfig {
    #[serde(default = "default_capacity")]
    pub capacity: usize,
}
impl Default for BusConfig {
    fn default() -> Self { Self { capacity: default_capacity() } }
}
fn default_capacity() -> usize { 256 }

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
}
impl Default for ServerConfig {
    fn default() -> Self { Self { bind: default_bind() } }
}
fn default_bind() -> String { "127.0.0.1:8090".into() }

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfig {
    pub id: String,
    pub kind: String,            // "webhook"
    pub agent_id: String,
    pub session: SessionStrategyConfig,
    #[serde(default = "default_fire_timeout")]
    pub fire_timeout_secs: u64,
    pub template: String,
    pub webhook: Option<WebhookConfig>,
}
fn default_fire_timeout() -> u64 { 600 }

impl InboundConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let cfg: Self = toml::from_str(&raw)
            .with_context(|| format!("parsing {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        let mut ids = std::collections::HashSet::new();
        let mut paths = std::collections::HashSet::new();
        for src in &self.sources {
            if !ids.insert(src.id.clone()) {
                return Err(anyhow!("duplicate source id: {}", src.id));
            }
            match src.kind.as_str() {
                "webhook" => {
                    let wh = src.webhook.as_ref()
                        .ok_or_else(|| anyhow!("source {}: missing [sources.webhook]", src.id))?;
                    if !paths.insert(wh.path.clone()) {
                        return Err(anyhow!("duplicate webhook path: {}", wh.path));
                    }
                }
                other => return Err(anyhow!("source {}: unknown kind '{}'", src.id, other)),
            }
            // PerSenderSticky requires idle_timeout — enforced by serde via tagged enum.
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_and_load(content: &str) -> Result<InboundConfig> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inbound.toml");
        std::fs::write(&path, content).unwrap();
        InboundConfig::load(&path)
    }

    #[test]
    fn minimal_webhook_loads() {
        let cfg = write_and_load(r#"
            [[sources]]
            id = "orders"
            kind = "webhook"
            agent_id = "order-bot"
            session = "ephemeral"
            template = "New: {{payload.body}}"
            [sources.webhook]
            path = "/orders"
            reply_mode = "sync"
        "#).unwrap();
        assert_eq!(cfg.sources.len(), 1);
        assert_eq!(cfg.sources[0].fire_timeout_secs, 600);
        assert_eq!(cfg.bus.capacity, 256);
    }

    #[test]
    fn duplicate_source_id_rejected() {
        let err = write_and_load(r#"
            [[sources]]
            id = "x"
            kind = "webhook"
            agent_id = "a"
            session = "ephemeral"
            template = "t"
            [sources.webhook]
            path = "/a"
            reply_mode = "sync"
            [[sources]]
            id = "x"
            kind = "webhook"
            agent_id = "a"
            session = "ephemeral"
            template = "t"
            [sources.webhook]
            path = "/b"
            reply_mode = "sync"
        "#).unwrap_err();
        assert!(err.to_string().contains("duplicate source id"));
    }

    #[test]
    fn duplicate_webhook_path_rejected() {
        let err = write_and_load(r#"
            [[sources]]
            id = "a"
            kind = "webhook"
            agent_id = "x"
            session = "ephemeral"
            template = "t"
            [sources.webhook]
            path = "/shared"
            reply_mode = "sync"
            [[sources]]
            id = "b"
            kind = "webhook"
            agent_id = "x"
            session = "ephemeral"
            template = "t"
            [sources.webhook]
            path = "/shared"
            reply_mode = "sync"
        "#).unwrap_err();
        assert!(err.to_string().contains("duplicate webhook path"));
    }

    #[test]
    fn unknown_kind_rejected() {
        let err = write_and_load(r#"
            [[sources]]
            id = "x"
            kind = "carrier-pigeon"
            agent_id = "a"
            session = "ephemeral"
            template = "t"
        "#).unwrap_err();
        assert!(err.to_string().contains("unknown kind"));
    }
}
```

- [ ] **Step 2: Create the webhook config + channels module skeletons**

`crates/roy-inbound/src/channels/mod.rs`:

```rust
//! Channel implementations (webhook, future telegram/imap/whatsapp).
pub mod webhook;
```

`crates/roy-inbound/src/channels/webhook/mod.rs`:

```rust
pub mod config;
```

`crates/roy-inbound/src/channels/webhook/config.rs`:

```rust
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookConfig {
    pub path: String,
    #[serde(default)]
    pub secret_env: Option<String>,
    #[serde(default = "default_reply_mode")]
    pub reply_mode: ReplyMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyMode { Sync, Async }

fn default_reply_mode() -> ReplyMode { ReplyMode::Sync }
```

- [ ] **Step 3: Create `SessionStrategyConfig` stub (full impl in Task 5)**

`crates/roy-inbound/src/session.rs`:

```rust
//! Session strategy + resolver. Resolver impl lands in Task 5.
use std::time::Duration;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SessionStrategyConfig {
    Ephemeral,
    PersistentOne,
    PerSenderSticky { idle_timeout_secs: u64 },
}

// Compact form (`session = "ephemeral"`) — accepted via untagged fallback.
impl<'de> serde::Deserialize<'de> for SessionStrategyConfig {
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where D: serde::Deserializer<'de> {
        // Try string first, then tagged map.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Helper {
            Short(String),
            Tagged { kind: String, idle_timeout_secs: Option<u64> },
        }
        match Helper::deserialize(de)? {
            Helper::Short(s) => match s.as_str() {
                "ephemeral" => Ok(Self::Ephemeral),
                "persistent_one" => Ok(Self::PersistentOne),
                other => Err(serde::de::Error::custom(format!(
                    "unknown session strategy '{other}' (use tagged form for per_sender_sticky)"
                ))),
            },
            Helper::Tagged { kind, idle_timeout_secs } => match kind.as_str() {
                "ephemeral" => Ok(Self::Ephemeral),
                "persistent_one" => Ok(Self::PersistentOne),
                "per_sender_sticky" => {
                    let secs = idle_timeout_secs.ok_or_else(|| serde::de::Error::custom(
                        "per_sender_sticky requires idle_timeout_secs"))?;
                    Ok(Self::PerSenderSticky { idle_timeout_secs: secs })
                }
                other => Err(serde::de::Error::custom(format!("unknown kind '{other}'"))),
            },
        }
    }
}

impl SessionStrategyConfig {
    pub fn idle_timeout(&self) -> Option<Duration> {
        match self {
            Self::PerSenderSticky { idle_timeout_secs } => Some(Duration::from_secs(*idle_timeout_secs)),
            _ => None,
        }
    }
}
```

Note the manual `Deserialize` impl above — we can't have both `#[derive]` and a custom impl. Remove the `#[derive]` line from `SessionStrategyConfig` (it was illustrative).

- [ ] **Step 4: Wire modules**

Append to `crates/roy-inbound/src/lib.rs`:

```rust
pub mod channels;
pub mod config;
pub mod session;
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p roy-inbound config::tests`
Expected: 4 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/roy-inbound/src/{config.rs,session.rs,channels} crates/roy-inbound/src/lib.rs
git commit -m "feat(roy-inbound): TOML config parser with validation"
```

---

### Task 4: SQLite migrations + `db::open`

**Files:**
- Create: `crates/roy-inbound/migrations/sqlite/0001_initial.sql`
- Create: `crates/roy-inbound/src/store/mod.rs`
- Create: `crates/roy-inbound/src/store/db.rs`
- Modify: `crates/roy-inbound/src/lib.rs`

Mirrors `roy-scheduler::db::open` — WAL, 5s busy timeout, mode 0600 on Unix.

- [ ] **Step 1: Write the migration SQL**

`crates/roy-inbound/migrations/sqlite/0001_initial.sql`:

```sql
CREATE TABLE bindings (
    id              TEXT PRIMARY KEY,
    source_id       TEXT NOT NULL,
    sender_id       TEXT NOT NULL,
    session_id      TEXT NOT NULL,
    agent_id        TEXT NOT NULL,
    strategy        TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    last_active_at  TEXT NOT NULL,
    UNIQUE(source_id, sender_id)
);

CREATE INDEX bindings_by_last_active ON bindings(last_active_at);
```

- [ ] **Step 2: Write the failing test for `open`**

`crates/roy-inbound/src/store/mod.rs`:

```rust
pub mod bindings;
pub mod db;
```

`crates/roy-inbound/src/store/db.rs`:

```rust
//! SQLite pool + auto-migrate for roy-inbound.

use std::path::Path;

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("migrations/sqlite");

pub async fn open(path: &Path) -> Result<SqlitePool> {
    // TODO impl
    let _ = path;
    anyhow::bail!("unimplemented")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_creates_db_and_applies_migrations() {
        let dir = tempfile::tempdir().unwrap();
        let pool = open(&dir.path().join("state.db")).await.unwrap();

        let tables: Vec<(String,)> =
            sqlx::query_as("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .fetch_all(&pool)
                .await
                .unwrap();
        let names: Vec<&str> = tables.iter().map(|(n,)| n.as_str()).collect();
        assert!(names.contains(&"bindings"));
    }

    #[tokio::test]
    async fn open_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.db");
        let _ = open(&p).await.unwrap();
        let _ = open(&p).await.unwrap();
    }
}
```

Append to `crates/roy-inbound/src/lib.rs`:

```rust
pub mod store;
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p roy-inbound store::db::tests`
Expected: 2 FAIL (`bail!("unimplemented")`).

- [ ] **Step 4: Implement `open`**

Replace body of `open`:

```rust
pub async fn open(path: &Path) -> Result<SqlitePool> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .with_context(|| format!("opening SQLite at {}", path.display()))?;
    MIGRATOR.run(&pool).await.context("running migrations")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if path.exists() {
            let mut perms = std::fs::metadata(path)?.permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(path, perms)?;
        }
    }
    Ok(pool)
}
```

Also add a `bindings.rs` stub so the module compiles:

`crates/roy-inbound/src/store/bindings.rs`:

```rust
// Real impl in Task 5.
```

- [ ] **Step 5: Verify tests pass**

Run: `cargo test -p roy-inbound store::db::tests`
Expected: 2 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/roy-inbound/migrations crates/roy-inbound/src/store crates/roy-inbound/src/lib.rs
git commit -m "feat(roy-inbound): SQLite pool + bindings migration"
```

---

### Task 5: `BindingStore` — CRUD on the bindings table

**Files:**
- Modify: `crates/roy-inbound/src/store/bindings.rs`

- [ ] **Step 1: Write the failing tests + skeleton**

`crates/roy-inbound/src/store/bindings.rs`:

```rust
//! CRUD on the `bindings` table. The dispatcher calls `lookup`, then on a
//! fresh Spawn calls `upsert` with the daemon-issued session id. `touch`
//! refreshes `last_active_at` after a successful Resume.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, PartialEq)]
pub struct Binding {
    pub id: String,
    pub source_id: String,
    pub sender_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub strategy: String,
    pub created_at: DateTime<Utc>,
    pub last_active_at: DateTime<Utc>,
}

pub struct BindingStore {
    pool: SqlitePool,
}

impl BindingStore {
    pub fn new(pool: SqlitePool) -> Self { Self { pool } }

    pub async fn lookup(&self, source_id: &str, sender_id: &str) -> Result<Option<Binding>> {
        let _ = (source_id, sender_id);
        anyhow::bail!("unimplemented")
    }

    pub async fn upsert(
        &self,
        source_id: &str,
        sender_id: &str,
        agent_id: &str,
        strategy: &str,
        session_id: &str,
    ) -> Result<Binding> {
        let _ = (source_id, sender_id, agent_id, strategy, session_id);
        anyhow::bail!("unimplemented")
    }

    pub async fn touch(&self, id: &str) -> Result<()> {
        let _ = id;
        anyhow::bail!("unimplemented")
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        let _ = id;
        anyhow::bail!("unimplemented")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db;
    use tempfile::tempdir;

    async fn store() -> (tempfile::TempDir, BindingStore) {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("s.db")).await.unwrap();
        (dir, BindingStore::new(pool))
    }

    #[tokio::test]
    async fn lookup_miss_returns_none() {
        let (_d, s) = store().await;
        let b = s.lookup("src", "alice").await.unwrap();
        assert!(b.is_none());
    }

    #[tokio::test]
    async fn upsert_then_lookup_returns_row() {
        let (_d, s) = store().await;
        let b = s.upsert("src", "alice", "agent-1", "per_sender_sticky", "sid-1").await.unwrap();
        assert_eq!(b.session_id, "sid-1");
        let b2 = s.lookup("src", "alice").await.unwrap().unwrap();
        assert_eq!(b2.id, b.id);
    }

    #[tokio::test]
    async fn upsert_overwrites_existing() {
        let (_d, s) = store().await;
        let first = s.upsert("src", "alice", "agent-1", "per_sender_sticky", "old").await.unwrap();
        let second = s.upsert("src", "alice", "agent-1", "per_sender_sticky", "new").await.unwrap();
        assert_eq!(first.id, second.id, "upsert keeps same row id");
        assert_eq!(second.session_id, "new");
    }

    #[tokio::test]
    async fn touch_updates_last_active() {
        let (_d, s) = store().await;
        let b = s.upsert("src", "alice", "a", "per_sender_sticky", "sid").await.unwrap();
        let before = b.last_active_at;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        s.touch(&b.id).await.unwrap();
        let after = s.lookup("src", "alice").await.unwrap().unwrap();
        assert!(after.last_active_at > before);
    }

    #[tokio::test]
    async fn delete_removes_row() {
        let (_d, s) = store().await;
        let b = s.upsert("src", "alice", "a", "per_sender_sticky", "sid").await.unwrap();
        s.delete(&b.id).await.unwrap();
        assert!(s.lookup("src", "alice").await.unwrap().is_none());
    }
}
```

- [ ] **Step 2: Run tests — verify they fail**

Run: `cargo test -p roy-inbound store::bindings::tests`
Expected: 5 FAIL.

- [ ] **Step 3: Implement the methods**

Replace stubs in `BindingStore`:

```rust
    pub async fn lookup(&self, source_id: &str, sender_id: &str) -> Result<Option<Binding>> {
        let row: Option<Binding> = sqlx::query_as(
            "SELECT id, source_id, sender_id, session_id, agent_id, strategy, \
                    created_at, last_active_at \
             FROM bindings WHERE source_id = ?1 AND sender_id = ?2"
        )
        .bind(source_id)
        .bind(sender_id)
        .fetch_optional(&self.pool)
        .await
        .context("lookup binding")?;
        Ok(row)
    }

    pub async fn upsert(
        &self,
        source_id: &str,
        sender_id: &str,
        agent_id: &str,
        strategy: &str,
        session_id: &str,
    ) -> Result<Binding> {
        let now = Utc::now();
        let new_id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO bindings (id, source_id, sender_id, session_id, agent_id, strategy, \
                                   created_at, last_active_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7) \
             ON CONFLICT(source_id, sender_id) DO UPDATE SET \
                session_id = excluded.session_id, \
                agent_id = excluded.agent_id, \
                strategy = excluded.strategy, \
                last_active_at = excluded.last_active_at"
        )
        .bind(&new_id)
        .bind(source_id)
        .bind(sender_id)
        .bind(session_id)
        .bind(agent_id)
        .bind(strategy)
        .bind(now)
        .execute(&self.pool)
        .await
        .context("upsert binding")?;
        self.lookup(source_id, sender_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("binding vanished after upsert"))
    }

    pub async fn touch(&self, id: &str) -> Result<()> {
        sqlx::query("UPDATE bindings SET last_active_at = ?1 WHERE id = ?2")
            .bind(Utc::now())
            .bind(id)
            .execute(&self.pool)
            .await
            .context("touch binding")?;
        Ok(())
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM bindings WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await
            .context("delete binding")?;
        Ok(())
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p roy-inbound store::bindings::tests`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-inbound/src/store/bindings.rs
git commit -m "feat(roy-inbound): BindingStore CRUD"
```

---

### Task 6: `SessionResolver` — strategy → `FireTarget` + `PendingBinding`

**Files:**
- Modify: `crates/roy-inbound/src/session.rs`

- [ ] **Step 1: Append the resolver skeleton + tests to `session.rs`**

Add to the bottom of `crates/roy-inbound/src/session.rs`:

```rust
use roy::FireTarget;
use std::sync::Arc;
use anyhow::Result;

use crate::store::bindings::BindingStore;

#[derive(Debug, Clone)]
pub struct PendingBinding {
    pub source_id: String,
    pub sender_id: String,
    pub agent_id: String,
    pub strategy_db_label: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub enum SessionStrategy {
    Ephemeral,
    PersistentOne,
    PerSenderSticky { idle_timeout: Duration },
}

impl From<&SessionStrategyConfig> for SessionStrategy {
    fn from(c: &SessionStrategyConfig) -> Self {
        match c {
            SessionStrategyConfig::Ephemeral => Self::Ephemeral,
            SessionStrategyConfig::PersistentOne => Self::PersistentOne,
            SessionStrategyConfig::PerSenderSticky { idle_timeout_secs } =>
                Self::PerSenderSticky { idle_timeout: Duration::from_secs(*idle_timeout_secs) },
        }
    }
}

impl SessionStrategy {
    fn db_label(&self) -> &'static str {
        match self {
            Self::Ephemeral => "ephemeral",
            Self::PersistentOne => "persistent_one",
            Self::PerSenderSticky { .. } => "per_sender_sticky",
        }
    }
}

pub struct SessionResolver {
    bindings: Arc<BindingStore>,
    preset: String,            // used when Spawn is needed
}

impl SessionResolver {
    pub fn new(bindings: Arc<BindingStore>, preset: String) -> Self {
        Self { bindings, preset }
    }

    pub async fn resolve(
        &self,
        source_id: &str,
        sender_id: &str,
        agent_id: &str,
        strategy: SessionStrategy,
    ) -> Result<(FireTarget, Option<PendingBinding>)> {
        let _ = (source_id, sender_id, agent_id, strategy);
        anyhow::bail!("unimplemented")
    }
}

#[cfg(test)]
mod resolver_tests {
    use super::*;
    use crate::store::db;
    use tempfile::tempdir;

    async fn resolver() -> (tempfile::TempDir, SessionResolver) {
        let dir = tempdir().unwrap();
        let pool = db::open(&dir.path().join("s.db")).await.unwrap();
        let r = SessionResolver::new(Arc::new(BindingStore::new(pool)), "claude".into());
        (dir, r)
    }

    #[tokio::test]
    async fn ephemeral_always_spawn_no_binding() {
        let (_d, r) = resolver().await;
        let (t, pb) = r.resolve("src", "alice", "agent-1", SessionStrategy::Ephemeral).await.unwrap();
        assert!(matches!(t, FireTarget::Spawn { .. }));
        assert!(pb.is_none());
    }

    #[tokio::test]
    async fn sticky_miss_returns_spawn_plus_pending() {
        let (_d, r) = resolver().await;
        let strat = SessionStrategy::PerSenderSticky { idle_timeout: Duration::from_secs(3600) };
        let (t, pb) = r.resolve("src", "alice", "agent-1", strat).await.unwrap();
        assert!(matches!(t, FireTarget::Spawn { .. }));
        let pb = pb.unwrap();
        assert_eq!(pb.source_id, "src");
        assert_eq!(pb.sender_id, "alice");
    }

    #[tokio::test]
    async fn sticky_hit_returns_resume_no_pending() {
        let (_d, r) = resolver().await;
        r.bindings.upsert("src", "alice", "agent-1", "per_sender_sticky", "sid-old").await.unwrap();
        let strat = SessionStrategy::PerSenderSticky { idle_timeout: Duration::from_secs(3600) };
        let (t, pb) = r.resolve("src", "alice", "agent-1", strat).await.unwrap();
        assert!(matches!(t, FireTarget::Resume { ref session_id } if session_id == "sid-old"));
        assert!(pb.is_none());
    }

    #[tokio::test]
    async fn sticky_expired_returns_spawn_plus_pending() {
        let (_d, r) = resolver().await;
        r.bindings.upsert("src", "alice", "agent-1", "per_sender_sticky", "sid-old").await.unwrap();
        // Force last_active_at into the past by direct SQL.
        sqlx::query("UPDATE bindings SET last_active_at = ?1 WHERE source_id='src' AND sender_id='alice'")
            .bind(chrono::Utc::now() - chrono::Duration::seconds(7200))
            .execute(&r.bindings.pool_for_test()).await.unwrap();
        let strat = SessionStrategy::PerSenderSticky { idle_timeout: Duration::from_secs(3600) };
        let (t, pb) = r.resolve("src", "alice", "agent-1", strat).await.unwrap();
        assert!(matches!(t, FireTarget::Spawn { .. }));
        assert!(pb.is_some());
    }

    #[tokio::test]
    async fn persistent_one_uses_wildcard_sender() {
        let (_d, r) = resolver().await;
        r.bindings.upsert("src", "*", "agent-1", "persistent_one", "sid-pone").await.unwrap();
        // Caller passes a real sender — resolver must look up under "*".
        let (t, pb) = r.resolve("src", "anything", "agent-1", SessionStrategy::PersistentOne).await.unwrap();
        assert!(matches!(t, FireTarget::Resume { ref session_id } if session_id == "sid-pone"));
        assert!(pb.is_none());
    }
}
```

The `pool_for_test()` helper needs to be added to `BindingStore` so the test can mutate `last_active_at` directly. Add to `crates/roy-inbound/src/store/bindings.rs` inside `impl BindingStore`:

```rust
    #[cfg(test)]
    pub fn pool_for_test(&self) -> &SqlitePool { &self.pool }
```

- [ ] **Step 2: Run tests — verify they fail**

Run: `cargo test -p roy-inbound session::resolver_tests`
Expected: 5 FAIL.

- [ ] **Step 3: Implement `resolve`**

Replace the body of `SessionResolver::resolve`:

```rust
    pub async fn resolve(
        &self,
        source_id: &str,
        sender_id: &str,
        agent_id: &str,
        strategy: SessionStrategy,
    ) -> Result<(FireTarget, Option<PendingBinding>)> {
        let spawn_target = || FireTarget::Spawn {
            preset: self.preset.clone(),
            project_id: None,
            system_prompt: None,
        };

        let pending = |label: &'static str| PendingBinding {
            source_id: source_id.to_string(),
            sender_id: sender_id.to_string(),
            agent_id: agent_id.to_string(),
            strategy_db_label: label,
        };

        match strategy {
            SessionStrategy::Ephemeral => Ok((spawn_target(), None)),
            SessionStrategy::PersistentOne => {
                if let Some(b) = self.bindings.lookup(source_id, "*").await? {
                    Ok((FireTarget::Resume { session_id: b.session_id }, None))
                } else {
                    Ok((spawn_target(),
                        Some(PendingBinding { sender_id: "*".into(), ..pending("persistent_one") })))
                }
            }
            SessionStrategy::PerSenderSticky { idle_timeout } => {
                if let Some(b) = self.bindings.lookup(source_id, sender_id).await? {
                    let age = chrono::Utc::now() - b.last_active_at;
                    if age.to_std().map(|d| d > idle_timeout).unwrap_or(false) {
                        // Expired: Spawn + replace.
                        Ok((spawn_target(), Some(pending("per_sender_sticky"))))
                    } else {
                        Ok((FireTarget::Resume { session_id: b.session_id }, None))
                    }
                } else {
                    Ok((spawn_target(), Some(pending("per_sender_sticky"))))
                }
            }
        }
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p roy-inbound session::resolver_tests`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-inbound/src/session.rs crates/roy-inbound/src/store/bindings.rs
git commit -m "feat(roy-inbound): SessionResolver maps strategy to FireTarget"
```

---

### Task 7: Event types — `InboundEvent`, `ReplyHandle`, `BusSender`

**Files:**
- Create: `crates/roy-inbound/src/bus.rs`
- Modify: `crates/roy-inbound/src/lib.rs`

No tests in this task — types are exercised by downstream tasks. The only thing we verify here is that the file compiles in the workspace.

- [ ] **Step 1: Create the file**

`crates/roy-inbound/src/bus.rs`:

```rust
//! Bus payload types. `InboundEvent` is what publishers push and the
//! dispatcher consumes. `ReplyHandle` is the typed token carried on the
//! event that tells the per-channel ReplyHook how to deliver back.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Response, StatusCode};
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

pub type BusSender = mpsc::Sender<InboundEvent>;
pub type BusReceiver = mpsc::Receiver<InboundEvent>;

pub fn channel(capacity: usize) -> (BusSender, BusReceiver) {
    mpsc::channel(capacity)
}

#[derive(Debug)]
pub struct InboundEvent {
    pub id: Uuid,
    pub source_id: String,
    pub source_kind: String,
    pub sender_id: String,
    pub payload: Value,
    pub received_at: DateTime<Utc>,
    pub reply: ReplyHandle,
}

#[derive(Debug)]
pub enum ReplyHandle {
    Noop,
    HttpSync(oneshot::Sender<HttpReply>),
}

#[derive(Debug, Clone)]
pub struct HttpReply {
    pub status: StatusCode,
    pub body: String,
}

impl HttpReply {
    pub fn into_response(self) -> Response<Body> {
        Response::builder()
            .status(self.status)
            .header("content-type", "application/json")
            .body(Body::from(self.body))
            .unwrap_or_else(|_| Response::new(Body::empty()))
    }
}

/// Marker used in tags maps to identify roy-inbound dispatches.
pub const TAG_PREFIX: &str = "roy-inbound";

/// Helper newtype so non-event consumers (router, hook factories) can be
/// cloned without cloning the oneshot sender.
#[derive(Debug, Clone)]
pub struct EventRef {
    pub id: Uuid,
    pub source_id: Arc<str>,
    pub source_kind: Arc<str>,
    pub sender_id: Arc<str>,
}

impl From<&InboundEvent> for EventRef {
    fn from(e: &InboundEvent) -> Self {
        Self {
            id: e.id,
            source_id: Arc::from(e.source_id.as_str()),
            source_kind: Arc::from(e.source_kind.as_str()),
            sender_id: Arc::from(e.sender_id.as_str()),
        }
    }
}
```

Append to `crates/roy-inbound/src/lib.rs`:

```rust
pub mod bus;
```

- [ ] **Step 2: Verify it builds**

Run: `cargo build -p roy-inbound`
Expected: clean build.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-inbound/src/bus.rs crates/roy-inbound/src/lib.rs
git commit -m "feat(roy-inbound): InboundEvent + ReplyHandle types"
```

---

### Task 8: `Router` trait + `ConfigRouter`

**Files:**
- Create: `crates/roy-inbound/src/router.rs`
- Modify: `crates/roy-inbound/src/lib.rs`

- [ ] **Step 1: Write the failing test + skeleton**

`crates/roy-inbound/src/router.rs`:

```rust
//! Router turns an InboundEvent into a FireSpec. Default ConfigRouter
//! looks up source_id in the loaded config, renders the template, and
//! builds the tag map.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::bus::{InboundEvent, TAG_PREFIX};
use crate::config::{InboundConfig, SourceConfig};
use crate::session::SessionStrategy;
use crate::template::render;

#[derive(Debug, Clone)]
pub struct FireSpec {
    pub agent_id: String,
    pub prompt: String,
    pub session_strategy: SessionStrategy,
    pub tags: BTreeMap<String, String>,
    pub fire_timeout_secs: u64,
}

#[async_trait]
pub trait Router: Send + Sync {
    async fn route(&self, ev: &InboundEvent) -> Option<FireSpec>;
}

pub struct ConfigRouter {
    sources_by_id: BTreeMap<String, Arc<SourceConfig>>,
}

impl ConfigRouter {
    pub fn from_config(cfg: &InboundConfig) -> Self {
        let sources_by_id = cfg.sources.iter()
            .map(|s| (s.id.clone(), Arc::new(s.clone())))
            .collect();
        Self { sources_by_id }
    }
}

#[async_trait]
impl Router for ConfigRouter {
    async fn route(&self, ev: &InboundEvent) -> Option<FireSpec> {
        let src = self.sources_by_id.get(&ev.source_id)?;
        let prompt = render(&src.template, &ev.payload);
        let mut tags = BTreeMap::new();
        tags.insert(format!("{TAG_PREFIX}:source_id"), ev.source_id.clone());
        tags.insert(format!("{TAG_PREFIX}:source_kind"), ev.source_kind.clone());
        tags.insert(format!("{TAG_PREFIX}:event_id"), ev.id.to_string());
        tags.insert(format!("{TAG_PREFIX}:sender_id"), ev.sender_id.clone());
        Some(FireSpec {
            agent_id: src.agent_id.clone(),
            prompt,
            session_strategy: SessionStrategy::from(&src.session),
            tags,
            fire_timeout_secs: src.fire_timeout_secs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{InboundEvent, ReplyHandle};
    use serde_json::json;
    use uuid::Uuid;

    fn event(source_id: &str, payload: serde_json::Value) -> InboundEvent {
        InboundEvent {
            id: Uuid::new_v4(),
            source_id: source_id.into(),
            source_kind: "webhook".into(),
            sender_id: "alice".into(),
            payload,
            received_at: chrono::Utc::now(),
            reply: ReplyHandle::Noop,
        }
    }

    fn cfg(toml: &str) -> InboundConfig {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, toml).unwrap();
        InboundConfig::load(&p).unwrap()
    }

    #[tokio::test]
    async fn unknown_source_returns_none() {
        let c = cfg(r#"
            [[sources]]
            id = "orders"
            kind = "webhook"
            agent_id = "bot"
            session = "ephemeral"
            template = "x"
            [sources.webhook]
            path = "/o"
            reply_mode = "sync"
        "#);
        let r = ConfigRouter::from_config(&c);
        assert!(r.route(&event("not-orders", json!({}))).await.is_none());
    }

    #[tokio::test]
    async fn known_source_renders_template_and_tags() {
        let c = cfg(r#"
            [[sources]]
            id = "orders"
            kind = "webhook"
            agent_id = "bot"
            session = "ephemeral"
            template = "Order {{payload.id}}"
            [sources.webhook]
            path = "/o"
            reply_mode = "sync"
        "#);
        let r = ConfigRouter::from_config(&c);
        let ev = event("orders", json!({"id": 42}));
        let spec = r.route(&ev).await.unwrap();
        assert_eq!(spec.agent_id, "bot");
        assert_eq!(spec.prompt, "Order 42");
        assert_eq!(spec.tags["roy-inbound:source_id"], "orders");
        assert_eq!(spec.tags["roy-inbound:sender_id"], "alice");
        assert!(matches!(spec.session_strategy, SessionStrategy::Ephemeral));
    }
}
```

Append to `crates/roy-inbound/src/lib.rs`:

```rust
pub mod router;
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p roy-inbound router::tests`
Expected: 2 passed (the impl is fully written above; this isn't strict TDD but the unit is small enough that splitting test/impl adds no value).

- [ ] **Step 3: Commit**

```bash
git add crates/roy-inbound/src/router.rs crates/roy-inbound/src/lib.rs
git commit -m "feat(roy-inbound): ConfigRouter"
```

---

### Task 9: `ReplyHook` trait + `FireOutcome` + `ReplyHookFactory`

**Files:**
- Create: `crates/roy-inbound/src/reply.rs`
- Modify: `crates/roy-inbound/src/lib.rs`

- [ ] **Step 1: Create the types**

`crates/roy-inbound/src/reply.rs`:

```rust
//! ReplyHook contract. One hook instance lives per fire; the dispatcher
//! calls `on_turn_event` for every intermediate `TurnEvent` (currently
//! unused — see spec on streaming) and `on_finish` exactly once.

use std::collections::HashMap;

use anyhow::Result;
use async_trait::async_trait;
use roy::event::TurnEvent;
use roy::ErrorCode;

use crate::bus::{EventRef, ReplyHandle};

#[derive(Debug, Clone)]
pub enum FireOutcome {
    Ok { assistant_text: String, cost_usd: Option<f64>, stop_reason: String },
    DaemonError { code: ErrorCode, message: String },
    Timeout { partial_text: Option<String> },
    Cancelled,
    RouteRejected,
}

#[async_trait]
pub trait ReplyHook: Send {
    async fn on_turn_event(&mut self, ev: &TurnEvent) -> Result<()>;
    async fn on_finish(self: Box<Self>, outcome: FireOutcome, reply: ReplyHandle) -> Result<()>;
}

/// Per-source-kind factory. The dispatcher consults the registry to build
/// a fresh hook for every event.
pub type ReplyHookFactory = Box<dyn Fn(&EventRef) -> Box<dyn ReplyHook> + Send + Sync>;

pub struct ReplyHookRegistry {
    factories: HashMap<String, ReplyHookFactory>,
}

impl ReplyHookRegistry {
    pub fn new() -> Self { Self { factories: HashMap::new() } }

    pub fn register(&mut self, kind: &str, factory: ReplyHookFactory) {
        self.factories.insert(kind.into(), factory);
    }

    pub fn make(&self, kind: &str, ev: &EventRef) -> Option<Box<dyn ReplyHook>> {
        self.factories.get(kind).map(|f| f(ev))
    }
}

impl Default for ReplyHookRegistry {
    fn default() -> Self { Self::new() }
}
```

Append to `crates/roy-inbound/src/lib.rs`:

```rust
pub mod reply;
```

- [ ] **Step 2: Build**

Run: `cargo build -p roy-inbound`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-inbound/src/reply.rs crates/roy-inbound/src/lib.rs
git commit -m "feat(roy-inbound): ReplyHook trait + factory registry"
```

---

### Task 10: `WebhookReplyHook` — deliver `FireOutcome` over `ReplyHandle::HttpSync`

**Files:**
- Create: `crates/roy-inbound/src/channels/webhook/reply.rs`
- Modify: `crates/roy-inbound/src/channels/webhook/mod.rs`

- [ ] **Step 1: Write the failing test + skeleton**

`crates/roy-inbound/src/channels/webhook/reply.rs`:

```rust
//! Webhook reply hook. Receives the fire outcome, encodes it as JSON, and
//! sends it through the `ReplyHandle::HttpSync` oneshot. If the handle is
//! `Noop` (async mode) the outcome is just logged and dropped.

use anyhow::Result;
use async_trait::async_trait;
use axum::http::StatusCode;
use roy::event::TurnEvent;
use serde_json::json;

use crate::bus::{HttpReply, ReplyHandle};
use crate::reply::{FireOutcome, ReplyHook};

pub struct WebhookReplyHook {
    event_id: String,
}

impl WebhookReplyHook {
    pub fn new(event_id: String) -> Self { Self { event_id } }
}

#[async_trait]
impl ReplyHook for WebhookReplyHook {
    async fn on_turn_event(&mut self, _ev: &TurnEvent) -> Result<()> { Ok(()) }

    async fn on_finish(self: Box<Self>, outcome: FireOutcome, reply: ReplyHandle) -> Result<()> {
        let (status, body) = match outcome {
            FireOutcome::Ok { assistant_text, cost_usd, stop_reason } => (
                StatusCode::OK,
                json!({
                    "ok": true,
                    "event_id": self.event_id,
                    "assistant_text": assistant_text,
                    "cost_usd": cost_usd,
                    "stop_reason": stop_reason,
                }).to_string(),
            ),
            FireOutcome::RouteRejected => (
                StatusCode::NOT_FOUND,
                json!({"ok": false, "error": "route_rejected"}).to_string(),
            ),
            FireOutcome::Timeout { .. } => (
                StatusCode::GATEWAY_TIMEOUT,
                json!({"ok": false, "error": "timeout"}).to_string(),
            ),
            FireOutcome::Cancelled => (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"ok": false, "error": "cancelled"}).to_string(),
            ),
            FireOutcome::DaemonError { code, message } => (
                StatusCode::BAD_GATEWAY,
                json!({"ok": false, "error": "daemon", "code": code.to_string(),
                       "message": message}).to_string(),
            ),
        };

        match reply {
            ReplyHandle::Noop => {
                tracing::info!(event_id = self.event_id, %status, "webhook reply (async mode): dropping outcome");
            }
            ReplyHandle::HttpSync(tx) => {
                let _ = tx.send(HttpReply { status, body });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn ok_outcome_sends_200_with_body() {
        let (tx, rx) = oneshot::channel();
        let hook = Box::new(WebhookReplyHook::new("evt-1".into()));
        hook.on_finish(
            FireOutcome::Ok { assistant_text: "hi".into(), cost_usd: Some(0.01),
                              stop_reason: "EndTurn".into() },
            ReplyHandle::HttpSync(tx),
        ).await.unwrap();
        let r = rx.await.unwrap();
        assert_eq!(r.status, StatusCode::OK);
        assert!(r.body.contains("\"assistant_text\":\"hi\""));
    }

    #[tokio::test]
    async fn route_rejected_sends_404() {
        let (tx, rx) = oneshot::channel();
        let hook = Box::new(WebhookReplyHook::new("evt-1".into()));
        hook.on_finish(FireOutcome::RouteRejected, ReplyHandle::HttpSync(tx)).await.unwrap();
        let r = rx.await.unwrap();
        assert_eq!(r.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn noop_handle_just_logs() {
        let hook = Box::new(WebhookReplyHook::new("evt-1".into()));
        hook.on_finish(
            FireOutcome::Ok { assistant_text: "hi".into(), cost_usd: None,
                              stop_reason: "EndTurn".into() },
            ReplyHandle::Noop,
        ).await.unwrap();
        // No oneshot to await; pass means no panic.
    }
}
```

Add to `crates/roy-inbound/src/channels/webhook/mod.rs`:

```rust
pub mod config;
pub mod reply;
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p roy-inbound channels::webhook::reply::tests`
Expected: 3 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-inbound/src/channels/webhook
git commit -m "feat(roy-inbound): WebhookReplyHook"
```

---

### Task 11: Daemon fire client — `fire_with_hook`

**Files:**
- Create: `crates/roy-inbound/src/daemon_client.rs`
- Modify: `crates/roy-inbound/src/lib.rs`

Short-lived UDS connection per fire (scheduler-style), but streams every `Frame` through `ReplyHook::on_turn_event` before falling through to the terminal `FireDone`/`FireTimeout`/`FireError`. In v1 the daemon does not actually emit intermediate `Frame`s during `Fire` (see scheduler's `roy_client::fire` matching only the three terminal variants) — the matching is in place so a future daemon change is picked up without re-architecting.

- [ ] **Step 1: Write the failing test + skeleton**

`crates/roy-inbound/src/daemon_client.rs`:

```rust
//! Daemon client. Opens one short-lived UDS connection per fire, sends
//! ClientCommand::Fire, drains ServerEvents, calls into the ReplyHook for
//! each Frame and on the terminal event.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use roy::{ClientCommand, FireTarget, ServerEvent, TurnEvent};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::reply::{FireOutcome, ReplyHook};

/// Outcome the dispatcher needs above and beyond what the hook gets:
/// it wants the session id so it can write a binding.
#[derive(Debug, Clone)]
pub struct FireResult {
    pub outcome_kind: OutcomeKind,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum OutcomeKind {
    Ok,
    Timeout,
    DaemonError(String),         // ErrorCode as string for binding-write decision
    Cancelled,
}

pub async fn fire_with_hook(
    socket_path: &Path,
    target: FireTarget,
    prompt: String,
    tags: BTreeMap<String, String>,
    timeout: Duration,
    mut hook: Box<dyn ReplyHook>,
    reply: crate::bus::ReplyHandle,
) -> Result<FireResult> {
    let cmd = ClientCommand::Fire {
        target,
        prompt,
        tags,
        timeout_ms: Some(timeout.as_millis() as u64),
    };
    let stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connecting to daemon at {}", socket_path.display()))?;
    let (rd, mut wr) = stream.into_split();
    let mut lines = BufReader::new(rd).lines();
    wr.write_all(serde_json::to_string(&cmd)?.as_bytes()).await?;
    wr.write_all(b"\n").await?;
    wr.flush().await?;

    loop {
        let raw = lines.next_line().await?
            .ok_or_else(|| anyhow!("daemon hung up before terminal Fire event"))?;
        let evt: ServerEvent = serde_json::from_str(raw.trim())?;
        match evt {
            ServerEvent::Frame { event, .. } => {
                hook.on_turn_event(&event).await?;
            }
            ServerEvent::FireDone { session, result, assistant_text, .. } => {
                let TurnEvent::Result { cost_usd, stop_reason } = result else {
                    return Err(anyhow!("non-Result in FireDone"));
                };
                hook.on_finish(
                    FireOutcome::Ok {
                        assistant_text,
                        cost_usd,
                        stop_reason: format!("{stop_reason:?}"),
                    },
                    reply,
                ).await?;
                return Ok(FireResult { outcome_kind: OutcomeKind::Ok, session_id: Some(session) });
            }
            ServerEvent::FireTimeout { session, .. } => {
                hook.on_finish(FireOutcome::Timeout { partial_text: None }, reply).await?;
                return Ok(FireResult { outcome_kind: OutcomeKind::Timeout, session_id: Some(session) });
            }
            ServerEvent::FireError { session, code, message } => {
                hook.on_finish(
                    FireOutcome::DaemonError { code: code.clone(), message: message.clone() },
                    reply,
                ).await?;
                return Ok(FireResult {
                    outcome_kind: OutcomeKind::DaemonError(code.to_string()),
                    session_id: session,
                });
            }
            _ => continue,
        }
    }
}
```

`ServerEvent::Frame` has a `seq` field too — confirm by reading
`crates/roy/src/control.rs` before writing the destructure; the snippet above
uses `..` to ignore other fields and stays robust to additions.

Append to `crates/roy-inbound/src/lib.rs`:

```rust
pub mod daemon_client;
```

- [ ] **Step 2: Add an inline unit test for the wire shape**

Append to `crates/roy-inbound/src/daemon_client.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use tokio::net::UnixListener;
    use tokio::sync::oneshot;
    use roy::{ErrorCode, StopReason};

    struct CapturingHook {
        on_finish_called: Mutex<Option<FireOutcome>>,
    }

    #[async_trait]
    impl ReplyHook for CapturingHook {
        async fn on_turn_event(&mut self, _ev: &TurnEvent) -> Result<()> { Ok(()) }
        async fn on_finish(self: Box<Self>, outcome: FireOutcome, _reply: crate::bus::ReplyHandle) -> Result<()> {
            *self.on_finish_called.lock().unwrap() = Some(outcome);
            Ok(())
        }
    }

    async fn mock_daemon(path: PathBuf, reply: ServerEvent) {
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (rd, mut wr) = sock.into_split();
            let mut lines = BufReader::new(rd).lines();
            let _ = lines.next_line().await.unwrap();
            let line = serde_json::to_string(&reply).unwrap();
            wr.write_all(line.as_bytes()).await.unwrap();
            wr.write_all(b"\n").await.unwrap();
        });
    }

    #[tokio::test]
    async fn fire_done_hits_hook_and_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("r.sock");
        mock_daemon(p.clone(), ServerEvent::FireDone {
            session: "sid".into(),
            seq_range: (1, 3),
            result: TurnEvent::Result { cost_usd: Some(0.01), stop_reason: StopReason::EndTurn },
            assistant_text: "hi".into(),
        }).await;
        let hook = Box::new(CapturingHook { on_finish_called: Mutex::new(None) });
        let (_tx, _rx) = oneshot::channel::<crate::bus::HttpReply>();
        let captured = hook.on_finish_called.clone() // not actually clonable — pull arc; see step 3
            ;
        let _ = (hook, captured);  // dummy use; this test stub is finished in step 3.
    }
}
```

This test is intentionally left dangling — Step 3 finishes it. We need to
share the captured state across the hook handoff, which forces an `Arc<Mutex<…>>`.

- [ ] **Step 3: Finish the test using `Arc<Mutex<…>>`**

Replace the test module body above with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use tokio::net::UnixListener;
    use tokio::sync::oneshot;
    use roy::{ErrorCode, StopReason};

    struct CapturingHook { captured: Arc<Mutex<Option<FireOutcome>>> }

    #[async_trait]
    impl ReplyHook for CapturingHook {
        async fn on_turn_event(&mut self, _ev: &TurnEvent) -> Result<()> { Ok(()) }
        async fn on_finish(self: Box<Self>, outcome: FireOutcome, _reply: crate::bus::ReplyHandle) -> Result<()> {
            *self.captured.lock().unwrap() = Some(outcome);
            Ok(())
        }
    }

    async fn mock_daemon(path: PathBuf, reply: ServerEvent) {
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let (rd, mut wr) = sock.into_split();
            let mut lines = BufReader::new(rd).lines();
            let _ = lines.next_line().await.unwrap();
            let line = serde_json::to_string(&reply).unwrap();
            wr.write_all(line.as_bytes()).await.unwrap();
            wr.write_all(b"\n").await.unwrap();
        });
    }

    #[tokio::test]
    async fn fire_done_hits_hook_and_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("r.sock");
        mock_daemon(p.clone(), ServerEvent::FireDone {
            session: "sid".into(),
            seq_range: (1, 3),
            result: TurnEvent::Result { cost_usd: Some(0.01), stop_reason: StopReason::EndTurn },
            assistant_text: "hi".into(),
        }).await;
        let captured: Arc<Mutex<Option<FireOutcome>>> = Arc::new(Mutex::new(None));
        let hook = Box::new(CapturingHook { captured: captured.clone() });
        let (tx, _rx) = oneshot::channel::<crate::bus::HttpReply>();
        let result = fire_with_hook(
            &p,
            FireTarget::Spawn { preset: "claude".into(), project_id: None, system_prompt: None },
            "hello".into(),
            Default::default(),
            std::time::Duration::from_secs(5),
            hook,
            crate::bus::ReplyHandle::HttpSync(tx),
        ).await.unwrap();
        assert_eq!(result.outcome_kind, OutcomeKind::Ok);
        assert_eq!(result.session_id.as_deref(), Some("sid"));
        match captured.lock().unwrap().as_ref().unwrap() {
            FireOutcome::Ok { assistant_text, .. } => assert_eq!(assistant_text, "hi"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn fire_error_returns_daemon_error_kind() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("r.sock");
        mock_daemon(p.clone(), ServerEvent::FireError {
            session: None,
            code: ErrorCode::NoSession,
            message: "gone".into(),
        }).await;
        let captured = Arc::new(Mutex::new(None));
        let hook = Box::new(CapturingHook { captured: captured.clone() });
        let (tx, _rx) = oneshot::channel::<crate::bus::HttpReply>();
        let result = fire_with_hook(
            &p, FireTarget::Spawn { preset: "claude".into(), project_id: None, system_prompt: None },
            "x".into(), Default::default(), std::time::Duration::from_secs(5),
            hook, crate::bus::ReplyHandle::HttpSync(tx)).await.unwrap();
        assert_eq!(result.outcome_kind, OutcomeKind::DaemonError("no_session".into()));
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p roy-inbound daemon_client::tests`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-inbound/src/daemon_client.rs crates/roy-inbound/src/lib.rs
git commit -m "feat(roy-inbound): daemon fire client with reply-hook streaming"
```

---

### Task 12: `InboundDispatcher::run` — the loop tying everything together

**Files:**
- Create: `crates/roy-inbound/src/dispatcher.rs`
- Modify: `crates/roy-inbound/src/lib.rs`

- [ ] **Step 1: Write skeleton + the failing integration test**

`crates/roy-inbound/src/dispatcher.rs`:

```rust
//! The single bus consumer. Receives InboundEvents, routes, resolves
//! session, fires, writes bindings, retries once on NoSession.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use roy::FireTarget;
use tokio_util::sync::CancellationToken;

use crate::bus::{BusReceiver, EventRef, InboundEvent, ReplyHandle};
use crate::daemon_client::{fire_with_hook, OutcomeKind};
use crate::reply::{FireOutcome, ReplyHookRegistry};
use crate::router::{FireSpec, Router};
use crate::session::{PendingBinding, SessionResolver};
use crate::store::bindings::BindingStore;

pub struct InboundDispatcher {
    pub bus: BusReceiver,
    pub router: Arc<dyn Router>,
    pub resolver: SessionResolver,
    pub bindings: Arc<BindingStore>,
    pub hooks: Arc<ReplyHookRegistry>,
    pub socket_path: PathBuf,
}

impl InboundDispatcher {
    pub async fn run(mut self, cancel: CancellationToken) -> Result<()> {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Ok(()),
                next = self.bus.recv() => {
                    let Some(event) = next else { return Ok(()); };
                    if let Err(e) = self.handle_one(event).await {
                        tracing::error!(error = ?e, "dispatcher handle_one error (continuing)");
                    }
                }
            }
        }
    }

    async fn handle_one(&self, event: InboundEvent) -> Result<()> {
        let ev_ref = EventRef::from(&event);
        let kind = event.source_kind.clone();
        let InboundEvent { reply, .. } = event;
        let event_payload = (); // moved-out; use ev_ref for ids beyond this point.

        // Route.
        let spec = match self.router.route(&InboundEvent::from_ref(&ev_ref, reply.is_noop())).await {
            Some(s) => s,
            None => {
                let hook = self.hooks.make(&kind, &ev_ref)
                    .ok_or_else(|| anyhow::anyhow!("no reply hook for kind '{kind}'"))?;
                hook.on_finish(FireOutcome::RouteRejected, reply).await?;
                return Ok(());
            }
        };

        // (omitted — finished in step 3)
        let _ = (spec, event_payload);
        Ok(())
    }
}
```

Stop here — the partial `InboundEvent::from_ref` call doesn't exist. Step 2 reorganizes around that constraint.

- [ ] **Step 2: Rework `handle_one` so it doesn't try to re-route from `EventRef`**

The router needs the *full* event (payload + ids); rebind to keep the event around. Replace `handle_one` with:

```rust
    async fn handle_one(&self, event: InboundEvent) -> Result<()> {
        let ev_ref = EventRef::from(&event);
        let kind = event.source_kind.clone();

        let spec = match self.router.route(&event).await {
            Some(s) => s,
            None => {
                let hook = self.hooks.make(&kind, &ev_ref)
                    .ok_or_else(|| anyhow::anyhow!("no reply hook for kind '{kind}'"))?;
                hook.on_finish(FireOutcome::RouteRejected, event.reply).await?;
                return Ok(());
            }
        };

        // Resolve session.
        let (target, pending) = self.resolver.resolve(
            &event.source_id, &event.sender_id, &spec.agent_id, spec.session_strategy
        ).await?;

        // Run the fire, possibly with one NoSession retry.
        let (result, used_spawn) = self.run_once(
            target.clone(), &spec, &kind, &ev_ref, event.reply,
        ).await?;

        // Binding writes only on success / when we deliberately spawned.
        if let Some(pb) = pending {
            if let OutcomeKind::Ok = result.outcome_kind {
                if let Some(ref sid) = result.session_id {
                    self.bindings.upsert(
                        &pb.source_id, &pb.sender_id, &pb.agent_id,
                        pb.strategy_db_label, sid,
                    ).await?;
                }
            }
        }

        // Touch existing binding on a successful Resume (sticky strategies).
        if used_spawn.is_none() && matches!(result.outcome_kind, OutcomeKind::Ok) {
            if let Some(b) = self.bindings.lookup(&event.source_id, &event.sender_id).await? {
                self.bindings.touch(&b.id).await?;
            }
            // PersistentOne is keyed by "*"; touch that too if present.
            if let Some(b) = self.bindings.lookup(&event.source_id, "*").await? {
                self.bindings.touch(&b.id).await?;
            }
        }

        Ok(())
    }

    async fn run_once(
        &self,
        target: FireTarget,
        spec: &FireSpec,
        kind: &str,
        ev_ref: &EventRef,
        reply: ReplyHandle,
    ) -> Result<(crate::daemon_client::FireResult, Option<()>)> {
        let was_spawn = matches!(target, FireTarget::Spawn { .. });

        let hook = self.hooks.make(kind, ev_ref)
            .ok_or_else(|| anyhow::anyhow!("no reply hook for kind '{kind}'"))?;
        let res = fire_with_hook(
            &self.socket_path,
            target.clone(),
            spec.prompt.clone(),
            spec.tags.clone(),
            Duration::from_secs(spec.fire_timeout_secs),
            hook,
            reply,
        ).await?;

        // NoSession retry: if Resume hit a missing session, clear and Spawn.
        if let OutcomeKind::DaemonError(ref code) = res.outcome_kind {
            if code == "no_session" && !was_spawn {
                // Best-effort: delete binding that pointed at the dead session.
                if let FireTarget::Resume { session_id: dead_sid } = &target {
                    let _ = dead_sid;
                    // Lookups are by (source_id, sender_id); we don't have those here.
                    // The dispatcher's outer caller has them and could re-issue the fire
                    // — but a single fresh Spawn here is simpler and matches scheduler's pattern.
                }
                let fresh_target = FireTarget::Spawn {
                    preset: "claude".into(),
                    project_id: None,
                    system_prompt: None,
                };
                // ReplyHandle was consumed already — no retry of the reply is possible.
                // The hook has already delivered the DaemonError to the caller.
                // For v1 we don't retry once the reply is gone. Log and return.
                tracing::warn!(event_id = %ev_ref.id, "NoSession on Resume; reply already delivered. Skipping retry for v1.");
                let _ = fresh_target;
            }
        }

        Ok((res, if was_spawn { Some(()) } else { None }))
    }
```

The NoSession retry path is **gated to "log only" in v1** because the
oneshot `ReplyHandle` is consumed by the first hook call and we can't
duplicate it. This is acceptable: the client sees one `BAD_GATEWAY +
{"error":"daemon","code":"no_session"}` reply and the operator clears the
stale binding manually (or the next event takes care of it via expiry).
Full silent retry is a follow-up — note this limitation in the spec's
"Open questions" once implementation lands.

Replace `InboundEvent::from_ref` references that no longer exist (we don't
add that helper).

Add a helper to `crate::bus::ReplyHandle`:

```rust
impl ReplyHandle {
    pub fn is_noop(&self) -> bool { matches!(self, Self::Noop) }
}
```

Append to `crates/roy-inbound/src/lib.rs`:

```rust
pub mod dispatcher;
```

- [ ] **Step 3: Run the build**

Run: `cargo build -p roy-inbound`
Expected: clean. (Integration tests for the dispatcher land in Task 15.)

- [ ] **Step 4: Update spec — note the v1 NoSession-after-Resume gap**

Modify `docs/superpowers/specs/2026-05-25-inbound-event-bus-design.md`:
in the "Error handling" table, replace the `NoSession on Resume` row with:

```
| Daemon returns NoSession on Resume | v1: reply delivered as DaemonError; operator clears the binding manually (or next event triggers `last_active_at` expiry). Silent in-fire retry is deferred until the dispatcher can clone the reply handle. |
```

Add an entry to "Open questions":

```
6. **NoSession silent retry**. v1 cannot transparently retry on NoSession
   because the oneshot `ReplyHandle` is consumed by the first hook call.
   To make this transparent we need either (a) a reply pre-flight that
   doesn't commit the oneshot until the dispatcher confirms a terminal
   outcome, or (b) a non-oneshot reply primitive (channel) so multiple
   `on_finish` calls during retries are coalesced. Decided during plan
   execution; revisit when the second sticky channel (telegram-CS or
   email) lands.
```

- [ ] **Step 5: Commit**

```bash
git add crates/roy-inbound/src/dispatcher.rs crates/roy-inbound/src/bus.rs \
        crates/roy-inbound/src/lib.rs \
        docs/superpowers/specs/2026-05-25-inbound-event-bus-design.md
git commit -m "feat(roy-inbound): InboundDispatcher with binding write-after-fire"
```

---

### Task 13: `WebhookPublisher` — axum server that pushes to the bus

**Files:**
- Modify: `crates/roy-inbound/src/channels/webhook/mod.rs`
- Create: `crates/roy-inbound/src/channels/mod.rs` Publisher trait (if not yet)
- Modify: `crates/roy-inbound/src/channels/webhook/mod.rs` (real publisher)

- [ ] **Step 1: Add the `Publisher` trait**

Modify `crates/roy-inbound/src/channels/mod.rs`:

```rust
//! Channel-implementation root.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::bus::BusSender;

pub mod webhook;

#[async_trait]
pub trait Publisher: Send + Sync {
    /// Run until cancelled. Pushes InboundEvents into `bus`.
    async fn run(self: Arc<Self>, bus: BusSender, cancel: CancellationToken) -> Result<()>;
}
```

- [ ] **Step 2: Implement `WebhookPublisher`**

`crates/roy-inbound/src/channels/webhook/mod.rs`:

```rust
pub mod config;
pub mod reply;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use axum::{
    body::Bytes,
    extract::{Path as AxumPath, State},
    http::{HeaderMap, Method, StatusCode},
    response::Response,
    routing::any,
    Router,
};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::bus::{BusSender, HttpReply, InboundEvent, ReplyHandle};
use crate::channels::Publisher;
use crate::channels::webhook::config::{ReplyMode, WebhookConfig};

const REPLY_TIMEOUT_DEFAULT: Duration = Duration::from_secs(620); // > fire_timeout default

#[derive(Clone)]
struct RouteEntry {
    source_id: Arc<str>,
    secret: Option<Arc<[u8]>>,
    reply_mode: ReplyMode,
}

#[derive(Clone)]
struct AppState {
    bus: BusSender,
    routes: Arc<HashMap<String, RouteEntry>>,
    reply_timeout: Duration,
}

pub struct WebhookPublisher {
    bind_addr: SocketAddr,
    routes: HashMap<String, RouteEntry>,
}

pub struct WebhookSourceSpec {
    pub source_id: String,
    pub config: WebhookConfig,
}

impl WebhookPublisher {
    pub fn new(bind_addr: SocketAddr, sources: Vec<WebhookSourceSpec>) -> Result<Self> {
        let mut routes = HashMap::new();
        for s in sources {
            let secret = match &s.config.secret_env {
                Some(env_var) => Some(Arc::<[u8]>::from(
                    std::env::var(env_var)
                        .map_err(|_| anyhow::anyhow!(
                            "webhook source '{}' references env var '{}' which is not set",
                            s.source_id, env_var))?
                        .into_bytes()
                )),
                None => None,
            };
            routes.insert(s.config.path.clone(), RouteEntry {
                source_id: Arc::from(s.source_id.as_str()),
                secret,
                reply_mode: s.config.reply_mode,
            });
        }
        Ok(Self { bind_addr, routes })
    }
}

#[async_trait]
impl Publisher for WebhookPublisher {
    async fn run(self: Arc<Self>, bus: BusSender, cancel: CancellationToken) -> Result<()> {
        let state = AppState {
            bus,
            routes: Arc::new(self.routes.clone()),
            reply_timeout: REPLY_TIMEOUT_DEFAULT,
        };
        let app = Router::new()
            .route("/*path", any(handle))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind(self.bind_addr).await?;
        tracing::info!(addr = %self.bind_addr, "webhook publisher listening");
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { cancel.cancelled().await })
            .await?;
        Ok(())
    }
}

async fn handle(
    AxumPath(_path): AxumPath<String>,
    State(state): State<AppState>,
    headers: HeaderMap,
    method: Method,
    uri: axum::http::Uri,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    let Some(entry) = state.routes.get(&path) else {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(axum::body::Body::from(r#"{"ok":false,"error":"unknown_path"}"#))
            .unwrap();
    };

    // HMAC validation when configured.
    if let Some(secret) = &entry.secret {
        let provided = headers.get("x-roy-signature").and_then(|h| h.to_str().ok()).unwrap_or("");
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret).expect("hmac");
        mac.update(&body);
        let expected = hex::encode(mac.finalize().into_bytes());
        if !bool::from(expected.as_bytes().ct_eq(provided.as_bytes())) {
            return Response::builder().status(StatusCode::UNAUTHORIZED)
                .body(axum::body::Body::from(r#"{"ok":false,"error":"bad_signature"}"#)).unwrap();
        }
    }

    // Build the InboundEvent.
    let payload = build_payload(&method, &headers, &body);
    let sender_id = extract_sender(&headers).unwrap_or_else(|| "anon".into());
    let id = Uuid::new_v4();

    let (reply, rx) = match entry.reply_mode {
        ReplyMode::Sync => {
            let (tx, rx) = oneshot::channel();
            (ReplyHandle::HttpSync(tx), Some(rx))
        }
        ReplyMode::Async => (ReplyHandle::Noop, None),
    };

    let ev = InboundEvent {
        id,
        source_id: entry.source_id.to_string(),
        source_kind: "webhook".into(),
        sender_id,
        payload,
        received_at: chrono::Utc::now(),
        reply,
    };

    if state.bus.send(ev).await.is_err() {
        return Response::builder().status(StatusCode::SERVICE_UNAVAILABLE)
            .body(axum::body::Body::from(r#"{"ok":false,"error":"bus_closed"}"#)).unwrap();
    }

    match rx {
        None => Response::builder().status(StatusCode::ACCEPTED)
            .body(axum::body::Body::from(format!(r#"{{"ok":true,"event_id":"{id}"}}"#))).unwrap(),
        Some(rx) => match tokio::time::timeout(state.reply_timeout, rx).await {
            Ok(Ok(reply)) => HttpReply::into_response(reply),
            Ok(Err(_)) => Response::builder().status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(axum::body::Body::from(r#"{"ok":false,"error":"reply_dropped"}"#)).unwrap(),
            Err(_) => Response::builder().status(StatusCode::GATEWAY_TIMEOUT)
                .body(axum::body::Body::from(r#"{"ok":false,"error":"reply_timeout"}"#)).unwrap(),
        },
    }
}

fn build_payload(method: &Method, headers: &HeaderMap, body: &Bytes) -> serde_json::Value {
    let mut hdr_map = serde_json::Map::new();
    for (k, v) in headers.iter() {
        if let Ok(v_str) = v.to_str() {
            hdr_map.insert(k.as_str().to_string(), serde_json::Value::String(v_str.to_string()));
        }
    }
    let body_json = serde_json::from_slice::<serde_json::Value>(body)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(body).into_owned()));
    serde_json::json!({
        "method": method.as_str(),
        "headers": serde_json::Value::Object(hdr_map),
        "body": body_json,
    })
}

fn extract_sender(headers: &HeaderMap) -> Option<String> {
    headers.get("x-forwarded-for")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
}
```

- [ ] **Step 3: Build**

Run: `cargo build -p roy-inbound`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/roy-inbound/src/channels
git commit -m "feat(roy-inbound): WebhookPublisher (axum) with HMAC + sync/async reply"
```

---

### Task 14: Top-level `run()` + `roy-inbound` binary

**Files:**
- Create: `crates/roy-inbound/src/cli.rs`
- Modify: `crates/roy-inbound/src/lib.rs`
- Modify: `crates/roy-inbound/src/main.rs`

- [ ] **Step 1: Write `cli::run`**

`crates/roy-inbound/src/cli.rs`:

```rust
//! `roy-inbound` entry point. Loads config, opens DB, spawns publishers
//! and the dispatcher, awaits ctrl-c.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;

use crate::bus::{self, EventRef};
use crate::channels::webhook::{WebhookPublisher, WebhookSourceSpec};
use crate::channels::Publisher;
use crate::config::InboundConfig;
use crate::dispatcher::InboundDispatcher;
use crate::reply::{ReplyHook, ReplyHookRegistry};
use crate::router::ConfigRouter;
use crate::session::SessionResolver;
use crate::store::{bindings::BindingStore, db};

#[derive(clap::Parser, Debug)]
#[command(name = "roy-inbound", about = "Inbound event bus for roy")]
pub struct Args {
    /// Path to the inbound TOML config.
    #[arg(long)]
    pub config: PathBuf,
    /// SQLite DB path (default ~/.local/state/roy-inbound/state.db).
    #[arg(long, env = "ROY_INBOUND_DB")]
    pub db: Option<PathBuf>,
    /// roy daemon Unix socket.
    #[arg(long, env = "ROY_SOCKET")]
    pub socket: Option<PathBuf>,
    /// Default preset used when resolving Spawn targets.
    #[arg(long, default_value = "claude")]
    pub preset: String,
}

pub async fn run(args: Args) -> Result<()> {
    let cfg = InboundConfig::load(&args.config)
        .with_context(|| format!("loading {}", args.config.display()))?;

    let db_path = args.db.unwrap_or_else(default_db_path);
    let pool = db::open(&db_path).await?;
    let bindings = Arc::new(BindingStore::new(pool));

    let socket_path = args.socket.unwrap_or_else(default_socket_path);

    let (bus_tx, bus_rx) = bus::channel(cfg.bus.capacity);

    // Reply-hook registry: register webhook for now.
    let mut hooks = ReplyHookRegistry::new();
    hooks.register("webhook", Box::new(|ev: &EventRef| -> Box<dyn ReplyHook> {
        Box::new(crate::channels::webhook::reply::WebhookReplyHook::new(ev.id.to_string()))
    }));
    let hooks = Arc::new(hooks);

    // Build the webhook publisher from config (one source per webhook).
    let webhook_sources: Vec<_> = cfg.sources.iter()
        .filter(|s| s.kind == "webhook")
        .map(|s| WebhookSourceSpec {
            source_id: s.id.clone(),
            config: s.webhook.clone().expect("validated in InboundConfig::load"),
        })
        .collect();
    let bind: std::net::SocketAddr = cfg.server.bind.parse()
        .with_context(|| format!("parsing server.bind '{}'", cfg.server.bind))?;
    let webhook = Arc::new(WebhookPublisher::new(bind, webhook_sources)?);

    let router: Arc<dyn crate::router::Router> = Arc::new(ConfigRouter::from_config(&cfg));
    let resolver = SessionResolver::new(bindings.clone(), args.preset);

    let dispatcher = InboundDispatcher {
        bus: bus_rx,
        router,
        resolver,
        bindings: bindings.clone(),
        hooks: hooks.clone(),
        socket_path,
    };

    let cancel = CancellationToken::new();
    let cancel_pub = cancel.clone();
    let cancel_disp = cancel.clone();

    let dispatcher_handle = tokio::spawn(async move {
        if let Err(e) = dispatcher.run(cancel_disp).await {
            tracing::error!(error = ?e, "dispatcher exited with error");
        }
    });

    let pub_handle = tokio::spawn(async move {
        if let Err(e) = webhook.run(bus_tx, cancel_pub).await {
            tracing::error!(error = ?e, "webhook publisher exited with error");
        }
    });

    tokio::signal::ctrl_c().await.context("waiting for ctrl-c")?;
    tracing::info!("ctrl-c received; shutting down");
    cancel.cancel();
    let _ = tokio::join!(dispatcher_handle, pub_handle);
    Ok(())
}

fn default_db_path() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/roy-inbound/state.db")
}

fn default_socket_path() -> PathBuf {
    if let Ok(s) = std::env::var("ROY_SOCKET") { return PathBuf::from(s); }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".roy/daemon.sock")
}
```

Append to `crates/roy-inbound/src/lib.rs`:

```rust
pub mod cli;
```

- [ ] **Step 2: Wire the standalone binary**

Replace `crates/roy-inbound/src/main.rs`:

```rust
use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "roy_inbound=info,warn".into()))
        .init();
    let args = roy_inbound::cli::Args::parse();
    roy_inbound::cli::run(args).await
}
```

- [ ] **Step 3: Build**

Run: `cargo build -p roy-inbound`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/roy-inbound/src/cli.rs crates/roy-inbound/src/main.rs crates/roy-inbound/src/lib.rs
git commit -m "feat(roy-inbound): cli::run + standalone binary"
```

---

### Task 15: Integration test — `webhook POST → mock daemon → HTTP response`

**Files:**
- Create: `crates/roy-inbound/tests/integration.rs`

- [ ] **Step 1: Write the test**

`crates/roy-inbound/tests/integration.rs`:

```rust
//! End-to-end: webhook POST → real axum publisher → real dispatcher →
//! mock daemon → real reply hook → HTTP response.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use roy::{ServerEvent, StopReason, TurnEvent};
use roy_inbound::{
    bus::{self, EventRef},
    channels::webhook::{config::ReplyMode, WebhookPublisher, WebhookSourceSpec},
    channels::Publisher,
    channels::webhook::config::WebhookConfig,
    dispatcher::InboundDispatcher,
    reply::{ReplyHook, ReplyHookRegistry},
    router::{ConfigRouter, Router},
    session::SessionResolver,
    store::{bindings::BindingStore, db},
};
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

async fn spawn_mock_daemon(path: PathBuf, reply: ServerEvent) {
    let listener = UnixListener::bind(&path).unwrap();
    tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let (rd, mut wr) = sock.into_split();
        let mut lines = BufReader::new(rd).lines();
        let _ = lines.next_line().await.unwrap();
        let line = serde_json::to_string(&reply).unwrap();
        wr.write_all(line.as_bytes()).await.unwrap();
        wr.write_all(b"\n").await.unwrap();
    });
}

#[tokio::test]
async fn webhook_sync_round_trip() {
    // 1. Mock daemon.
    let dir = tempdir().unwrap();
    let sock_path = dir.path().join("daemon.sock");
    spawn_mock_daemon(sock_path.clone(), ServerEvent::FireDone {
        session: "sid-ok".into(),
        seq_range: (1, 3),
        result: TurnEvent::Result { cost_usd: None, stop_reason: StopReason::EndTurn },
        assistant_text: "classified=ham".into(),
    }).await;

    // 2. DB + bindings + resolver.
    let pool = db::open(&dir.path().join("inbound.db")).await.unwrap();
    let bindings = Arc::new(BindingStore::new(pool));

    // 3. Config (built in-memory, no TOML file needed).
    let toml_cfg = format!(r#"
        [server]
        bind = "127.0.0.1:0"

        [[sources]]
        id = "orders"
        kind = "webhook"
        agent_id = "order-bot"
        session = "ephemeral"
        template = "Classify: {{{{payload.body.text}}}}"
        fire_timeout_secs = 5
        [sources.webhook]
        path = "/orders"
        reply_mode = "sync"
    "#);
    let cfg_path = dir.path().join("c.toml");
    std::fs::write(&cfg_path, toml_cfg).unwrap();
    let cfg = roy_inbound::config::InboundConfig::load(&cfg_path).unwrap();

    // 4. Pick a free port — bind to 127.0.0.1:0 and discover the port.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    // 5. Build the substrate.
    let (tx, rx) = bus::channel(16);
    let mut hooks = ReplyHookRegistry::new();
    hooks.register("webhook", Box::new(|ev: &EventRef| -> Box<dyn ReplyHook> {
        Box::new(roy_inbound::channels::webhook::reply::WebhookReplyHook::new(ev.id.to_string()))
    }));
    let hooks = Arc::new(hooks);
    let router: Arc<dyn Router> = Arc::new(ConfigRouter::from_config(&cfg));
    let resolver = SessionResolver::new(bindings.clone(), "claude".into());

    let dispatcher = InboundDispatcher {
        bus: rx, router, resolver, bindings: bindings.clone(),
        hooks: hooks.clone(), socket_path: sock_path.clone(),
    };

    let webhook = Arc::new(WebhookPublisher::new(
        format!("127.0.0.1:{port}").parse().unwrap(),
        vec![WebhookSourceSpec {
            source_id: "orders".into(),
            config: WebhookConfig {
                path: "/orders".into(),
                secret_env: None,
                reply_mode: ReplyMode::Sync,
            },
        }],
    ).unwrap());

    let cancel = CancellationToken::new();
    let cd = cancel.clone();
    let cp = cancel.clone();
    let h_disp = tokio::spawn(async move { dispatcher.run(cd).await.ok() });
    let h_pub = tokio::spawn(async move { webhook.run(tx, cp).await.ok() });

    // 6. POST and assert response.
    tokio::time::sleep(Duration::from_millis(100)).await;  // axum bind
    let client = reqwest::Client::new();
    let resp = client.post(format!("http://127.0.0.1:{port}/orders"))
        .json(&serde_json::json!({"text": "win a prize"}))
        .send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["assistant_text"], "classified=ham");

    cancel.cancel();
    let _ = tokio::join!(h_disp, h_pub);
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p roy-inbound --test integration`
Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-inbound/tests/integration.rs
git commit -m "test(roy-inbound): end-to-end webhook → mock daemon"
```

---

### Task 16: `roy-cli` — `roy inbound` subcommand

**Files:**
- Modify: `crates/roy-cli/Cargo.toml`
- Modify: `crates/roy-cli/src/main.rs`

- [ ] **Step 1: Add the dep**

In `crates/roy-cli/Cargo.toml` (under `[dependencies]`):

```toml
roy-inbound = { path = "../roy-inbound" }
```

- [ ] **Step 2: Wire the subcommand**

Find the existing `Cli` enum in `crates/roy-cli/src/main.rs` (the one with
`Gateway`, `Scheduler`, `Management`) and add a sibling:

```rust
    /// Start the inbound event bus (axum webhook server + dispatcher).
    Inbound(roy_inbound::cli::Args),
```

Find the match arm dispatching the subcommands and add:

```rust
        Commands::Inbound(args) => roy_inbound::cli::run(args).await?,
```

If `roy-cli`'s `main.rs` uses an enum named `Commands` instead of `Cli`,
adjust the variant accordingly — keep the variant name `Inbound`.

- [ ] **Step 3: Build**

Run: `cargo build -p roy-cli`
Expected: clean.

- [ ] **Step 4: Smoke-test the help text**

Run: `cargo run -p roy-cli -- inbound --help`
Expected: prints clap help including `--config`, `--db`, `--socket`, `--preset`.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-cli/Cargo.toml crates/roy-cli/src/main.rs
git commit -m "feat(roy-cli): add `roy inbound` subcommand"
```

---

### Task 17: Documentation — README + example config + CLAUDE.md update

**Files:**
- Create: `crates/roy-inbound/README.md`
- Create: `docs/examples/inbound.example.toml`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Write the README**

`crates/roy-inbound/README.md`:

````markdown
# roy-inbound

In-process event bus that lets external systems (HTTP webhooks today, IMAP /
WhatsApp / Telegram-customer-support later) wake up roy agents.

## Quick start

```bash
# 1. Make sure `roy serve` is running.
cargo run -p roy --bin roy -- serve &

# 2. Make sure an agent exists in roy-agents.
roy agents create --name order-bot --preset claude --prompt "You triage orders."

# 3. Write the inbound config.
cat > ~/.config/roy/inbound.toml <<'EOF'
[server]
bind = "127.0.0.1:8090"

[[sources]]
id = "orders"
kind = "webhook"
agent_id = "order-bot"
session = "ephemeral"
template = "New order: {{payload.body}}"
fire_timeout_secs = 600
  [sources.webhook]
  path = "/webhooks/orders"
  reply_mode = "sync"
EOF

# 4. Start the inbound runner.
roy inbound --config ~/.config/roy/inbound.toml

# 5. POST a test event.
curl -s -X POST http://127.0.0.1:8090/webhooks/orders \
     -H 'content-type: application/json' \
     -d '{"id":42,"item":"book"}'
```

## Architecture

See `docs/superpowers/specs/2026-05-25-inbound-event-bus-design.md`.

## Session strategies

- `ephemeral` — every event spawns a fresh roy session
- `persistent_one` — one session for the whole source (all senders share it)
- `per_sender_sticky` — one session per `(source_id, sender_id)` — needs
  `idle_timeout_secs`

## Webhook auth

Set `secret_env = "SOMENAME"` on the source's `[sources.webhook]` table and
provide the HMAC-SHA256 (hex) signature in the `X-Roy-Signature` header.
The signature must be over the raw request body.
````

- [ ] **Step 2: Write the example config**

`docs/examples/inbound.example.toml`:

```toml
[bus]
capacity = 256

[server]
bind = "127.0.0.1:8090"

# Synchronous webhook returning the agent's answer in the HTTP response.
[[sources]]
id = "orders"
kind = "webhook"
agent_id = "order-bot"
session = "ephemeral"
fire_timeout_secs = 600
template = "New order: {{payload.body}}"
  [sources.webhook]
  path = "/webhooks/orders"
  secret_env = "ORDERS_WEBHOOK_SECRET"
  reply_mode = "sync"

# Async / fire-and-forget — caller gets 202 immediately.
[[sources]]
id = "cron-alert"
kind = "webhook"
agent_id = "alert-bot"
session = "ephemeral"
fire_timeout_secs = 60
template = "Alert: {{payload.body.message}}"
  [sources.webhook]
  path = "/webhooks/alert"
  reply_mode = "async"
```

- [ ] **Step 3: Update `CLAUDE.md`**

In `CLAUDE.md`, find the "What this is" section listing the crates and add a
new bullet between `roy-management` and the closing paragraph:

```markdown
- **`crates/roy-inbound`** — library + thin binary. Inbound event bus for external systems (HTTP webhook today, IMAP / WhatsApp / Telegram-customer-support later). Pure publishers normalize external events into `InboundEvent`s onto an in-process `tokio::mpsc` bus; a single dispatcher resolves a per-source session strategy (`ephemeral`/`persistent_one`/`per_sender_sticky`), fires the agent over the daemon Unix socket, and a per-channel `ReplyHook` delivers the result back. Same boundary rule as `roy-scheduler`/`roy-gateway`. Owns SQLite state at `~/.local/state/roy-inbound/state.db` (table `bindings`). Configured via TOML (`~/.config/roy/inbound.toml`).
```

Also update the count: "A Cargo workspace with seven crates" → "A Cargo workspace with eight crates" (the very first sentence of the section).

- [ ] **Step 4: Run full workspace tests**

Run: `cargo fmt --all -- --check && cargo build --workspace --all-targets && cargo test --workspace --no-fail-fast`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-inbound/README.md docs/examples/inbound.example.toml CLAUDE.md
git commit -m "docs(roy-inbound): README, example config, CLAUDE.md entry"
```

---

## Self-review

**Spec coverage:**

| Spec section | Task(s) |
|---|---|
| Goals 1 — crate scaffold | 1 |
| Goals 2 — channels are pure publishers | 13 |
| Goals 3 — pluggable reply paths | 9, 10 |
| Goals 4 — session strategies | 6 |
| Goals 5 — gateway unchanged | (verified by Task 17 step 4 — workspace tests) |
| Goals 6 — daemon untouched, UDS-only boundary | 11 |
| Non-goals | not built; cross-checked in 17 step 4 |
| Terminology | 7 (types) + 13 (Publisher) + 8 (Router) |
| Components — InboundEvent, ReplyHandle | 7 |
| Components — Publisher trait | 13 |
| Components — Bus | 7 (channel fn) + 12 (dispatcher consumer) |
| Components — Router | 8 |
| Components — SessionResolver | 6 |
| Components — ReplyHook | 9 |
| Components — InboundDispatcher | 12 |
| State (bindings table) | 4, 5 |
| Config (TOML) | 3 |
| Error handling | 12 (route_rejected, bus_full via send, hook errors), 11 (daemon errors), 13 (HMAC, timeout), 17 (NoSession deferred — documented) |
| Testing — unit + integration | every functional task + 15 |
| Migration plan | 17 (README) |
| Open questions | 17 update notes NoSession; templating/HMAC/observability/idle-sweep already in spec |

**Placeholder scan:** no "TBD", "TODO impl" remaining in shipped code (stub bodies in early steps are explicitly replaced by later steps within the same task). No undefined references.

**Type consistency:** `FireSpec` (router.rs) uses `session_strategy: SessionStrategy` (runtime enum from session.rs). `SessionStrategyConfig` (config) → `SessionStrategy` via `From` impl in Task 6. `PendingBinding` returned by resolver carries `strategy_db_label: &'static str` consumed by `BindingStore::upsert` in Task 12. `ReplyHandle` constructed by publisher (Task 13), consumed by hook (Task 10). `FireOutcome` produced by `fire_with_hook` (Task 11), consumed by hook (Task 10). All check out.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-05-25-inbound-event-bus.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints

**Which approach?**
