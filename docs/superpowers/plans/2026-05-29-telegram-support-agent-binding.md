# Telegram Support (bot→agent, per-user sessions) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a Telegram bot be bound to an agent persona (managed from the roy web UI), so every Telegram user gets their own sticky session with that agent — built as a new Telegram channel in `roy-inbound`.

**Architecture:** Two-plane state. **Config** (`channel_bindings` table in `roy-management`'s `agents.db`, web-UI managed) answers "which agent does bot B run?". **Runtime** (`bindings` table in `roy-inbound`, already exists) answers "which session is sender S talking to?". `roy-inbound` learns the bot token + agent persona by calling a read-only, control-plane-only HTTP endpoint on `roy-management` (boundary decision **A1** from the spec); the daemon is still reached only over its Unix socket via `Fire`.

**Tech Stack:** Rust (edition 2021), `serde`/`serde_json`, `sqlx` (SQLite), `axum` 0.8, `teloxide` 0.13, `reqwest`, `tokio`, `async_trait`, `anyhow`. Spec: `docs/superpowers/specs/2026-05-29-telegram-support-agent-binding-design.md`.

**Commit convention:** every commit message in this plan ends with the trailer
`Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>` (omitted from the short `-m` examples below for brevity — add it).

---

## File Structure

**Slice 1 — control plane (`roy-protocol` + `roy-management`)**
- Create: `crates/roy-protocol/src/channel.rs` — `TelegramSource`, `SessionStrategyWire` wire DTOs.
- Modify: `crates/roy-protocol/src/lib.rs` — register + re-export `channel`.
- Modify: `crates/roy-management/src/connections.rs` — accept `telegram_bot` kind.
- Modify: `crates/roy-management/src/agents.rs` — add `read_agent_persona`.
- Create: `crates/roy-management/migrations/sqlite/0002_channel_bindings.sql` — new table.
- Create: `crates/roy-management/src/channel_bindings.rs` — store + CRUD + internal endpoint.
- Modify: `crates/roy-management/src/state.rs` — add `channel_bindings` + `internal_token` to `AppState`.
- Modify: `crates/roy-management/src/auth.rs` — add `require_internal_token` middleware.
- Modify: `crates/roy-management/src/http.rs` — mount the new routers.
- Modify: `crates/roy-management/src/lib.rs` — read `ROY_INTERNAL_TOKEN`; declare module; populate `AppState`.
- Modify: `crates/roy-management/tests/common/mod.rs` — populate the two new `AppState` fields.
- Create: `crates/roy-management/tests/telegram_bindings.rs` — HTTP integration tests.

**Slice 2 — channel skeleton (`roy-inbound`)**
- Modify: `crates/roy-inbound/Cargo.toml` — add `teloxide`, `dptree`, `reqwest`.
- Modify: `crates/roy-inbound/src/router.rs` — add `harness`/`system_prompt` to `FireSpec`; add `CompositeRouter`.
- Modify: `crates/roy-inbound/src/session.rs` — `resolve()` takes persona overrides.
- Modify: `crates/roy-inbound/src/dispatcher.rs` — pass persona to `resolve()`.
- Create: `crates/roy-inbound/src/channels/telegram/mod.rs` — registry, resolved source, management client, publisher.
- Create: `crates/roy-inbound/src/channels/telegram/reply.rs` — `TelegramReplyHook`, `TgSender`.
- Modify: `crates/roy-inbound/src/channels/mod.rs` — `pub mod telegram;`.
- Modify: `crates/roy-inbound/src/cli.rs` — Args (management url/token), wire the Telegram channel.

**Slice 3 — reconciliation & resilience (`roy-inbound`)**
- Modify: `crates/roy-inbound/src/channels/telegram/mod.rs` — poll loop, `diff_sources`, reconcile, retry/backoff.

**Slice 4 (deferred — separate plan):** streaming edit UX (port `DraftStream`/typing/`/cancel` from `roy-gateway`).

---

# SLICE 1 — Control plane

## Task 1.1: `roy-protocol` channel DTOs

**Files:**
- Create: `crates/roy-protocol/src/channel.rs`
- Modify: `crates/roy-protocol/src/lib.rs`
- Test: in `crates/roy-protocol/src/channel.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write the failing test** — create `crates/roy-protocol/src/channel.rs` with the types and a round-trip test:

```rust
//! Wire DTOs for inbound channels managed by `roy-management` and consumed by
//! `roy-inbound`. Control-plane only (config), never session operations.

use serde::{Deserialize, Serialize};

/// One Telegram bot resolved to its agent persona. Returned by
/// `roy-management`'s `GET /internal/telegram-sources` and consumed by the
/// `roy-inbound` Telegram channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelegramSource {
    /// Stable per-bot id: `"tg:<connection_id>"`.
    pub source_id: String,
    pub bot_token: String,
    /// Agent slug (record-keeping; stored in the runtime binding).
    pub agent_slug: String,
    pub harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub session_strategy: SessionStrategyWire,
    /// Empty = public (any Telegram user may message the bot).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_user_ids: Vec<i64>,
}

/// Wire form of the per-source session strategy (mirrors
/// `roy_inbound::session::SessionStrategyConfig`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionStrategyWire {
    Ephemeral,
    PersistentOne,
    PerSenderSticky { idle_timeout_secs: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telegram_source_round_trips() {
        let src = TelegramSource {
            source_id: "tg:conn-1".into(),
            bot_token: "123:abc".into(),
            agent_slug: "support-l1".into(),
            harness: "claude".into(),
            system_prompt: Some("You are support.".into()),
            model: None,
            session_strategy: SessionStrategyWire::PerSenderSticky {
                idle_timeout_secs: 3600,
            },
            allowed_user_ids: vec![],
        };
        let json = serde_json::to_string(&src).unwrap();
        // empty + None fields omitted
        assert!(!json.contains("allowed_user_ids"));
        assert!(!json.contains("model"));
        assert!(json.contains(r#""kind":"per_sender_sticky""#));
        let back: TelegramSource = serde_json::from_str(&json).unwrap();
        assert_eq!(src, back);
    }

    #[test]
    fn strategy_short_variants_use_kind_tag() {
        let j = serde_json::to_string(&SessionStrategyWire::Ephemeral).unwrap();
        assert_eq!(j, r#"{"kind":"ephemeral"}"#);
    }
}
```

- [ ] **Step 2: Run test to verify it fails** — the module is not yet wired into `lib.rs`, so the crate won't see it.

Run: `cargo test -p roy-protocol channel::`
Expected: FAIL — `error: module 'channel' ... not found` / unresolved.

- [ ] **Step 3: Wire the module** — in `crates/roy-protocol/src/lib.rs`, add the module declaration alphabetically (after `pub mod bus;`? there is none — the list is `control, error, event, harnesses, journal, pid_lock, wire`). Insert `pub mod channel;` right after `pub mod control;`, and add a re-export line after the existing `pub use control::{...};`:

```rust
pub mod channel;
```
and
```rust
pub use channel::{SessionStrategyWire, TelegramSource};
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p roy-protocol channel::`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/roy-protocol/src/channel.rs crates/roy-protocol/src/lib.rs
git commit -m "feat(roy-protocol): add channel DTOs (TelegramSource, SessionStrategyWire)"
```

## Task 1.2: Accept `telegram_bot` connection kind

**Files:**
- Modify: `crates/roy-management/src/connections.rs:79-130` (kind/config validators)
- Test: `crates/roy-management/src/connections.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write the failing test** — add to the existing `#[cfg(test)] mod tests` in `connections.rs`:

```rust
    #[test]
    fn accepts_telegram_bot_kind() {
        assert!(validate_kind(KIND_TELEGRAM_BOT).is_ok());
        // token lives in secrets, so an empty config object is valid
        validate_config(KIND_TELEGRAM_BOT, &json!({})).unwrap();
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p roy-management connections::tests::accepts_telegram_bot_kind`
Expected: FAIL — `KIND_TELEGRAM_BOT` not found / `validate_kind` returns Err.

- [ ] **Step 3: Implement** — in `connections.rs`, add the constant next to `KIND_MCP_STDIO` (line 79) and extend both validators:

```rust
pub const KIND_TELEGRAM_BOT: &str = "telegram_bot";
```

Change `validate_kind` (lines 82-89) to:
```rust
pub fn validate_kind(kind: &str) -> Result<(), String> {
    match kind {
        KIND_MCP_STDIO | KIND_TELEGRAM_BOT => Ok(()),
        other => Err(format!(
            "unsupported connection kind '{other}'; supported: 'mcp_stdio', 'telegram_bot'"
        )),
    }
}
```

In `validate_config` (lines 93-130), add a `telegram_bot` arm before the catch-all `_`:
```rust
        KIND_TELEGRAM_BOT => {
            // The bot token is a secret, not config. Only require an object.
            if !config.is_object() {
                return Err("config must be an object".to_string());
            }
            Ok(())
        }
```

- [ ] **Step 4: Run test to verify it passes** (and the existing `rejects_unknown_kind` still passes — it asserts `mcp_http`/`nango` err, which still holds).

Run: `cargo test -p roy-management connections::tests`
Expected: PASS (all connection tests).

- [ ] **Step 5: Commit**

```bash
git add crates/roy-management/src/connections.rs
git commit -m "feat(roy-management): accept telegram_bot connection kind"
```

## Task 1.3: `agents::read_agent_persona`

**Files:**
- Modify: `crates/roy-management/src/agents.rs`
- Test: `crates/roy-management/src/agents.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write the failing test** — add to `agents.rs` `#[cfg(test)] mod tests`:

```rust
    #[tokio::test]
    async fn read_persona_by_slug() {
        let home = TempDir::new().unwrap();
        let dir = home.path().join("agents");
        write(
            &dir,
            "support-l1.md",
            "---\nname: Support\ndescription: d\nharness: claude\nmodel: claude-opus-4-8\n---\n\nYou are support.\n",
        );
        let (harness, model, body) = read_agent_persona(&dir, "support-l1").await.unwrap();
        assert_eq!(harness, "claude");
        assert_eq!(model.as_deref(), Some("claude-opus-4-8"));
        assert!(body.contains("You are support."));

        // unsafe slug / missing file / no harness → None
        assert!(read_agent_persona(&dir, "../escape").await.is_none());
        assert!(read_agent_persona(&dir, "missing").await.is_none());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p roy-management agents::tests::read_persona_by_slug`
Expected: FAIL — `read_agent_persona` not found.

- [ ] **Step 3: Implement** — add this public fn to `agents.rs` (after `list_all_agents`, before `struct ParsedAgent`). It reuses the private `is_safe_agent_name` + `parse_agent_md`:

```rust
/// Resolve a single agent file `<dir>/<slug>.md` to its persona, returning
/// `(harness, model, system_prompt_body)`. `None` if the slug is unsafe, the
/// file is missing/unparseable, or it lacks the required `harness` field.
pub async fn read_agent_persona(
    dir: &Path,
    slug: &str,
) -> Option<(String, Option<String>, String)> {
    if !is_safe_agent_name(slug) {
        return None;
    }
    let path = dir.join(format!("{slug}.md"));
    let contents = tokio::fs::read_to_string(&path).await.ok()?;
    let parsed = parse_agent_md(&contents)?;
    let harness = parsed.harness?;
    Some((harness, parsed.model, parsed.body))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p roy-management agents::tests::read_persona_by_slug`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-management/src/agents.rs
git commit -m "feat(roy-management): add agents::read_agent_persona(dir, slug)"
```

## Task 1.4: `channel_bindings` migration + Store

**Files:**
- Create: `crates/roy-management/migrations/sqlite/0002_channel_bindings.sql`
- Create: `crates/roy-management/src/channel_bindings.rs`
- Modify: `crates/roy-management/src/lib.rs` (add `pub mod channel_bindings;`)
- Test: `crates/roy-management/src/channel_bindings.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write the migration**

Create `crates/roy-management/migrations/sqlite/0002_channel_bindings.sql`:
```sql
CREATE TABLE channel_bindings (
    id                TEXT PRIMARY KEY,
    owner_id          TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    channel_kind      TEXT NOT NULL,
    connection_id     TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
    agent_slug        TEXT NOT NULL,
    agent_scope       TEXT NOT NULL,
    session_strategy  TEXT NOT NULL,
    idle_timeout_secs INTEGER,
    allowed_user_ids  TEXT,
    enabled           INTEGER NOT NULL DEFAULT 1,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    UNIQUE (connection_id)
);
CREATE INDEX channel_bindings_owner_idx ON channel_bindings(owner_id);
```

- [ ] **Step 2: Write the failing test** — create `crates/roy-management/src/channel_bindings.rs` with the store + types + tests (HTTP added in Task 1.6):

```rust
//! Channel→agent bindings: which agent persona a Telegram bot runs, and with
//! what session strategy. Web-UI managed (CRUD), read by `roy-inbound` via the
//! internal endpoint (see `internal_telegram_sources`). Owner is always a user.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

pub const CHANNEL_TELEGRAM: &str = "telegram";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChannelBinding {
    pub id: String,
    pub owner_id: String,
    pub channel_kind: String,
    pub connection_id: String,
    pub agent_slug: String,
    pub agent_scope: String,
    pub session_strategy: String,
    pub idle_timeout_secs: Option<i64>,
    pub allowed_user_ids: Vec<i64>,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Request body for `POST /channel-bindings`.
#[derive(Debug, Clone, Deserialize)]
pub struct NewChannelBinding {
    pub connection_id: String,
    pub agent_slug: String,
    /// "user" | "team:<team_id>"
    pub agent_scope: String,
    #[serde(default = "default_strategy")]
    pub session_strategy: String,
    #[serde(default)]
    pub idle_timeout_secs: Option<i64>,
    #[serde(default)]
    pub allowed_user_ids: Vec<i64>,
}

fn default_strategy() -> String {
    "per_sender_sticky".to_string()
}

#[derive(sqlx::FromRow)]
struct BindingRow {
    id: String,
    owner_id: String,
    channel_kind: String,
    connection_id: String,
    agent_slug: String,
    agent_scope: String,
    session_strategy: String,
    idle_timeout_secs: Option<i64>,
    allowed_user_ids: Option<String>,
    enabled: i64,
    created_at: i64,
    updated_at: i64,
}

fn row_to_binding(r: BindingRow) -> ChannelBinding {
    let allowed_user_ids = r
        .allowed_user_ids
        .as_deref()
        .and_then(|s| serde_json::from_str::<Vec<i64>>(s).ok())
        .unwrap_or_default();
    ChannelBinding {
        id: r.id,
        owner_id: r.owner_id,
        channel_kind: r.channel_kind,
        connection_id: r.connection_id,
        agent_slug: r.agent_slug,
        agent_scope: r.agent_scope,
        session_strategy: r.session_strategy,
        idle_timeout_secs: r.idle_timeout_secs,
        allowed_user_ids,
        enabled: r.enabled != 0,
        created_at: r.created_at,
        updated_at: r.updated_at,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("binding not found: {0}")]
    NotFound(String),
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

const SELECT_COLS: &str = "id, owner_id, channel_kind, connection_id, agent_slug, agent_scope, \
     session_strategy, idle_timeout_secs, allowed_user_ids, enabled, created_at, updated_at";

impl Store {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert a Telegram binding. Caller has already validated the connection,
    /// agent, and strategy. `allowed_user_ids` is stored as a JSON array.
    pub async fn create(
        &self,
        owner_id: &str,
        new: &NewChannelBinding,
    ) -> Result<ChannelBinding, StoreError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp();
        let allowed = serde_json::to_string(&new.allowed_user_ids)
            .map_err(|e| StoreError::Invalid(format!("allowed_user_ids: {e}")))?;
        let res = sqlx::query(
            "INSERT INTO channel_bindings
             (id, owner_id, channel_kind, connection_id, agent_slug, agent_scope,
              session_strategy, idle_timeout_secs, allowed_user_ids, enabled, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?)",
        )
        .bind(&id)
        .bind(owner_id)
        .bind(CHANNEL_TELEGRAM)
        .bind(&new.connection_id)
        .bind(&new.agent_slug)
        .bind(&new.agent_scope)
        .bind(&new.session_strategy)
        .bind(new.idle_timeout_secs)
        .bind(&allowed)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await;
        match res {
            Ok(_) => self.get(owner_id, &id).await,
            Err(sqlx::Error::Database(d)) if d.is_unique_violation() => Err(StoreError::Invalid(
                "this bot is already bound to an agent".to_string(),
            )),
            Err(e) => Err(StoreError::Db(e)),
        }
    }

    pub async fn list_by_owner(&self, owner_id: &str) -> Result<Vec<ChannelBinding>, StoreError> {
        let rows: Vec<BindingRow> = sqlx::query_as(&format!(
            "SELECT {SELECT_COLS} FROM channel_bindings WHERE owner_id = ? ORDER BY created_at DESC"
        ))
        .bind(owner_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_binding).collect())
    }

    pub async fn get(&self, owner_id: &str, id: &str) -> Result<ChannelBinding, StoreError> {
        let row: Option<BindingRow> = sqlx::query_as(&format!(
            "SELECT {SELECT_COLS} FROM channel_bindings WHERE owner_id = ? AND id = ?"
        ))
        .bind(owner_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_binding)
            .ok_or_else(|| StoreError::NotFound(id.to_string()))
    }

    pub async fn delete(&self, owner_id: &str, id: &str) -> Result<(), StoreError> {
        let res = sqlx::query("DELETE FROM channel_bindings WHERE owner_id = ? AND id = ?")
            .bind(owner_id)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(StoreError::NotFound(id.to_string()));
        }
        Ok(())
    }

    /// All enabled Telegram bindings across every owner. Used by the internal
    /// endpoint to build the source list for `roy-inbound`.
    pub async fn list_enabled_telegram(&self) -> Result<Vec<ChannelBinding>, StoreError> {
        let rows: Vec<BindingRow> = sqlx::query_as(&format!(
            "SELECT {SELECT_COLS} FROM channel_bindings \
             WHERE channel_kind = ? AND enabled = 1"
        ))
        .bind(CHANNEL_TELEGRAM)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(row_to_binding).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use roy_auth::test_support::make_user;

    async fn setup_pool() -> SqlitePool {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = crate::db::open(&dir.path().join("agents.db"))
            .await
            .unwrap();
        roy_auth::apply_migrations(&pool).await.unwrap();
        std::mem::forget(dir);
        pool
    }

    async fn make_conn(pool: &SqlitePool, owner_id: &str) -> String {
        let store = crate::connections::Store::new(pool.clone());
        let c = store
            .create_custom(
                owner_id,
                crate::connections::NewConnectionCustom {
                    name: "My Bot".into(),
                    kind: crate::connections::KIND_TELEGRAM_BOT.into(),
                    config: serde_json::json!({}),
                    secrets: Some(serde_json::json!({"bot_token": "123:abc"})),
                    description: None,
                },
            )
            .await
            .unwrap();
        c.id
    }

    #[tokio::test]
    async fn create_list_get_delete() {
        let pool = setup_pool().await;
        let user = make_user(&pool, "alice").await;
        let conn_id = make_conn(&pool, &user.id).await;
        let store = Store::new(pool.clone());

        let b = store
            .create(
                &user.id,
                &NewChannelBinding {
                    connection_id: conn_id.clone(),
                    agent_slug: "support-l1".into(),
                    agent_scope: "user".into(),
                    session_strategy: "per_sender_sticky".into(),
                    idle_timeout_secs: Some(3600),
                    allowed_user_ids: vec![],
                },
            )
            .await
            .unwrap();
        assert_eq!(b.connection_id, conn_id);
        assert!(b.enabled);

        assert_eq!(store.list_by_owner(&user.id).await.unwrap().len(), 1);
        assert_eq!(store.list_enabled_telegram().await.unwrap().len(), 1);
        assert_eq!(store.get(&user.id, &b.id).await.unwrap().id, b.id);

        store.delete(&user.id, &b.id).await.unwrap();
        assert!(store.list_by_owner(&user.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn one_bot_one_binding() {
        let pool = setup_pool().await;
        let user = make_user(&pool, "alice").await;
        let conn_id = make_conn(&pool, &user.id).await;
        let store = Store::new(pool.clone());
        let new = NewChannelBinding {
            connection_id: conn_id,
            agent_slug: "a".into(),
            agent_scope: "user".into(),
            session_strategy: "ephemeral".into(),
            idle_timeout_secs: None,
            allowed_user_ids: vec![],
        };
        store.create(&user.id, &new).await.unwrap();
        let err = store.create(&user.id, &new).await.unwrap_err();
        assert!(matches!(err, StoreError::Invalid(_)));
    }
}
```

- [ ] **Step 3: Declare the module** — in `crates/roy-management/src/lib.rs`, add `pub mod channel_bindings;` next to `pub mod connections;` (or wherever `connections` is declared).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p roy-management channel_bindings::tests`
Expected: PASS (2 tests). The migration is picked up automatically by `sqlx::migrate!("migrations/sqlite")`.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-management/migrations/sqlite/0002_channel_bindings.sql \
        crates/roy-management/src/channel_bindings.rs crates/roy-management/src/lib.rs
git commit -m "feat(roy-management): channel_bindings table + store"
```

## Task 1.5: `AppState` fields + constructors

**Files:**
- Modify: `crates/roy-management/src/state.rs`
- Modify: `crates/roy-management/src/lib.rs` (the `AppState { ... }` literal in `run`)
- Modify: `crates/roy-management/tests/common/mod.rs` (the `AppState { ... }` literal in `test_app`)

- [ ] **Step 1: Add the fields** — in `state.rs`, add to `AppState` (after `connections`):

```rust
    pub channel_bindings: crate::channel_bindings::Store,
    /// Bearer token gating `GET /internal/telegram-sources`. `None` disables it.
    pub internal_token: Option<String>,
```

- [ ] **Step 2: Populate in `run`** — in `crates/roy-management/src/lib.rs`, before constructing `AppState`, read the env var; then add the two fields to the `AppState { ... }` literal:

```rust
    let internal_token = std::env::var("ROY_INTERNAL_TOKEN").ok().filter(|s| s.len() >= 32);
    if internal_token.is_none() {
        tracing::info!("ROY_INTERNAL_TOKEN unset or <32 bytes; /internal/telegram-sources disabled");
    }
```
and inside the literal:
```rust
        channel_bindings: crate::channel_bindings::Store::new(pool.clone()),
        internal_token,
```

- [ ] **Step 3: Populate in `test_app`** — in `crates/roy-management/tests/common/mod.rs`, add to the `AppState { ... }` literal:

```rust
        channel_bindings: roy_management::channel_bindings::Store::new(pool.clone()),
        internal_token: Some("test-internal-token-0123456789abcdef".to_string()),
```

- [ ] **Step 4: Verify it compiles** (and the whole crate still builds + existing tests pass).

Run: `cargo test -p roy-management --no-run && cargo test -p roy-management`
Expected: builds; existing tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-management/src/state.rs crates/roy-management/src/lib.rs \
        crates/roy-management/tests/common/mod.rs
git commit -m "feat(roy-management): AppState channel_bindings store + internal_token"
```

## Task 1.6: HTTP — CRUD router, internal endpoint, auth gate, wiring

**Files:**
- Modify: `crates/roy-management/src/channel_bindings.rs` (add HTTP section)
- Modify: `crates/roy-management/src/auth.rs` (add `require_internal_token`)
- Modify: `crates/roy-management/src/http.rs` (mount routers)
- Test: `crates/roy-management/tests/telegram_bindings.rs`

- [ ] **Step 1: Write the failing integration test** — create `crates/roy-management/tests/telegram_bindings.rs`:

```rust
mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::test_app;
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn login(app: &axum::Router, pool: &sqlx::SqlitePool) -> String {
    roy_auth::test_support::make_user(pool, "alice").await;
    let body =
        serde_json::to_vec(&serde_json::json!({"username":"alice","password":"test-password-1234"}))
            .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::post("/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    resp.headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}

async fn json_body(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[serial_test::serial]
#[tokio::test]
async fn bind_bot_then_internal_endpoint_resolves_persona() {
    let (app, pool, workspace) = test_app().await;
    let cookie = login(&app, &pool).await;

    // 1. Create a telegram_bot connection.
    let conn_body = serde_json::json!({
        "name": "Support Bot",
        "kind": "telegram_bot",
        "config": {},
        "secrets": {"bot_token": "111:AAA"}
    });
    let resp = app
        .clone()
        .oneshot(
            Request::post("/connections")
                .header("cookie", &cookie)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&conn_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let conn = json_body(resp).await;
    let conn_id = conn["id"].as_str().unwrap().to_string();

    // 2. Write an agent file in the owner's personal scope.
    let uid = conn["owner_id"].as_str().unwrap();
    let agent_dir = workspace.join("users").join(uid).join(".roy/agents");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("support-l1.md"),
        "---\nname: Support\ndescription: d\nharness: claude\n---\n\nYou are support.\n",
    )
    .unwrap();

    // 3. Bind the bot to the agent.
    let bind_body = serde_json::json!({
        "connection_id": conn_id,
        "agent_slug": "support-l1",
        "agent_scope": "user",
        "session_strategy": "per_sender_sticky",
        "idle_timeout_secs": 3600
    });
    let resp = app
        .clone()
        .oneshot(
            Request::post("/channel-bindings")
                .header("cookie", &cookie)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&bind_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // 4. Internal endpoint (bearer) returns the resolved source.
    let resp = app
        .clone()
        .oneshot(
            Request::get("/internal/telegram-sources")
                .header("authorization", "Bearer test-internal-token-0123456789abcdef")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let sources = json_body(resp).await;
    let arr = sources.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["source_id"], format!("tg:{conn_id}"));
    assert_eq!(arr[0]["bot_token"], "111:AAA");
    assert_eq!(arr[0]["harness"], "claude");
    assert_eq!(arr[0]["system_prompt"], "You are support.\n");
    assert_eq!(arr[0]["session_strategy"]["kind"], "per_sender_sticky");

    // 5. Internal endpoint without the token → 401.
    let resp = app
        .oneshot(
            Request::get("/internal/telegram-sources")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
```

> If `http_body_util` / `tower` are not already dev-deps of `roy-management`, check `crates/roy-management/Cargo.toml` `[dev-dependencies]` — the existing `tests/auth_flow.rs` uses `tower::ServiceExt::oneshot`, so `tower` is present; add `http-body-util` if the existing tests don't already use it (grep `into_body().collect()` in `tests/`).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p roy-management --test telegram_bindings`
Expected: FAIL — routes `/channel-bindings` and `/internal/telegram-sources` 404 (not mounted yet).

- [ ] **Step 3: Add the HTTP section to `channel_bindings.rs`** — append:

```rust
// ---------------- HTTP ----------------

use axum::{
    extract::{Path as AxPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Extension, Json, Router,
};
use std::path::{Path, PathBuf};

use crate::auth::AuthUser;
use crate::state::AppState;
use roy_protocol::channel::{SessionStrategyWire, TelegramSource};

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({"error": self.1}))).into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound(id) => ApiError(StatusCode::NOT_FOUND, format!("not found: {id}")),
            StoreError::Invalid(m) => ApiError(StatusCode::BAD_REQUEST, m),
            StoreError::Db(e) => {
                tracing::error!(error = %e, "channel_bindings db error");
                ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
            }
        }
    }
}

/// Authenticated CRUD, mounted behind `require_user`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/channel-bindings", get(list_handler).post(create_handler))
        .route("/channel-bindings/{id}", get(get_handler).delete(delete_handler))
}

/// Internal source list, mounted behind `require_internal_token`.
pub fn internal_router() -> Router<AppState> {
    Router::new().route("/internal/telegram-sources", get(internal_telegram_sources))
}

async fn list_handler(
    Extension(AuthUser(uid)): Extension<AuthUser>,
    State(s): State<AppState>,
) -> Result<Json<Vec<ChannelBinding>>, ApiError> {
    Ok(Json(s.channel_bindings.list_by_owner(&uid).await?))
}

async fn get_handler(
    Extension(AuthUser(uid)): Extension<AuthUser>,
    State(s): State<AppState>,
    AxPath(id): AxPath<String>,
) -> Result<Json<ChannelBinding>, ApiError> {
    Ok(Json(s.channel_bindings.get(&uid, &id).await?))
}

async fn delete_handler(
    Extension(AuthUser(uid)): Extension<AuthUser>,
    State(s): State<AppState>,
    AxPath(id): AxPath<String>,
) -> Result<StatusCode, ApiError> {
    s.channel_bindings.delete(&uid, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_handler(
    Extension(AuthUser(uid)): Extension<AuthUser>,
    State(s): State<AppState>,
    Json(new): Json<NewChannelBinding>,
) -> Result<(StatusCode, Json<ChannelBinding>), ApiError> {
    // Validate strategy.
    match new.session_strategy.as_str() {
        "ephemeral" | "persistent_one" => {}
        "per_sender_sticky" => {
            if new.idle_timeout_secs.is_none() {
                return Err(ApiError(
                    StatusCode::BAD_REQUEST,
                    "per_sender_sticky requires idle_timeout_secs".into(),
                ));
            }
        }
        other => {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                format!("unknown session_strategy '{other}'"),
            ))
        }
    }
    // Validate the connection: owned, telegram_bot, has a non-empty bot_token.
    let conn = s
        .connections
        .get(&uid, &new.connection_id)
        .await
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "unknown connection".into()))?;
    if conn.kind != crate::connections::KIND_TELEGRAM_BOT {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "connection is not a telegram_bot".into(),
        ));
    }
    let has_token = conn
        .secrets
        .as_ref()
        .and_then(|v| v.get("bot_token"))
        .and_then(|v| v.as_str())
        .is_some_and(|t| !t.is_empty());
    if !has_token {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "connection has no bot_token secret".into(),
        ));
    }
    // Validate the agent resolves in the requested scope.
    let dir = scope_dir(&s.workspace_dir, &uid, &new.agent_scope).ok_or_else(|| {
        ApiError(StatusCode::BAD_REQUEST, "invalid agent_scope".into())
    })?;
    if crate::agents::read_agent_persona(&dir, &new.agent_slug)
        .await
        .is_none()
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            format!("agent '{}' not found in scope", new.agent_slug),
        ));
    }
    let b = s.channel_bindings.create(&uid, &new).await?;
    Ok((StatusCode::CREATED, Json(b)))
}

/// `<workspace>/users/<owner>/.roy/agents` for `"user"`, or
/// `<workspace>/teams/<tid>/.roy/agents` for `"team:<tid>"`.
fn scope_dir(workspace_dir: &Path, owner_id: &str, scope: &str) -> Option<PathBuf> {
    if scope == "user" {
        Some(workspace_dir.join("users").join(owner_id).join(".roy/agents"))
    } else if let Some(tid) = scope.strip_prefix("team:") {
        if tid.is_empty() {
            None
        } else {
            Some(workspace_dir.join("teams").join(tid).join(".roy/agents"))
        }
    } else {
        None
    }
}

async fn internal_telegram_sources(State(s): State<AppState>) -> Json<Vec<TelegramSource>> {
    Json(resolve_telegram_sources(&s.channel_bindings, &s.connections, &s.workspace_dir).await)
}

/// Resolve all enabled Telegram bindings to self-contained sources. Bindings
/// whose connection or agent fails to resolve are skipped with a warning.
pub(crate) async fn resolve_telegram_sources(
    bindings: &Store,
    connections: &crate::connections::Store,
    workspace_dir: &Path,
) -> Vec<TelegramSource> {
    let rows = match bindings.list_enabled_telegram().await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "listing telegram bindings");
            return vec![];
        }
    };
    let mut out = Vec::new();
    for b in rows {
        let conn = match connections.get(&b.owner_id, &b.connection_id).await {
            Ok(c) => c,
            Err(_) => {
                tracing::warn!(binding = b.id, "telegram binding: connection gone; skipping");
                continue;
            }
        };
        let token = conn
            .secrets
            .as_ref()
            .and_then(|v| v.get("bot_token"))
            .and_then(|v| v.as_str())
            .filter(|t| !t.is_empty());
        let Some(token) = token else {
            tracing::warn!(binding = b.id, "telegram binding: no bot_token; skipping");
            continue;
        };
        let Some(dir) = scope_dir(workspace_dir, &b.owner_id, &b.agent_scope) else {
            tracing::warn!(binding = b.id, scope = b.agent_scope, "bad agent_scope; skipping");
            continue;
        };
        let Some((harness, model, body)) =
            crate::agents::read_agent_persona(&dir, &b.agent_slug).await
        else {
            tracing::warn!(binding = b.id, slug = b.agent_slug, "agent unresolved; skipping");
            continue;
        };
        out.push(TelegramSource {
            source_id: format!("tg:{}", b.connection_id),
            bot_token: token.to_string(),
            agent_slug: b.agent_slug,
            harness,
            system_prompt: Some(body),
            model,
            session_strategy: strategy_to_wire(&b.session_strategy, b.idle_timeout_secs),
            allowed_user_ids: b.allowed_user_ids,
        });
    }
    out
}

fn strategy_to_wire(name: &str, idle: Option<i64>) -> SessionStrategyWire {
    match name {
        "persistent_one" => SessionStrategyWire::PersistentOne,
        "per_sender_sticky" => SessionStrategyWire::PerSenderSticky {
            idle_timeout_secs: idle.unwrap_or(3600).max(0) as u64,
        },
        _ => SessionStrategyWire::Ephemeral,
    }
}
```

- [ ] **Step 4: Add `require_internal_token` to `auth.rs`** — append:

```rust
/// Middleware gating internal, service-to-service endpoints with a bearer token
/// matched against `AppState::internal_token`. 503 if the server has no token
/// configured; 401 if the header is missing or wrong.
pub async fn require_internal_token(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let Some(expected) = state.internal_token.as_deref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "internal endpoint disabled"})),
        )
            .into_response();
    };
    let provided = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if provided == Some(expected) {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "bad internal token"})),
        )
            .into_response()
    }
}
```

> The imports `State`, `Request`, `Body`, `Next`, `Response`, `StatusCode`, `header`, `Json` are already used by `require_user` in this file — no new `use` needed.

- [ ] **Step 5: Mount the routers in `http.rs`** — in `pub fn router(state: AppState)`:
  1. add `.merge(crate::channel_bindings::router())` to the `protected` chain (next to `.merge(crate::connections::router())`);
  2. build the internal tier and merge it at the top level:

```rust
    let internal = crate::channel_bindings::internal_router().route_layer(
        axum::middleware::from_fn_with_state(state.clone(), auth::require_internal_token),
    );

    auth::router().merge(protected).merge(internal).with_state(state)
```

(replace the existing final `auth::router().merge(protected).with_state(state)` line).

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test -p roy-management --test telegram_bindings`
Expected: PASS. Also run `cargo test -p roy-management` to confirm no regressions.

- [ ] **Step 7: Commit**

```bash
git add crates/roy-management/src/channel_bindings.rs crates/roy-management/src/auth.rs \
        crates/roy-management/src/http.rs crates/roy-management/tests/telegram_bindings.rs
git commit -m "feat(roy-management): channel-bindings CRUD + internal telegram-sources endpoint"
```

- [ ] **Step 8: Slice-1 gate**

Run: `cargo fmt --all -- --check && cargo build -p roy-protocol -p roy-management --all-targets && cargo test -p roy-protocol -p roy-management --no-fail-fast`
Expected: all green.

---

# SLICE 2 — Telegram channel skeleton (roy-inbound)

## Task 2.1: Persona flows into Spawn

**Files:**
- Modify: `crates/roy-inbound/src/router.rs` (`FireSpec` + `ConfigRouter`)
- Modify: `crates/roy-inbound/src/session.rs` (`resolve` signature + existing tests)
- Modify: `crates/roy-inbound/src/dispatcher.rs` (call site)
- Test: `crates/roy-inbound/src/session.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write the failing test** — add to `session.rs` `#[cfg(test)] mod tests`:

```rust
    #[tokio::test]
    async fn spawn_uses_persona_override_when_present() {
        let store = test_store().await; // existing test helper; if absent, see note below
        let resolver = SessionResolver::new(store, "claude".into());
        let (target, _pending) = resolver
            .resolve(
                "tg:c1",
                "555",
                "support-l1",
                SessionStrategy::Ephemeral,
                Some("gemini"),
                Some("You are support."),
            )
            .await
            .unwrap();
        match target {
            roy_protocol::FireTarget::Spawn { harness, system_prompt } => {
                assert_eq!(harness, "gemini");
                assert_eq!(system_prompt.as_deref(), Some("You are support."));
            }
            other => panic!("expected Spawn, got {other:?}"),
        }
    }
```

> If `session.rs` tests use a different `BindingStore` fixture name, reuse it; the existing strategy tests already construct an in-memory `Arc<BindingStore>`. Match that helper. The point is: pass the two new `Some(...)` args.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p roy-inbound session::tests::spawn_uses_persona_override_when_present`
Expected: FAIL — `resolve` takes 4 args, not 6.

- [ ] **Step 3: Add fields to `FireSpec`** — in `router.rs`, extend the struct:

```rust
#[derive(Debug, Clone)]
pub struct FireSpec {
    pub agent_id: String,
    pub prompt: String,
    pub session_strategy: SessionStrategy,
    pub tags: BTreeMap<String, String>,
    pub fire_timeout_secs: u64,
    /// Per-source harness override (Telegram channel). `None` → resolver default.
    pub harness: Option<String>,
    /// Per-source system/persona prompt (Telegram channel). `None` → no prompt.
    pub system_prompt: Option<String>,
}
```
In `ConfigRouter::route`, set the two new fields to `None` in the returned `FireSpec { ... }`:
```rust
            harness: None,
            system_prompt: None,
```

- [ ] **Step 4: Change `resolve`** — in `session.rs`, update the signature and `spawn_target`:

```rust
    pub async fn resolve(
        &self,
        source_id: &str,
        sender_id: &str,
        agent_id: &str,
        strategy: SessionStrategy,
        harness: Option<&str>,
        system_prompt: Option<&str>,
    ) -> Result<(FireTarget, Option<PendingBinding>)> {
        let spawn_target = || FireTarget::Spawn {
            harness: harness
                .map(str::to_string)
                .unwrap_or_else(|| self.harness.clone()),
            system_prompt: system_prompt.map(str::to_string),
        };
        // ... rest unchanged ...
```
Update the **existing** strategy tests in this module: every `resolver.resolve(a, b, c, strat)` call gains `, None, None`.

- [ ] **Step 5: Update the dispatcher call site** — in `dispatcher.rs` `handle_one`, change the `resolve` call to pass the persona from the spec:

```rust
        let (target, pending) = self
            .resolver
            .resolve(
                &event.source_id,
                &event.sender_id,
                &spec.agent_id,
                spec.session_strategy,
                spec.harness.as_deref(),
                spec.system_prompt.as_deref(),
            )
            .await?;
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p roy-inbound session:: router:: dispatcher::`
Expected: PASS (new test + updated existing tests).

- [ ] **Step 7: Commit**

```bash
git add crates/roy-inbound/src/router.rs crates/roy-inbound/src/session.rs \
        crates/roy-inbound/src/dispatcher.rs
git commit -m "feat(roy-inbound): thread per-source persona into FireTarget::Spawn"
```

## Task 2.2: Dependencies + resolved source + management client

**Files:**
- Modify: `crates/roy-inbound/Cargo.toml`
- Create: `crates/roy-inbound/src/channels/telegram/mod.rs` (registry + resolved source + client; publisher added in 2.5)
- Modify: `crates/roy-inbound/src/channels/mod.rs`
- Test: in `telegram/mod.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Add dependencies** — in `crates/roy-inbound/Cargo.toml` `[dependencies]`, copy the `teloxide = ...` and `dptree = ...` lines **verbatim** from `crates/roy-gateway/Cargo.toml` (same version/features the workspace already vets), and add an HTTP client:

```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
```
(If `reqwest` already appears elsewhere in the workspace, match that version.)

- [ ] **Step 2: Write the failing test** — create `crates/roy-inbound/src/channels/telegram/mod.rs` with the non-teloxide pieces and a `From` test:

```rust
//! Telegram support channel. Bots + agents are configured in `roy-management`;
//! this channel fetches them via the internal HTTP endpoint, runs one teloxide
//! dispatcher per bot pushing `InboundEvent`s onto the bus, and replies through
//! `TelegramReplyHook`. Per-sender sticky sessions live in the shared `bindings`
//! table (see `session.rs`).

pub mod reply;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Result;
use roy_protocol::channel::{SessionStrategyWire, TelegramSource};

use crate::session::SessionStrategy;
use reply::TgSender;

/// Runtime view of one bound bot, derived from a `TelegramSource`.
#[derive(Debug, Clone)]
pub struct ResolvedSource {
    pub source_id: String,
    pub agent_slug: String,
    pub harness: String,
    pub system_prompt: Option<String>,
    pub session_strategy: SessionStrategy,
    pub allowed_user_ids: Arc<Vec<i64>>,
    pub fire_timeout_secs: u64,
}

const DEFAULT_FIRE_TIMEOUT_SECS: u64 = 600;

impl From<TelegramSource> for ResolvedSource {
    fn from(s: TelegramSource) -> Self {
        let session_strategy = match s.session_strategy {
            SessionStrategyWire::Ephemeral => SessionStrategy::Ephemeral,
            SessionStrategyWire::PersistentOne => SessionStrategy::PersistentOne,
            SessionStrategyWire::PerSenderSticky { idle_timeout_secs } => {
                SessionStrategy::PerSenderSticky {
                    idle_timeout: Duration::from_secs(idle_timeout_secs),
                }
            }
        };
        ResolvedSource {
            source_id: s.source_id,
            agent_slug: s.agent_slug,
            harness: s.harness,
            system_prompt: s.system_prompt,
            session_strategy,
            allowed_user_ids: Arc::new(s.allowed_user_ids),
            fire_timeout_secs: DEFAULT_FIRE_TIMEOUT_SECS,
        }
    }
}

/// Shared registry of live Telegram sources. Synchronous lock so the reply-hook
/// factory (a sync closure) and the async router can both read it without await.
#[derive(Default)]
pub struct TelegramRegistry {
    inner: RwLock<HashMap<String, SourceRuntime>>,
}

pub(crate) struct SourceRuntime {
    pub resolved: Arc<ResolvedSource>,
    pub sender: Arc<dyn TgSender>,
    pub token: String,
    pub task: tokio::task::JoinHandle<()>,
}

impl TelegramRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn resolved_for(&self, source_id: &str) -> Option<Arc<ResolvedSource>> {
        self.inner
            .read()
            .unwrap()
            .get(source_id)
            .map(|r| r.resolved.clone())
    }

    pub fn sender_for(&self, source_id: &str) -> Option<Arc<dyn TgSender>> {
        self.inner
            .read()
            .unwrap()
            .get(source_id)
            .map(|r| r.sender.clone())
    }

    pub(crate) fn insert(&self, source_id: String, runtime: SourceRuntime) {
        if let Some(old) = self.inner.write().unwrap().insert(source_id, runtime) {
            old.task.abort();
        }
    }

    pub(crate) fn remove(&self, source_id: &str) {
        if let Some(old) = self.inner.write().unwrap().remove(source_id) {
            old.task.abort();
        }
    }

    pub fn source_ids(&self) -> Vec<String> {
        self.inner.read().unwrap().keys().cloned().collect()
    }
}

/// Thin HTTP client for `roy-management`'s internal source endpoint.
pub struct ManagementClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl ManagementClient {
    pub fn new(base_url: String, token: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
            http: reqwest::Client::new(),
        }
    }

    pub async fn fetch_telegram_sources(&self) -> Result<Vec<TelegramSource>> {
        let url = format!("{}/internal/telegram-sources", self.base_url);
        let resp = self.http.get(&url).bearer_auth(&self.token).send().await?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("management returned {status} for telegram-sources");
        }
        Ok(resp.json::<Vec<TelegramSource>>().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_source_maps_sticky_strategy() {
        let src = TelegramSource {
            source_id: "tg:c1".into(),
            bot_token: "t".into(),
            agent_slug: "a".into(),
            harness: "claude".into(),
            system_prompt: Some("p".into()),
            model: None,
            session_strategy: SessionStrategyWire::PerSenderSticky {
                idle_timeout_secs: 60,
            },
            allowed_user_ids: vec![7],
        };
        let r: ResolvedSource = src.into();
        assert_eq!(r.harness, "claude");
        assert_eq!(r.system_prompt.as_deref(), Some("p"));
        assert!(matches!(
            r.session_strategy,
            SessionStrategy::PerSenderSticky { idle_timeout } if idle_timeout == Duration::from_secs(60)
        ));
        assert_eq!(*r.allowed_user_ids, vec![7]);
    }
}
```

- [ ] **Step 3: Declare the module** — in `crates/roy-inbound/src/channels/mod.rs`, add `pub mod telegram;` after `pub mod webhook;`.

- [ ] **Step 4: Run test to verify it passes** (after Task 2.4 creates `reply.rs` this compiles; to keep this task self-contained, do Step 2 of Task 2.4 — the `TgSender` trait — now, or temporarily stub `reply.rs` with just the trait). Simplest: create `reply.rs` with the `TgSender` trait first (it has no deps), then return here.

Run: `cargo test -p roy-inbound telegram::tests::resolved_source_maps_sticky_strategy`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-inbound/Cargo.toml crates/roy-inbound/src/channels/mod.rs \
        crates/roy-inbound/src/channels/telegram/mod.rs
git commit -m "feat(roy-inbound): telegram registry, resolved source, management client"
```

## Task 2.3: `CompositeRouter`

**Files:**
- Modify: `crates/roy-inbound/src/router.rs`
- Test: `crates/roy-inbound/src/router.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write the failing test** — add to `router.rs` `#[cfg(test)] mod tests` (create the module if absent):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::telegram::{ResolvedSource, TelegramRegistry};
    use crate::session::SessionStrategy;
    use std::sync::Arc;

    fn ev(source_kind: &str, source_id: &str, text: &str) -> InboundEvent {
        InboundEvent {
            id: uuid::Uuid::new_v4(),
            source_id: source_id.into(),
            source_kind: source_kind.into(),
            sender_id: "555".into(),
            payload: serde_json::json!({ "text": text }),
            received_at: chrono::Utc::now(),
            reply: crate::bus::ReplyHandle::Noop,
        }
    }

    #[tokio::test]
    async fn telegram_router_builds_spec_with_persona() {
        let reg = TelegramRegistry::new();
        let tg = TelegramRouter::new(reg.clone());
        // No source registered → None.
        assert!(tg.route(&ev("telegram", "tg:c1", "hi")).await.is_none());

        // Register a resolved source directly (bypassing teloxide).
        reg_insert_for_test(&reg, "tg:c1", "claude", Some("persona"));
        let spec = tg.route(&ev("telegram", "tg:c1", "hi")).await.unwrap();
        assert_eq!(spec.prompt, "hi");
        assert_eq!(spec.harness.as_deref(), Some("claude"));
        assert_eq!(spec.system_prompt.as_deref(), Some("persona"));
        assert_eq!(spec.agent_id, "agent-x");
    }

    // Helper: insert a resolved-only source (no live bot) for router tests.
    fn reg_insert_for_test(
        reg: &Arc<TelegramRegistry>,
        source_id: &str,
        harness: &str,
        prompt: Option<&str>,
    ) {
        use crate::channels::telegram::test_support::insert_resolved;
        insert_resolved(
            reg,
            ResolvedSource {
                source_id: source_id.into(),
                agent_slug: "agent-x".into(),
                harness: harness.into(),
                system_prompt: prompt.map(str::to_string),
                session_strategy: SessionStrategy::Ephemeral,
                allowed_user_ids: Arc::new(vec![]),
                fire_timeout_secs: 600,
            },
        );
    }
}
```

> This test needs a test-only insert that doesn't spawn a teloxide task. Add it in Task 2.2's module as a `pub(crate)` helper (Step 3 below).

- [ ] **Step 2: Implement `TelegramRouter` + `CompositeRouter`** — add to `router.rs`:

```rust
use crate::channels::telegram::TelegramRegistry;
use std::sync::Arc;

/// Router for Telegram sources resolved from `roy-management`.
pub struct TelegramRouter {
    registry: Arc<TelegramRegistry>,
}

impl TelegramRouter {
    pub fn new(registry: Arc<TelegramRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Router for TelegramRouter {
    async fn route(&self, ev: &InboundEvent) -> Option<FireSpec> {
        let src = self.registry.resolved_for(&ev.source_id)?;
        let prompt = ev
            .payload
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let mut tags = BTreeMap::new();
        tags.insert(format!("{TAG_PREFIX}:source_id"), ev.source_id.clone());
        tags.insert(format!("{TAG_PREFIX}:source_kind"), ev.source_kind.clone());
        tags.insert(format!("{TAG_PREFIX}:event_id"), ev.id.to_string());
        tags.insert(format!("{TAG_PREFIX}:sender_id"), ev.sender_id.clone());
        Some(FireSpec {
            agent_id: src.agent_slug.clone(),
            prompt,
            session_strategy: src.session_strategy,
            tags,
            fire_timeout_secs: src.fire_timeout_secs,
            harness: Some(src.harness.clone()),
            system_prompt: src.system_prompt.clone(),
        })
    }
}

/// Routes `telegram` events through `TelegramRouter`, everything else through
/// `ConfigRouter` (webhook).
pub struct CompositeRouter {
    pub telegram: TelegramRouter,
    pub config: ConfigRouter,
}

#[async_trait]
impl Router for CompositeRouter {
    async fn route(&self, ev: &InboundEvent) -> Option<FireSpec> {
        if ev.source_kind == "telegram" {
            self.telegram.route(ev).await
        } else {
            self.config.route(ev).await
        }
    }
}
```

- [ ] **Step 3: Add the test-only insert helper** — in `crates/roy-inbound/src/channels/telegram/mod.rs`, add:

```rust
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use reply::NoopSender;

    /// Insert a resolved source with no live bot/task — for router/unit tests.
    pub fn insert_resolved(reg: &Arc<TelegramRegistry>, resolved: ResolvedSource) {
        let source_id = resolved.source_id.clone();
        reg.inner.write().unwrap().insert(
            source_id,
            SourceRuntime {
                resolved: Arc::new(resolved),
                sender: Arc::new(NoopSender),
                token: String::new(),
                task: tokio::spawn(async {}),
            },
        );
    }
}
```

- [ ] **Step 4: Run test to verify it passes** (after Task 2.4 provides `NoopSender`; do Task 2.4 first if needed).

Run: `cargo test -p roy-inbound router::tests::telegram_router_builds_spec_with_persona`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-inbound/src/router.rs crates/roy-inbound/src/channels/telegram/mod.rs
git commit -m "feat(roy-inbound): CompositeRouter routing telegram via the registry"
```

## Task 2.4: `TelegramReplyHook` + `TgSender`

**Files:**
- Create: `crates/roy-inbound/src/channels/telegram/reply.rs`
- Test: `crates/roy-inbound/src/channels/telegram/reply.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write the failing test** — create `reply.rs`:

```rust
//! Outbound replies for the Telegram channel. The publisher pushes events with
//! `ReplyHandle::Noop`; the reply goes out-of-band through the bot via `TgSender`.

use anyhow::Result;
use async_trait::async_trait;

use crate::bus::ReplyHandle;
use crate::reply::{FireOutcome, ReplyHook};
use roy_protocol::TurnEvent;

/// Minimal send abstraction so the hook is unit-testable without a live bot.
#[async_trait]
pub trait TgSender: Send + Sync {
    async fn send(&self, chat_id: i64, text: &str) -> Result<()>;
}

/// Used when a bot was removed between event ingress and reply (rare race).
pub struct NoopSender;

#[async_trait]
impl TgSender for NoopSender {
    async fn send(&self, _chat_id: i64, _text: &str) -> Result<()> {
        tracing::warn!("telegram reply dropped: no live bot for source");
        Ok(())
    }
}

pub struct TelegramReplyHook {
    sender: std::sync::Arc<dyn TgSender>,
    chat_id: i64,
}

impl TelegramReplyHook {
    pub fn new(sender: std::sync::Arc<dyn TgSender>, chat_id: i64) -> Self {
        Self { sender, chat_id }
    }
}

#[async_trait]
impl ReplyHook for TelegramReplyHook {
    async fn on_turn_event(&mut self, _ev: &TurnEvent) -> Result<()> {
        Ok(()) // streaming edits are slice 4
    }

    async fn on_finish(self: Box<Self>, outcome: FireOutcome, _reply: ReplyHandle) -> Result<()> {
        let text = match outcome {
            FireOutcome::Ok { assistant_text, .. } => {
                if assistant_text.trim().is_empty() {
                    "(пустой ответ)".to_string()
                } else {
                    assistant_text
                }
            }
            FireOutcome::Timeout { .. } => "⚠ Превышено время ожидания, попробуйте ещё раз.".into(),
            FireOutcome::DaemonError { .. } => "⚠ Внутренняя ошибка, попробуйте позже.".into(),
            FireOutcome::Cancelled => "⚠ Запрос отменён.".into(),
            FireOutcome::RouteRejected => "⚠ Этот бот пока не настроен.".into(),
        };
        self.sender.send(self.chat_id, &text).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct MockSender {
        sent: Mutex<Vec<(i64, String)>>,
    }
    #[async_trait]
    impl TgSender for MockSender {
        async fn send(&self, chat_id: i64, text: &str) -> Result<()> {
            self.sent.lock().unwrap().push((chat_id, text.to_string()));
            Ok(())
        }
    }

    #[tokio::test]
    async fn ok_outcome_sends_assistant_text() {
        let sender = Arc::new(MockSender::default());
        let hook = Box::new(TelegramReplyHook::new(sender.clone(), 555));
        hook.on_finish(
            FireOutcome::Ok {
                assistant_text: "hello".into(),
                cost_usd: None,
                stop_reason: "end_turn".into(),
            },
            ReplyHandle::Noop,
        )
        .await
        .unwrap();
        let sent = sender.sent.lock().unwrap();
        assert_eq!(sent.as_slice(), &[(555, "hello".to_string())]);
    }

    #[tokio::test]
    async fn error_outcome_sends_friendly_message() {
        let sender = Arc::new(MockSender::default());
        let hook = Box::new(TelegramReplyHook::new(sender.clone(), 7));
        hook.on_finish(FireOutcome::RouteRejected, ReplyHandle::Noop)
            .await
            .unwrap();
        assert!(sender.sent.lock().unwrap()[0].1.contains("не настроен"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails then passes**

Run: `cargo test -p roy-inbound telegram::reply::tests`
Expected: first FAIL (file absent) → after creating it, PASS (2 tests).

- [ ] **Step 3: Commit**

```bash
git add crates/roy-inbound/src/channels/telegram/reply.rs
git commit -m "feat(roy-inbound): TelegramReplyHook + TgSender"
```

## Task 2.5: Publisher (teloxide) + cli wiring

**Files:**
- Modify: `crates/roy-inbound/src/channels/telegram/mod.rs` (publisher + event builder + bot sender)
- Modify: `crates/roy-inbound/src/cli.rs` (Args + wiring)
- Test: `crates/roy-inbound/src/channels/telegram/mod.rs` (pure `build_event` + `allowed` tests)

- [ ] **Step 1: Write the failing test** — add to `telegram/mod.rs` `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn build_event_shapes_payload() {
        let ev = build_event("tg:c1", 555, 999, "hi there");
        assert_eq!(ev.source_kind, "telegram");
        assert_eq!(ev.source_id, "tg:c1");
        assert_eq!(ev.sender_id, "555");
        assert_eq!(ev.payload["text"], "hi there");
        assert_eq!(ev.payload["user_id"], 999);
        assert!(matches!(ev.reply, crate::bus::ReplyHandle::Noop));
    }

    #[test]
    fn allowlist_logic() {
        assert!(allowed(&[], 5)); // empty = public
        assert!(allowed(&[5, 6], 5));
        assert!(!allowed(&[5, 6], 7));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p roy-inbound telegram::tests::build_event_shapes_payload`
Expected: FAIL — `build_event` / `allowed` not defined.

- [ ] **Step 3: Implement publisher + helpers** — add to `telegram/mod.rs`:

```rust
use crate::bus::{BusSender, InboundEvent, ReplyHandle};
use crate::channels::Publisher;
use async_trait::async_trait;
use chrono::Utc;
use reply::TgSender as _;
use serde_json::json;
use teloxide::prelude::*;
use teloxide::types::Message;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Pure mapping from a Telegram message to an `InboundEvent` (unit-tested).
pub(crate) fn build_event(source_id: &str, chat_id: i64, user_id: i64, text: &str) -> InboundEvent {
    InboundEvent {
        id: Uuid::new_v4(),
        source_id: source_id.to_string(),
        source_kind: "telegram".into(),
        sender_id: chat_id.to_string(),
        payload: json!({ "text": text, "user_id": user_id }),
        received_at: Utc::now(),
        reply: ReplyHandle::Noop,
    }
}

/// Allowlist check: empty list = public.
pub(crate) fn allowed(allowed_ids: &[i64], user_id: i64) -> bool {
    allowed_ids.is_empty() || allowed_ids.contains(&user_id)
}

/// teloxide `Bot` wrapped as a `TgSender` for replies.
pub(crate) struct BotSender(pub teloxide::Bot);

#[async_trait]
impl reply::TgSender for BotSender {
    async fn send(&self, chat_id: i64, text: &str) -> anyhow::Result<()> {
        self.0
            .send_message(teloxide::types::ChatId(chat_id), text)
            .await?;
        Ok(())
    }
}

#[derive(Clone)]
struct TgDeps {
    source_id: Arc<str>,
    allowed: Arc<Vec<i64>>,
    bus: BusSender,
}

async fn on_message(msg: &Message, deps: &TgDeps) -> anyhow::Result<()> {
    let Some(text) = msg.text() else {
        return Ok(());
    };
    let Some(from) = msg.from.as_ref() else {
        return Ok(());
    };
    let user_id = from.id.0 as i64;
    if !allowed(&deps.allowed, user_id) {
        return Ok(());
    }
    let chat_id = msg.chat.id.0;
    let ev = build_event(&deps.source_id, chat_id, user_id, text);
    deps.bus
        .send(ev)
        .await
        .map_err(|_| anyhow::anyhow!("bus closed"))?;
    Ok(())
}

fn spawn_bot_task(
    bot: teloxide::Bot,
    source_id: Arc<str>,
    allowed_ids: Arc<Vec<i64>>,
    bus: BusSender,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let deps = TgDeps {
            source_id,
            allowed: allowed_ids,
            bus,
        };
        let handler = Update::filter_message().endpoint(
            |_bot: teloxide::Bot, msg: Message, deps: TgDeps| async move {
                if let Err(e) = on_message(&msg, &deps).await {
                    tracing::warn!(?e, "telegram on_message failed");
                }
                respond(())
            },
        );
        Dispatcher::builder(bot, handler)
            .dependencies(dptree::deps![deps])
            .build()
            .dispatch()
            .await;
    })
}

/// Publisher for the Telegram channel: fetches sources from `roy-management`,
/// runs one teloxide dispatcher per bot, and keeps the shared registry current.
pub struct TelegramPublisher {
    registry: Arc<TelegramRegistry>,
    client: Arc<ManagementClient>,
}

impl TelegramPublisher {
    pub fn new(registry: Arc<TelegramRegistry>, client: Arc<ManagementClient>) -> Self {
        Self { registry, client }
    }

    /// Build a bot, spawn its dispatcher, and insert it into the registry.
    fn start_source(&self, src: TelegramSource, bus: &BusSender) {
        let token = src.bot_token.clone();
        let resolved: ResolvedSource = src.into();
        let source_id: Arc<str> = Arc::from(resolved.source_id.as_str());
        let bot = teloxide::Bot::new(&token);
        let sender: Arc<dyn TgSender> = Arc::new(BotSender(bot.clone()));
        let task = spawn_bot_task(
            bot,
            source_id.clone(),
            resolved.allowed_user_ids.clone(),
            bus.clone(),
        );
        self.registry.insert(
            resolved.source_id.clone(),
            SourceRuntime {
                resolved: Arc::new(resolved),
                sender,
                token,
                task,
            },
        );
    }
}

#[async_trait]
impl Publisher for TelegramPublisher {
    async fn run(self: Arc<Self>, bus: BusSender, cancel: CancellationToken) -> Result<()> {
        // Initial load (slice 2: fetch once; slice 3 adds the poll loop).
        match self.client.fetch_telegram_sources().await {
            Ok(sources) => {
                tracing::info!(count = sources.len(), "telegram: starting bots");
                for src in sources {
                    self.start_source(src, &bus);
                }
            }
            Err(e) => tracing::error!(error = ?e, "telegram: initial source fetch failed"),
        }
        cancel.cancelled().await;
        for id in self.registry.source_ids() {
            self.registry.remove(&id);
        }
        Ok(())
    }
}
```

- [ ] **Step 4: Run the pure tests to verify they pass**

Run: `cargo test -p roy-inbound telegram::tests`
Expected: PASS (`build_event_shapes_payload`, `allowlist_logic`, `resolved_source_maps_sticky_strategy`).

- [ ] **Step 5: Wire into `cli.rs`** — add Args fields and the channel wiring.

Add to `Args`:
```rust
    /// roy-management base URL for resolving telegram bot→agent bindings.
    #[arg(long, env = "ROY_MANAGEMENT_URL", default_value = "http://127.0.0.1:8088")]
    pub management_url: String,
```
(use the management default port; if the workspace default differs, match it).

In `run()`, after the webhook publisher is built and before constructing the router, add the Telegram channel (gated on the internal token being present):

```rust
    use crate::channels::telegram::{ManagementClient, TelegramPublisher, TelegramRegistry};
    use crate::channels::telegram::reply::TelegramReplyHook;

    let tg_registry = TelegramRegistry::new();

    // Reply hook factory for telegram (looks up the live bot in the registry).
    {
        let reg = tg_registry.clone();
        hooks_mut.register(
            "telegram",
            Box::new(move |ev: &EventRef| -> Box<dyn ReplyHook> {
                let chat_id = ev.sender_id.parse::<i64>().unwrap_or(0);
                match reg.sender_for(&ev.source_id) {
                    Some(sender) => Box::new(TelegramReplyHook::new(sender, chat_id)),
                    None => Box::new(crate::channels::telegram::reply::TelegramReplyHook::new(
                        std::sync::Arc::new(crate::channels::telegram::reply::NoopSender),
                        chat_id,
                    )),
                }
            }),
        );
    }
```

> NOTE on wiring order: `hooks` is built and immediately wrapped in `Arc` in the current `run()`. Refactor so the registry is mutated before the `Arc`: rename the local to `hooks` (mutable `ReplyHookRegistry`), register `webhook` AND `telegram`, then `let hooks = Arc::new(hooks);`. In the snippet above `hooks_mut` denotes that still-mutable binding.

Build the telegram publisher and the composite router:
```rust
    let mgmt_token = std::env::var("ROY_INTERNAL_TOKEN").ok();
    let tg_publisher = mgmt_token.as_ref().map(|tok| {
        Arc::new(TelegramPublisher::new(
            tg_registry.clone(),
            Arc::new(ManagementClient::new(args.management_url.clone(), tok.clone())),
        ))
    });
```

Replace the router construction:
```rust
    let router: Arc<dyn crate::router::Router> = Arc::new(crate::router::CompositeRouter {
        telegram: crate::router::TelegramRouter::new(tg_registry.clone()),
        config: ConfigRouter::from_config(&cfg),
    });
```

After the webhook publisher task is spawned, spawn the telegram publisher task (if configured):
```rust
    let tg_handle = tg_publisher.map(|pubr| {
        let bus_tx = bus_tx.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            if let Err(e) = pubr.run(bus_tx, cancel).await {
                tracing::error!(error = ?e, "telegram publisher exited with error");
            }
        })
    });
```
(`bus_tx` is moved into the webhook task in the current code — clone it before that move, or reorder so both publishers get a clone. Ensure `bus_tx` is cloned for each consumer.)

At shutdown, join the telegram handle too:
```rust
    if let Some(h) = tg_handle {
        let _ = h.await;
    }
```

- [ ] **Step 6: Build the crate (the teloxide runtime path is verified by manual smoke, not unit tests)**

Run: `cargo build -p roy-inbound --all-targets`
Expected: compiles.

- [ ] **Step 7: Manual smoke (requires a real bot token + running daemon + running management)**

```bash
# 1. management with an internal token
ROY_JWT_SECRET=<32+ bytes> ROY_INTERNAL_TOKEN=<32+ bytes> roy management &
# 2. via web UI (or curl with a login cookie): create a telegram_bot connection
#    (secrets.bot_token = <BotFather token>), write an agent .md, POST /channel-bindings
# 3. inbound
ROY_INTERNAL_TOKEN=<same token> roy inbound --config <inbound.toml> --management-url http://127.0.0.1:8088 &
# 4. DM the bot from two different Telegram accounts; each should get its own
#    sticky session; replies come back as messages.
```
Expected: each sender gets an isolated, continuing conversation with the bound agent.

- [ ] **Step 8: Commit**

```bash
git add crates/roy-inbound/src/channels/telegram/mod.rs crates/roy-inbound/src/cli.rs
git commit -m "feat(roy-inbound): Telegram publisher + cli wiring (fetch-once)"
```

- [ ] **Step 9: Slice-2 gate**

Run: `cargo fmt --all -- --check && cargo build --workspace --all-targets && cargo test --workspace --no-fail-fast`
Expected: all green.

---

# SLICE 3 — Reconciliation & resilience

## Task 3.1: Poll loop + `diff_sources` + reconcile + retry

**Files:**
- Modify: `crates/roy-inbound/src/channels/telegram/mod.rs`
- Test: `crates/roy-inbound/src/channels/telegram/mod.rs` (`#[cfg(test)]` for `diff_sources`)

- [ ] **Step 1: Write the failing test** — add to `telegram/mod.rs` tests:

```rust
    #[test]
    fn diff_sources_partitions_add_remove_keep() {
        let current = vec!["tg:a".to_string(), "tg:b".to_string()];
        let next = vec!["tg:b".to_string(), "tg:c".to_string()];
        let d = diff_sources(&current, &next);
        assert_eq!(d.to_add, vec!["tg:c".to_string()]);
        assert_eq!(d.to_remove, vec!["tg:a".to_string()]);
        assert_eq!(d.to_keep, vec!["tg:b".to_string()]);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p roy-inbound telegram::tests::diff_sources_partitions_add_remove_keep`
Expected: FAIL — `diff_sources` not defined.

- [ ] **Step 3: Implement diff + reconcile + poll** — add to `telegram/mod.rs`:

```rust
#[derive(Debug, Default, PartialEq)]
pub(crate) struct SourceDiff {
    pub to_add: Vec<String>,
    pub to_remove: Vec<String>,
    pub to_keep: Vec<String>,
}

/// Partition next-source-ids against the currently-live ones.
pub(crate) fn diff_sources(current: &[String], next: &[String]) -> SourceDiff {
    let cur: std::collections::HashSet<&String> = current.iter().collect();
    let nxt: std::collections::HashSet<&String> = next.iter().collect();
    SourceDiff {
        to_add: next.iter().filter(|id| !cur.contains(*id)).cloned().collect(),
        to_remove: current.iter().filter(|id| !nxt.contains(*id)).cloned().collect(),
        to_keep: next.iter().filter(|id| cur.contains(*id)).cloned().collect(),
    }
}
```

Add a `token_for` accessor on the registry (to detect token changes for kept sources) in the `impl TelegramRegistry`:
```rust
    pub(crate) fn token_for(&self, source_id: &str) -> Option<String> {
        self.inner.read().unwrap().get(source_id).map(|r| r.token.clone())
    }
    /// Replace the resolved view of a live source without restarting its bot.
    pub(crate) fn update_resolved(&self, source_id: &str, resolved: Arc<ResolvedSource>) {
        if let Some(r) = self.inner.write().unwrap().get_mut(source_id) {
            r.resolved = resolved;
        }
    }
```

Add the reconcile method on `TelegramPublisher`:
```rust
impl TelegramPublisher {
    fn reconcile(&self, sources: Vec<TelegramSource>, bus: &BusSender) {
        let by_id: std::collections::HashMap<String, TelegramSource> =
            sources.into_iter().map(|s| (s.source_id.clone(), s)).collect();
        let next_ids: Vec<String> = by_id.keys().cloned().collect();
        let diff = diff_sources(&self.registry.source_ids(), &next_ids);

        for id in diff.to_remove {
            tracing::info!(source = id, "telegram: stopping bot");
            self.registry.remove(&id);
        }
        for id in diff.to_add {
            if let Some(src) = by_id.get(&id) {
                tracing::info!(source = id, "telegram: starting bot");
                self.start_source(src.clone(), bus);
            }
        }
        for id in diff.to_keep {
            let Some(src) = by_id.get(&id) else { continue };
            if self.registry.token_for(&id).as_deref() != Some(src.bot_token.as_str()) {
                tracing::info!(source = id, "telegram: token changed; restarting bot");
                self.registry.remove(&id);
                self.start_source(src.clone(), bus);
            } else {
                self.registry
                    .update_resolved(&id, Arc::new(src.clone().into()));
            }
        }
    }
}
```

Replace the body of `Publisher::run` with a poll loop:
```rust
#[async_trait]
impl Publisher for TelegramPublisher {
    async fn run(self: Arc<Self>, bus: BusSender, cancel: CancellationToken) -> Result<()> {
        const POLL: Duration = Duration::from_secs(30);
        loop {
            match self.client.fetch_telegram_sources().await {
                Ok(sources) => self.reconcile(sources, &bus),
                Err(e) => tracing::warn!(error = ?e, "telegram: source refresh failed; keeping current bots"),
            }
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(POLL) => {}
            }
        }
        for id in self.registry.source_ids() {
            self.registry.remove(&id);
        }
        Ok(())
    }
}
```
Delete the old fetch-once `run` body (replaced above). `start_source` is reused.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p roy-inbound telegram::`
Expected: PASS (diff + all prior telegram tests).

- [ ] **Step 5: Manual smoke for reconcile** — with the stack from Task 2.5 running: add a second binding in the web UI → within ~30s a second bot starts; delete a binding → its bot stops; stop `roy management` → existing chats keep working, logs show "refresh failed; keeping current bots".

- [ ] **Step 6: Commit**

```bash
git add crates/roy-inbound/src/channels/telegram/mod.rs
git commit -m "feat(roy-inbound): telegram source reconciliation + resilience"
```

- [ ] **Step 7: Slice-3 gate (full CI gate locally)**

Run: `cargo fmt --all -- --check && cargo build --workspace --all-targets && cargo test --workspace --no-fail-fast`
Expected: all green.

---

## Documentation touch-ups (final task)

- [ ] Update `CLAUDE.md` `roy-inbound` bullet: webhook **and Telegram** channels; Telegram sources come from `roy-management`'s `channel_bindings` via the internal endpoint.
- [ ] Update `docs/architecture.md`: note the `roy-inbound → roy-management` control-plane HTTP edge (config only; daemon still socket-only).
- [ ] Update `docs/persistence.md`: add the `channel_bindings` table to the `agents.db` section.
- [ ] Add a short `## Telegram support` section to `crates/roy-inbound/README.md` (create a telegram_bot connection → bind to an agent → run `roy inbound` with `ROY_INTERNAL_TOKEN`).
- [ ] Commit: `docs: document telegram support channel`.

---

## Self-Review (completed during planning)

**Spec coverage:**
- Two-plane state model → Tasks 1.4 (config) + reuse of existing runtime bindings (Task 2.1 threads persona). ✓
- `channel_bindings` table + web-UI CRUD → Tasks 1.4, 1.6. ✓
- Internal endpoint resolving token + persona → Task 1.6 (`resolve_telegram_sources`). ✓
- A1 boundary (read-only control-plane HTTP, DTOs in roy-protocol) → Tasks 1.1, 2.2. ✓
- Per-sender sticky, persona into Spawn → Task 2.1. ✓
- Telegram channel (publisher + reply hook) → Tasks 2.4, 2.5. ✓
- Public-by-default access (`allowed_user_ids` empty) → Task 2.5 (`allowed`). ✓
- `sender_id = chat_id` → Task 2.5 (`build_event`). ✓
- Final-answer reply first, streaming deferred → Task 2.4 (`on_turn_event` no-op). ✓
- Reconciliation + resilience → Task 3.1. ✓
- Auth gate on internal endpoint (loopback + bearer) → Task 1.6 (`require_internal_token`). ✓

**Known scope limitations (carried from spec / surfaced during planning):**
- Agent resolution uses the binding's `agent_scope` dir only; **builtin agents are not bindable** in v1 (avoids needing management's builtin-dir path). Acceptable — support personas are user/team agents.
- `PUT /channel-bindings/{id}` not implemented (create + delete covers the lifecycle); add later if editing in place is wanted.
- `telegram_bot` `validate_config` accepts any object; token presence is enforced at binding-create time and at the internal endpoint, not in `validate_config`.

**Type consistency:** `TelegramSource`/`SessionStrategyWire` identical across roy-protocol (1.1), management resolver (1.6), inbound `From` (2.2). `FireSpec.harness/system_prompt` added in 2.1 and consumed in 2.3/dispatcher. `TgSender` defined in 2.4, used by 2.2 (`SourceRuntime.sender`), 2.3 (`NoopSender`), 2.5 (`BotSender`). `source_id` format `"tg:<connection_id>"` consistent (1.6 producer, 2.x consumers).
