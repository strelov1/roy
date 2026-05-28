# Agent Connections — Telegram v1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable an agent to expose itself to end-users via attached Telegram bots. Each Telegram user gets an isolated session with the agent's persona. The data model and resolver are designed so a web-widget channel can be added later without schema changes.

**Architecture:** A new `connections` table in the shared `agents.db` describes one outbound channel per row (kind=`telegram` for v1, `web_widget`/`web_link` later). `roy-management` exposes HTTP CRUD over connections; `roy-web` adds a per-agent "Connections" panel. `roy-gateway` is rewritten from a single hard-coded `[telegram]` bot to N concurrent bot tasks sourced from the DB at startup. `SessionBinder` key generalizes from `chat_id → session_id` to `(connection_id, external_id) → session_id`. The orchestrator's existing `system_prompt` plumbing (already in `ClientCommand::Spawn`) gets a per-connection value resolved from `connection.agent_id → agents.prompt`.

**Tech Stack:** Rust (Tokio, sqlx-SQLite, teloxide, axum); Svelte 5 + Vite + Tailwind v4 + bits-ui.

**Out of scope (deferred):**
- Web widget / public-link channels (foundation is laid; separate plan).
- Hot-reload of connections (gateway restart required to pick up a newly attached bot — documented in connection UI).
- Bot-token encryption at rest (TODO comment + follow-up plan; v1 stores plaintext in mode-0600 SQLite, same trust model as agent prompts).
- Per-guest rate-limiting beyond the existing `allowed_user_ids` allowlist (deferred to abuse-control plan).
- Group-chat semantics in Telegram (binder treats `chat_id` as the binding key; group behavior is "undefined" in v1).

---

## File Map

**roy-agents (Rust crate)**
- Create: `crates/roy-agents/migrations/sqlite/0013_connections.sql`
- Create: `crates/roy-agents/src/connections.rs`
- Modify: `crates/roy-agents/src/lib.rs` (re-export `connections`)

**roy-management (Rust crate)**
- Create: `crates/roy-management/migrations/sqlite/0014_session_connection.sql`
- Modify: `crates/roy-management/src/cwd.rs` (add `CwdScope::AgentChannel`)
- Modify: `crates/roy-management/src/meta_store.rs` (`SessionMeta.connection_id` round-trip; session create path sets it)
- Modify: `crates/roy-management/src/http.rs` (mount connection routes + handlers)
- Modify: `crates/roy-management/src/lib.rs` (no-op if routes are mounted from `http.rs`)
- Create: `crates/roy-management/src/connections.rs` (axum handlers; ACL checks `agent.created_by == caller`)

**roy-gateway (Rust crate)**
- Modify: `crates/roy-gateway/Cargo.toml` (add `roy-agents` dep)
- Modify: `crates/roy-gateway/src/binder.rs` (key becomes `(String, i64)`)
- Modify: `crates/roy-gateway/src/orchestrator.rs` (`OrchestratorConfig` gains `connection_id`, `system_prompt`; pass through Spawn)
- Modify: `crates/roy-gateway/src/daemon.rs` (`Conn::spawn` signature: `(&mut self, preset: &str, cwd: Option<PathBuf>, system_prompt: Option<String>)`)
- Modify: `crates/roy-gateway/src/lib.rs` (`build_telegram_task` → `build_telegram_tasks`)
- Modify: `crates/roy-gateway/src/config.rs` (drop `[telegram]`/`[binder]` TOML blocks; gateway now requires `[agents_db]` and `[binder].path` only)
- Modify: `crates/roy-gateway/src/telegram.rs` (no structural change — `BotDeps` already carries everything per-bot)

**roy-web (Svelte SPA in ../roy-web)**
- Modify: `../roy-web/src/lib/management-client.ts` (add `connections` API namespace)
- Create: `../roy-web/src/lib/AgentConnectionsPanel.svelte` (connections list + attach dialog for a given agent)
- Modify: `../roy-web/src/lib/AgentsView.svelte` (mount the panel inside the agent detail view)
- Modify: `../roy-web/src/lib/SessionList.svelte` (show connection badge on rows where `connection_id` is set)

**Docs**
- Modify: `crates/roy/CLAUDE.md` (one new bullet describing `connections` table + gateway sourcing change)

---

## Phase A: Backend Foundation

### Task 1: Migration — `connections` table

**Files:**
- Create: `crates/roy-agents/migrations/sqlite/0013_connections.sql`

- [ ] **Step 1: Write the migration**

```sql
-- 0013_connections.sql
--
-- One row per attached external channel for an agent. v1 supports
-- kind='telegram' (credentials_json = {"bot_token": "..."}). Web channels
-- (kind='web_widget' / 'web_link') will reuse this schema verbatim.
--
-- credentials_json is stored as TEXT; v1 trust model: SQLite is mode 0600
-- and lives in the user's home, same as agent prompts.

CREATE TABLE connections (
    id              TEXT PRIMARY KEY,
    agent_id        TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    kind            TEXT NOT NULL CHECK (kind IN ('telegram')),
    label           TEXT,
    credentials_json TEXT NOT NULL,
    allowed_external_ids TEXT NOT NULL DEFAULT '[]',  -- JSON array of strings/numbers; empty = open
    created_by      TEXT NOT NULL REFERENCES users(id),
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);

CREATE INDEX connections_agent ON connections(agent_id);
CREATE INDEX connections_kind ON connections(kind);
```

- [ ] **Step 2: Verify migration applies**

Run: `cargo test -p roy-agents db::tests::open_creates_db_and_applies_migration -- --nocapture`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/roy-agents/migrations/sqlite/0013_connections.sql
git commit -m "feat(roy-agents): connections table for external channel bindings"
```

---

### Task 2: `Connection` type + store CRUD

**Files:**
- Create: `crates/roy-agents/src/connections.rs`
- Modify: `crates/roy-agents/src/lib.rs`

- [ ] **Step 1: Write the failing tests first**

Create `crates/roy-agents/src/connections.rs`:

```rust
//! Connection store: external channels attached to an agent.
//! See `migrations/sqlite/0013_connections.sql` for the schema.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConnectionError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionKind {
    Telegram,
}

impl ConnectionKind {
    fn as_str(&self) -> &'static str {
        match self {
            ConnectionKind::Telegram => "telegram",
        }
    }
    fn parse(s: &str) -> Result<Self, ConnectionError> {
        match s {
            "telegram" => Ok(ConnectionKind::Telegram),
            other => Err(ConnectionError::Invalid(format!("kind={other}"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub id: String,
    pub agent_id: String,
    pub kind: ConnectionKind,
    pub label: Option<String>,
    /// kind-specific JSON: for telegram `{"bot_token": "..."}`.
    pub credentials: serde_json::Value,
    pub allowed_external_ids: Vec<serde_json::Value>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewConnection {
    pub agent_id: String,
    pub kind: ConnectionKind,
    #[serde(default)]
    pub label: Option<String>,
    pub credentials: serde_json::Value,
    #[serde(default)]
    pub allowed_external_ids: Vec<serde_json::Value>,
}

pub struct ConnectionStore {
    pool: SqlitePool,
}

impl ConnectionStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        new: NewConnection,
        created_by: &str,
    ) -> Result<Connection, ConnectionError> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();
        let creds = serde_json::to_string(&new.credentials)
            .map_err(|e| ConnectionError::Invalid(format!("credentials: {e}")))?;
        let allowed = serde_json::to_string(&new.allowed_external_ids)
            .map_err(|e| ConnectionError::Invalid(format!("allowed_external_ids: {e}")))?;
        sqlx::query(
            "INSERT INTO connections \
             (id, agent_id, kind, label, credentials_json, allowed_external_ids, \
              created_by, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&new.agent_id)
        .bind(new.kind.as_str())
        .bind(&new.label)
        .bind(&creds)
        .bind(&allowed)
        .bind(created_by)
        .bind(now.timestamp())
        .bind(now.timestamp())
        .execute(&self.pool)
        .await?;
        Ok(Connection {
            id,
            agent_id: new.agent_id,
            kind: new.kind,
            label: new.label,
            credentials: new.credentials,
            allowed_external_ids: new.allowed_external_ids,
            created_by: created_by.to_string(),
            created_at: now,
            updated_at: now,
        })
    }

    pub async fn get(&self, id: &str) -> Result<Connection, ConnectionError> {
        let row: Option<(String, String, String, Option<String>, String, String, String, i64, i64)> =
            sqlx::query_as(
                "SELECT id, agent_id, kind, label, credentials_json, allowed_external_ids, \
                 created_by, created_at, updated_at FROM connections WHERE id = ?",
            )
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(row_to_connection)
            .transpose()?
            .ok_or_else(|| ConnectionError::NotFound(id.to_string()))
    }

    pub async fn list_by_agent(
        &self,
        agent_id: &str,
    ) -> Result<Vec<Connection>, ConnectionError> {
        let rows: Vec<(String, String, String, Option<String>, String, String, String, i64, i64)> =
            sqlx::query_as(
                "SELECT id, agent_id, kind, label, credentials_json, allowed_external_ids, \
                 created_by, created_at, updated_at FROM connections \
                 WHERE agent_id = ? ORDER BY created_at",
            )
            .bind(agent_id)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(row_to_connection).collect()
    }

    pub async fn list_by_kind(
        &self,
        kind: ConnectionKind,
    ) -> Result<Vec<Connection>, ConnectionError> {
        let rows: Vec<(String, String, String, Option<String>, String, String, String, i64, i64)> =
            sqlx::query_as(
                "SELECT id, agent_id, kind, label, credentials_json, allowed_external_ids, \
                 created_by, created_at, updated_at FROM connections \
                 WHERE kind = ? ORDER BY created_at",
            )
            .bind(kind.as_str())
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(row_to_connection).collect()
    }

    pub async fn delete(&self, id: &str) -> Result<(), ConnectionError> {
        let res = sqlx::query("DELETE FROM connections WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(ConnectionError::NotFound(id.to_string()));
        }
        Ok(())
    }
}

fn row_to_connection(
    row: (String, String, String, Option<String>, String, String, String, i64, i64),
) -> Result<Connection, ConnectionError> {
    let (id, agent_id, kind, label, creds_json, allowed_json, created_by, created_at, updated_at) =
        row;
    let credentials: serde_json::Value = serde_json::from_str(&creds_json)
        .map_err(|e| ConnectionError::Invalid(format!("credentials_json: {e}")))?;
    let allowed_external_ids: Vec<serde_json::Value> = serde_json::from_str(&allowed_json)
        .map_err(|e| ConnectionError::Invalid(format!("allowed_external_ids: {e}")))?;
    Ok(Connection {
        id,
        agent_id,
        kind: ConnectionKind::parse(&kind)?,
        label,
        credentials,
        allowed_external_ids,
        created_by,
        created_at: DateTime::from_timestamp(created_at, 0).unwrap_or_else(Utc::now),
        updated_at: DateTime::from_timestamp(updated_at, 0).unwrap_or_else(Utc::now),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::seed_user_and_agent;

    #[tokio::test]
    async fn create_then_get_roundtrips() {
        let (pool, user_id, agent_id) = seed_user_and_agent().await;
        let store = ConnectionStore::new(pool);
        let c = store
            .create(
                NewConnection {
                    agent_id: agent_id.clone(),
                    kind: ConnectionKind::Telegram,
                    label: Some("@my_bot".into()),
                    credentials: serde_json::json!({"bot_token": "1234:abc"}),
                    allowed_external_ids: vec![],
                },
                &user_id,
            )
            .await
            .unwrap();
        let got = store.get(&c.id).await.unwrap();
        assert_eq!(got, c);
    }

    #[tokio::test]
    async fn list_by_agent_returns_only_matching() {
        let (pool, user_id, agent_id) = seed_user_and_agent().await;
        let store = ConnectionStore::new(pool);
        store
            .create(
                NewConnection {
                    agent_id: agent_id.clone(),
                    kind: ConnectionKind::Telegram,
                    label: None,
                    credentials: serde_json::json!({"bot_token": "t1"}),
                    allowed_external_ids: vec![],
                },
                &user_id,
            )
            .await
            .unwrap();
        let list = store.list_by_agent(&agent_id).await.unwrap();
        assert_eq!(list.len(), 1);
        let other = store.list_by_agent("nope").await.unwrap();
        assert!(other.is_empty());
    }

    #[tokio::test]
    async fn list_by_kind_groups_across_agents() {
        let (pool, user_id, agent_id) = seed_user_and_agent().await;
        let store = ConnectionStore::new(pool);
        store
            .create(
                NewConnection {
                    agent_id: agent_id.clone(),
                    kind: ConnectionKind::Telegram,
                    label: None,
                    credentials: serde_json::json!({"bot_token": "t1"}),
                    allowed_external_ids: vec![],
                },
                &user_id,
            )
            .await
            .unwrap();
        let all = store.list_by_kind(ConnectionKind::Telegram).await.unwrap();
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn delete_removes() {
        let (pool, user_id, agent_id) = seed_user_and_agent().await;
        let store = ConnectionStore::new(pool);
        let c = store
            .create(
                NewConnection {
                    agent_id,
                    kind: ConnectionKind::Telegram,
                    label: None,
                    credentials: serde_json::json!({"bot_token": "t1"}),
                    allowed_external_ids: vec![],
                },
                &user_id,
            )
            .await
            .unwrap();
        store.delete(&c.id).await.unwrap();
        assert!(matches!(
            store.get(&c.id).await,
            Err(ConnectionError::NotFound(_))
        ));
    }
}
```

- [ ] **Step 2: Ensure `seed_user_and_agent` test helper exists in `crates/roy-agents/src/store.rs`**

If `mod tests { ... fn seed_user_and_agent }` is not already present and pub(crate), add it. Inspect `crates/roy-agents/src/store.rs` for existing test scaffolding; reuse the existing tempdir + open + insert pattern. The helper must:
1. Open a tempdir-backed SQLite via `db::open`.
2. Apply both roy-agents and roy-auth migrations (auth's `users` table is required by the FK in `connections.created_by`). If roy-auth migrations aren't reachable from this crate, the seed function can `INSERT OR IGNORE INTO users (id, ...) VALUES (?, ...)` directly with hard-coded valid values, after the migration set is run with `set_ignore_missing(true)`. **Read `crates/roy-agents/src/store.rs` and follow whatever convention is there.**
3. Insert one user row and one agent row.
4. Return `(SqlitePool, user_id: String, agent_id: String)`.

- [ ] **Step 3: Re-export from lib.rs**

Open `crates/roy-agents/src/lib.rs` and add:

```rust
pub mod connections;
pub use connections::{Connection, ConnectionError, ConnectionKind, ConnectionStore, NewConnection};
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p roy-agents connections::tests -- --nocapture`
Expected: 4 PASS

- [ ] **Step 5: Commit**

```bash
git add crates/roy-agents/src/connections.rs crates/roy-agents/src/lib.rs crates/roy-agents/src/store.rs
git commit -m "feat(roy-agents): ConnectionStore CRUD"
```

---

### Task 3: `session_meta.connection_id` column

**Files:**
- Create: `crates/roy-management/migrations/sqlite/0014_session_connection.sql`
- Modify: `crates/roy-management/src/meta_store.rs`

- [ ] **Step 1: Write the migration**

```sql
-- 0014_session_connection.sql
--
-- Attribute a session to the external Connection that spawned it (gateway
-- guest sessions). NULL for interactive sessions started via /agents/{id}/run.

ALTER TABLE session_meta ADD COLUMN connection_id TEXT REFERENCES connections(id) ON DELETE SET NULL;
CREATE INDEX session_meta_connection ON session_meta(connection_id);
```

- [ ] **Step 2: Extend `SessionMeta` struct in `meta_store.rs`**

Find the struct definition (around `meta_store.rs:56-67`) and add:

```rust
pub connection_id: Option<String>,
```

Update every `SELECT` / `INSERT` / row-mapping site that touches `session_meta` to round-trip the new column. Use `grep -n "session_meta" crates/roy-management/src/meta_store.rs` to find them all. Place `connection_id` after `team_id` in the column order to match the existing convention.

- [ ] **Step 3: Add a setter method on `MetaStore`**

Append to `MetaStore`:

```rust
pub async fn set_session_connection(
    &self,
    session_id: &str,
    connection_id: Option<&str>,
) -> Result<(), MetaError> {
    sqlx::query("UPDATE session_meta SET connection_id = ? WHERE session_id = ?")
        .bind(connection_id)
        .bind(session_id)
        .execute(&self.pool)
        .await?;
    Ok(())
}
```

- [ ] **Step 4: Run existing meta_store tests**

Run: `cargo test -p roy-management meta_store -- --nocapture`
Expected: PASS (no new tests yet — Task 5 covers the wire-through)

- [ ] **Step 5: Commit**

```bash
git add crates/roy-management/migrations/sqlite/0014_session_connection.sql crates/roy-management/src/meta_store.rs
git commit -m "feat(roy-management): session_meta.connection_id for guest-session attribution"
```

---

### Task 4: `CwdScope::AgentChannel`

**Files:**
- Modify: `crates/roy-management/src/cwd.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/roy-management/src/cwd.rs`:

```rust
#[cfg(test)]
mod channel_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn agent_channel_resolves_under_agents_subtree() {
        let dir = tempdir().unwrap();
        let agent_id = uuid::Uuid::new_v4().to_string();
        let conn_id = uuid::Uuid::new_v4().to_string();
        let sid = uuid::Uuid::new_v4().to_string();
        let path = resolve_cwd(
            dir.path(),
            CwdInput {
                scope: CwdScope::AgentChannel {
                    agent_id: agent_id.clone(),
                    connection_id: conn_id.clone(),
                    external_id: "42".into(),
                },
                user_id: "ignored-for-this-scope".into(),
                team_id: None,
                project_id: None,
                session_id: sid.clone(),
            },
        )
        .unwrap();
        let expected = dir
            .path()
            .join("agents")
            .join(&agent_id)
            .join("channels")
            .join(&conn_id)
            .join("42")
            .join("sessions")
            .join(&sid);
        assert_eq!(path, expected);
    }

    #[test]
    fn agent_channel_rejects_external_id_with_slash() {
        let dir = tempdir().unwrap();
        let res = resolve_cwd(
            dir.path(),
            CwdInput {
                scope: CwdScope::AgentChannel {
                    agent_id: uuid::Uuid::new_v4().to_string(),
                    connection_id: uuid::Uuid::new_v4().to_string(),
                    external_id: "../escape".into(),
                },
                user_id: "u".into(),
                team_id: None,
                project_id: None,
                session_id: uuid::Uuid::new_v4().to_string(),
            },
        );
        assert!(matches!(res, Err(CwdError::InvalidId)));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p roy-management cwd::channel_tests -- --nocapture`
Expected: FAIL (variants don't exist)

- [ ] **Step 3: Extend `CwdScope` enum and resolver**

Replace the enum:

```rust
pub enum CwdScope {
    Personal,
    Team,
    /// Session attributable to an Agent through one of its Connections,
    /// keyed by external user id (e.g. Telegram chat_id as a string).
    AgentChannel {
        agent_id: String,
        connection_id: String,
        external_id: String,
    },
}
```

Update `resolve_cwd` to handle the new variant. Add a stricter shape check for `external_id` (alphanumerics, `-`, `_`, no path separators):

```rust
fn is_external_id_safe(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}
```

Inside `resolve_cwd`, before the scope-match:

```rust
if let CwdScope::AgentChannel {
    agent_id,
    connection_id,
    external_id,
} = &input.scope
{
    if !is_uuid_shape(agent_id) || !is_uuid_shape(connection_id) {
        return Err(CwdError::InvalidId);
    }
    if !is_external_id_safe(external_id) {
        return Err(CwdError::InvalidId);
    }
}
```

In the scope-match, add the new arm:

```rust
CwdScope::AgentChannel {
    agent_id,
    connection_id,
    external_id,
} => workspace_dir
    .join("agents")
    .join(agent_id)
    .join("channels")
    .join(connection_id)
    .join(external_id),
```

Then the existing `project_id` branch must not apply for `AgentChannel`; rewrite the path-assembly block as:

```rust
let path = match input.scope {
    CwdScope::Personal | CwdScope::Team => match &input.project_id {
        Some(p) => root
            .join("projects")
            .join(p)
            .join("sessions")
            .join(&input.session_id),
        None => root.join("sessions").join(&input.session_id),
    },
    CwdScope::AgentChannel { .. } => root.join("sessions").join(&input.session_id),
};
```

Where `root` for `AgentChannel` is the path computed above. (Refactor `resolve_cwd` so `root` is computed per variant, then `sessions/<sid>` is appended uniformly.)

- [ ] **Step 4: Run tests to verify all pass**

Run: `cargo test -p roy-management cwd -- --nocapture`
Expected: All cwd tests PASS

- [ ] **Step 5: Commit**

```bash
git add crates/roy-management/src/cwd.rs
git commit -m "feat(roy-management): CwdScope::AgentChannel for guest-session workspaces"
```

---

### Task 5: HTTP CRUD for connections

**Files:**
- Create: `crates/roy-management/src/connections.rs`
- Modify: `crates/roy-management/src/http.rs`
- Modify: `crates/roy-management/src/lib.rs`

- [ ] **Step 1: Write handlers**

Create `crates/roy-management/src/connections.rs`:

```rust
//! axum handlers for `/agents/{agent_id}/connections` CRUD.
//!
//! ACL: the caller must own the agent (`agents.created_by == caller`).
//! Same convention as other agent-scoped routes in `http.rs`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use roy_agents::{Connection, ConnectionKind, ConnectionStore, NewConnection};
use serde::Deserialize;

use crate::auth::AuthUser;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct CreateConnectionReq {
    pub kind: ConnectionKind,
    #[serde(default)]
    pub label: Option<String>,
    pub credentials: serde_json::Value,
    #[serde(default)]
    pub allowed_external_ids: Vec<serde_json::Value>,
}

pub async fn list_connections(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(agent_id): Path<String>,
) -> Result<Json<Vec<Connection>>, (StatusCode, String)> {
    state
        .acl
        .require_agent_owner(&user.id, &agent_id)
        .await
        .map_err(|_| (StatusCode::FORBIDDEN, "not your agent".into()))?;
    let store = ConnectionStore::new(state.agents_pool.clone());
    let list = store
        .list_by_agent(&agent_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(list))
}

pub async fn create_connection(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path(agent_id): Path<String>,
    Json(req): Json<CreateConnectionReq>,
) -> Result<(StatusCode, Json<Connection>), (StatusCode, String)> {
    state
        .acl
        .require_agent_owner(&user.id, &agent_id)
        .await
        .map_err(|_| (StatusCode::FORBIDDEN, "not your agent".into()))?;
    validate_credentials(&req.kind, &req.credentials)
        .map_err(|m| (StatusCode::BAD_REQUEST, m))?;
    let store = ConnectionStore::new(state.agents_pool.clone());
    let c = store
        .create(
            NewConnection {
                agent_id,
                kind: req.kind,
                label: req.label,
                credentials: req.credentials,
                allowed_external_ids: req.allowed_external_ids,
            },
            &user.id,
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok((StatusCode::CREATED, Json(c)))
}

pub async fn delete_connection(
    State(state): State<AppState>,
    AuthUser(user): AuthUser,
    Path((agent_id, connection_id)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .acl
        .require_agent_owner(&user.id, &agent_id)
        .await
        .map_err(|_| (StatusCode::FORBIDDEN, "not your agent".into()))?;
    let store = ConnectionStore::new(state.agents_pool.clone());
    store
        .delete(&connection_id)
        .await
        .map_err(|e| match e {
            roy_agents::ConnectionError::NotFound(_) => (StatusCode::NOT_FOUND, e.to_string()),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        })?;
    Ok(StatusCode::NO_CONTENT)
}

fn validate_credentials(
    kind: &ConnectionKind,
    creds: &serde_json::Value,
) -> Result<(), String> {
    match kind {
        ConnectionKind::Telegram => {
            let tok = creds
                .get("bot_token")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "telegram credentials missing 'bot_token'".to_string())?;
            // Format: <bot_id>:<35-chars-alphanumeric>
            if !tok.contains(':') || tok.len() < 20 || tok.len() > 100 {
                return Err("bot_token does not look like a Telegram token".into());
            }
            Ok(())
        }
    }
}
```

**NOTE for implementer:** the exact names `AppState`, `AuthUser`, `state.acl.require_agent_owner`, `state.agents_pool` are the conventional shapes in this codebase; verify them against `crates/roy-management/src/state.rs`, `auth.rs`, and existing agent handlers in `http.rs`. If a helper like `require_agent_owner` doesn't yet exist, either add it (mirror the existing project-owner check) or inline the SQL: `SELECT created_by FROM agents WHERE id = ?` and compare with `user.id`. Do not invent new abstractions — match what's there.

- [ ] **Step 2: Mount the routes**

Open `crates/roy-management/src/http.rs` and find the `let protected = Router::new()` block (around line 58). Add:

```rust
        .route(
            "/agents/{id}/connections",
            get(connections::list_connections).post(connections::create_connection),
        )
        .route(
            "/agents/{id}/connections/{cid}",
            axum::routing::delete(connections::delete_connection),
        )
```

And at the top of the file add:

```rust
use crate::connections;
```

- [ ] **Step 3: Register the module**

Open `crates/roy-management/src/lib.rs` and add `pub mod connections;` next to the other `pub mod` declarations.

- [ ] **Step 4: Write integration test**

In `crates/roy-management/tests/` (or in `http.rs::tests` following the local convention — check `grep -n "mod tests" crates/roy-management/src/http.rs`), add:

```rust
#[tokio::test]
async fn connection_create_list_delete_roundtrip() {
    let harness = TestHarness::new().await;
    let user = harness.bootstrap_user().await;
    let agent = harness
        .post::<serde_json::Value>(
            &user.token,
            "/agents",
            serde_json::json!({"name": "Pal", "preset": "claude", "prompt": "Be brief."}),
        )
        .await;
    let agent_id = agent["id"].as_str().unwrap();

    let created = harness
        .post::<serde_json::Value>(
            &user.token,
            &format!("/agents/{agent_id}/connections"),
            serde_json::json!({
                "kind": "telegram",
                "label": "@my_bot",
                "credentials": {"bot_token": "1234567890:ABCdefGHI_jklMNOpqrSTUvwxYZ012345678"},
                "allowed_external_ids": []
            }),
        )
        .await;
    let cid = created["id"].as_str().unwrap().to_string();

    let listed: Vec<serde_json::Value> = harness
        .get(&user.token, &format!("/agents/{agent_id}/connections"))
        .await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["label"].as_str(), Some("@my_bot"));

    harness
        .delete(&user.token, &format!("/agents/{agent_id}/connections/{cid}"))
        .await;
    let after: Vec<serde_json::Value> = harness
        .get(&user.token, &format!("/agents/{agent_id}/connections"))
        .await;
    assert!(after.is_empty());
}

#[tokio::test]
async fn connection_create_rejects_bad_telegram_token() {
    let harness = TestHarness::new().await;
    let user = harness.bootstrap_user().await;
    let agent = harness
        .post::<serde_json::Value>(
            &user.token,
            "/agents",
            serde_json::json!({"name": "Pal", "preset": "claude", "prompt": ""}),
        )
        .await;
    let agent_id = agent["id"].as_str().unwrap();

    let resp = harness
        .post_status(
            &user.token,
            &format!("/agents/{agent_id}/connections"),
            serde_json::json!({
                "kind": "telegram",
                "credentials": {"bot_token": "nope"},
                "allowed_external_ids": []
            }),
        )
        .await;
    assert_eq!(resp, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn connection_create_rejects_non_owner() {
    let harness = TestHarness::new().await;
    let owner = harness.bootstrap_user().await;
    let other = harness.create_user("other").await;
    let agent = harness
        .post::<serde_json::Value>(
            &owner.token,
            "/agents",
            serde_json::json!({"name": "Pal", "preset": "claude", "prompt": ""}),
        )
        .await;
    let agent_id = agent["id"].as_str().unwrap();
    let resp = harness
        .post_status(
            &other.token,
            &format!("/agents/{agent_id}/connections"),
            serde_json::json!({
                "kind": "telegram",
                "credentials": {"bot_token": "1234567890:ABCdefGHI_jklMNOpqrSTUvwxYZ012345678"},
                "allowed_external_ids": []
            }),
        )
        .await;
    assert_eq!(resp, StatusCode::FORBIDDEN);
}
```

**NOTE for implementer:** match the existing test-harness shape in this file (search for `TestHarness` or whatever the local helper is named). Don't introduce a new test framework. If `post_status` / `delete` helpers don't exist, add them as minimal additions to the existing harness — mirror the GET/POST methods that are already there.

- [ ] **Step 5: Run tests**

Run: `cargo test -p roy-management connection_ -- --nocapture`
Expected: 3 PASS

- [ ] **Step 6: Commit**

```bash
git add crates/roy-management/src/connections.rs crates/roy-management/src/http.rs crates/roy-management/src/lib.rs
# Plus the modified test file from Step 4
git commit -m "feat(roy-management): HTTP CRUD for agent connections"
```

---

### Task 6: `Conn::spawn` carries `system_prompt`

**Files:**
- Modify: `crates/roy-gateway/src/daemon.rs`
- Modify: `crates/roy-gateway/src/orchestrator.rs` (call sites only)

- [ ] **Step 1: Update the trait + impl**

In `crates/roy-gateway/src/daemon.rs:22`:

```rust
async fn spawn(
    &mut self,
    preset: &str,
    cwd: Option<PathBuf>,
    system_prompt: Option<String>,
) -> Result<String>;
```

In the `TurnConn::spawn` impl (around line 107), replace the `system_prompt: None` with `system_prompt`.

- [ ] **Step 2: Update existing tests**

In `daemon.rs` tests (`turn_conn_spawn_returns_session_id`), update the call site:

```rust
let sid = conn.spawn("claude", None, None).await.unwrap();
```

- [ ] **Step 3: Update orchestrator call site**

In `crates/roy-gateway/src/orchestrator.rs:109`:

```rust
None => conn.spawn(&cfg.preset, cfg.cwd.clone(), cfg.system_prompt.clone()).await,
```

(`cfg.system_prompt` is added in Task 7; for now this won't compile — that's expected.)

- [ ] **Step 4: Run gateway tests partially**

Run: `cargo test -p roy-gateway daemon::tests -- --nocapture`
Expected: PASS (orchestrator will be broken until Task 7)

- [ ] **Step 5: Commit**

```bash
git add crates/roy-gateway/src/daemon.rs
git commit -m "refactor(roy-gateway): Conn::spawn takes system_prompt"
```

(Skip committing `orchestrator.rs` — folded into Task 7.)

---

### Task 7: `OrchestratorConfig` per-connection fields

**Files:**
- Modify: `crates/roy-gateway/src/orchestrator.rs`
- Modify: `crates/roy-gateway/src/binder.rs`

- [ ] **Step 1: Generalize the binder key first**

Open `crates/roy-gateway/src/binder.rs` and replace the `State` definition and methods:

```rust
#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    /// "<connection_id>:<external_id>" → roy session_id.
    /// String key (not tuple) because serde_json::HashMap requires string keys.
    bindings: HashMap<String, String>,
}

fn binding_key(connection_id: &str, external_id: i64) -> String {
    format!("{connection_id}:{external_id}")
}

impl SessionBinder {
    pub async fn load(path: PathBuf) -> Result<Self> { /* unchanged */ }

    pub async fn get(&self, connection_id: &str, external_id: i64) -> Option<String> {
        self.state
            .lock()
            .await
            .bindings
            .get(&binding_key(connection_id, external_id))
            .cloned()
    }

    pub async fn set(
        &self,
        connection_id: &str,
        external_id: i64,
        session_id: String,
    ) -> Result<()> {
        let mut guard = self.state.lock().await;
        guard
            .bindings
            .insert(binding_key(connection_id, external_id), session_id);
        Self::persist(&self.path, &*guard).await
    }

    pub async fn forget(&self, connection_id: &str, external_id: i64) -> Result<()> {
        let mut guard = self.state.lock().await;
        guard
            .bindings
            .remove(&binding_key(connection_id, external_id));
        Self::persist(&self.path, &*guard).await
    }
}
```

Update the existing binder tests to pass the connection_id:

```rust
binder.set("conn-1", 7, "sess-1".into()).await.unwrap();
assert_eq!(binder.get("conn-1", 7).await.as_deref(), Some("sess-1"));
```

**Migration note:** existing binder JSON files have integer keys. Per CLAUDE.md ("no backwards-compat hacks") and because the binder is internal gateway state with no production deployments yet, **do not write a migration**. On startup, an old-format file will fail to deserialize; the operator deletes it and starts fresh. Document this in the file's module doc.

- [ ] **Step 2: Run binder tests**

Run: `cargo test -p roy-gateway binder::tests -- --nocapture`
Expected: PASS

- [ ] **Step 3: Extend `OrchestratorConfig`**

In `crates/roy-gateway/src/orchestrator.rs:26`:

```rust
#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    pub connection_id: String,
    pub preset: String,
    pub cwd: Option<PathBuf>,
    pub system_prompt: Option<String>,
    pub turn_timeout: Duration,
    pub typing_interval: Duration,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            connection_id: String::new(),
            preset: "claude".into(),
            cwd: None,
            system_prompt: None,
            turn_timeout: Duration::from_secs(600),
            typing_interval: Duration::from_secs(4),
        }
    }
}
```

- [ ] **Step 4: Update binder call sites in orchestrator**

In `drive_turn` around line 107:

```rust
let session_id = match binder.get(&cfg.connection_id, chat_id).await {
    Some(sid) => conn.resume(&sid).await,
    None => {
        conn.spawn(&cfg.preset, cfg.cwd.clone(), cfg.system_prompt.clone())
            .await
    }
};
let session_id = match session_id { /* unchanged */ };

binder
    .set(&cfg.connection_id, chat_id, session_id.clone())
    .await?;
```

(Look for every other `binder.get` / `binder.set` / `binder.forget` call in orchestrator.rs and migrate them the same way.)

- [ ] **Step 5: Update existing orchestrator tests**

Add `connection_id: "test-conn".into()` and `system_prompt: None` to every `OrchestratorConfig { ... }` literal in tests. `grep -n "OrchestratorConfig {" crates/roy-gateway/src/orchestrator.rs` to find them all.

- [ ] **Step 6: Run orchestrator tests**

Run: `cargo test -p roy-gateway -- --nocapture`
Expected: All gateway tests PASS

- [ ] **Step 7: Commit**

```bash
git add crates/roy-gateway/src/orchestrator.rs crates/roy-gateway/src/binder.rs
git commit -m "refactor(roy-gateway): per-connection orchestrator config and binder key"
```

---

### Task 8: Multi-bot `build_telegram_tasks`

**Files:**
- Modify: `crates/roy-gateway/Cargo.toml`
- Modify: `crates/roy-gateway/src/config.rs`
- Modify: `crates/roy-gateway/src/lib.rs`

- [ ] **Step 1: Add roy-agents dep**

In `crates/roy-gateway/Cargo.toml`, under `[dependencies]`:

```toml
roy-agents = { path = "../roy-agents" }
```

- [ ] **Step 2: Update config**

Open `crates/roy-gateway/src/config.rs`. Remove the `TelegramConfig` struct and the `telegram` field on `GatewayConfig`. Update `validate()`:

```rust
pub fn validate(&self) -> Result<()> {
    if !self.has_telegram() && self.websocket.is_none() {
        anyhow::bail!("config must enable at least one adapter ([websocket] or telegram via [agents_db])");
    }
    if self.has_telegram() && self.binder.is_none() {
        anyhow::bail!("telegram requires a [binder] section");
    }
    Ok(())
}

pub fn has_telegram(&self) -> bool {
    // Telegram is enabled implicitly when the agents DB is reachable;
    // the connections table is the source of truth. For now, require an
    // explicit opt-in flag so a websocket-only deployment can skip DB I/O.
    self.telegram_enabled.unwrap_or(false)
}
```

Add to the struct:

```rust
#[derive(Debug, Deserialize)]
pub struct GatewayConfig {
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub telegram_enabled: Option<bool>,
    #[serde(default)]
    pub agents_db: Option<AgentsDbConfig>,
    #[serde(default)]
    pub binder: Option<BinderConfig>,
    #[serde(default)]
    pub websocket: Option<WebsocketConfig>,
}

#[derive(Debug, Deserialize)]
pub struct AgentsDbConfig {
    /// Path to the shared agents.db SQLite. Defaults to `roy_agents::default_db_path()`.
    pub path: Option<String>,
}
```

Update the existing config tests: the test `parse_telegram_and_websocket` and `telegram_without_binder_is_an_error` need to use the new `telegram_enabled = true` form. Add a new test:

```rust
#[test]
fn telegram_enabled_without_binder_is_an_error() {
    let raw = r#"
        telegram_enabled = true
    "#;
    let cfg: GatewayConfig = toml::from_str(raw).unwrap();
    assert!(cfg.validate().is_err());
}
```

- [ ] **Step 3: Rewrite `build_telegram_task` as plural**

Open `crates/roy-gateway/src/lib.rs`. Replace `build_telegram_task` with:

```rust
async fn build_telegram_tasks(
    cfg: &GatewayConfig,
    socket_path: &Path,
) -> Result<Vec<tokio::task::JoinHandle<Result<()>>>> {
    if !cfg.has_telegram() {
        return Ok(Vec::new());
    }
    let binder_cfg = cfg
        .binder
        .as_ref()
        .expect("validate() guarantees binder when telegram is enabled");
    let binder_path = PathBuf::from(&binder_cfg.path);
    let binder = Arc::new(
        SessionBinder::load(binder_path.clone())
            .await
            .with_context(|| format!("loading binder {}", binder_path.display()))?,
    );

    // Open the agents DB read-only (gateway is a reader; management is the writer).
    let db_path = cfg
        .agents_db
        .as_ref()
        .and_then(|c| c.path.clone().map(PathBuf::from))
        .unwrap_or_else(roy_agents::db::default_db_path);
    let pool = roy_agents::db::open(&db_path)
        .await
        .with_context(|| format!("opening agents.db at {}", db_path.display()))?;
    let conn_store = roy_agents::ConnectionStore::new(pool.clone());
    let agent_store = roy_agents::AgentStore::new(pool.clone()); // verify exact name in roy-agents
    let connections = conn_store
        .list_by_kind(roy_agents::ConnectionKind::Telegram)
        .await
        .context("loading telegram connections")?;
    tracing::info!(count = connections.len(), "loaded telegram connections");

    let conn_factory = Arc::new(RealConnFactory::new(socket_path.to_path_buf()));
    let mut tasks = Vec::with_capacity(connections.len());

    for c in connections {
        let token = c
            .credentials
            .get("bot_token")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let Some(token) = token else {
            tracing::warn!(connection_id = %c.id, "telegram connection missing bot_token, skipping");
            continue;
        };
        let agent = match agent_store.get(&c.agent_id).await {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(connection_id = %c.id, agent_id = %c.agent_id, error = ?e, "agent missing for connection, skipping");
                continue;
            }
        };
        // External-id allowlist: each entry is a JSON number for Telegram user ids.
        let allowed: HashSet<u64> = c
            .allowed_external_ids
            .iter()
            .filter_map(|v| v.as_u64())
            .collect();
        let orch_cfg = Arc::new(OrchestratorConfig {
            connection_id: c.id.clone(),
            preset: agent.preset.clone(),
            cwd: None, // TODO: per-guest cwd via management; see Task 9.
            system_prompt: Some(agent.prompt.clone()).filter(|s| !s.is_empty()),
            turn_timeout: Duration::from_secs(600),
            typing_interval: Duration::from_secs(4),
        });
        let bot = Bot::new(token);
        let replier = Arc::new(TeloxideReplier::new(bot.clone()));
        let deps = BotDeps {
            cfg: orch_cfg,
            binder: binder.clone(),
            conn_factory: conn_factory.clone(),
            replier,
            cancel_registry: CancelRegistry::new(),
            allowed_user_ids: Arc::new(allowed),
        };
        let connection_id = c.id.clone();
        tasks.push(tokio::spawn(async move {
            tracing::info!(connection_id, "telegram bot task starting");
            telegram_run(bot, deps).await
        }));
    }
    Ok(tasks)
}
```

Update the `run` function to handle a Vec of tasks:

```rust
let telegram_tasks = build_telegram_tasks(&cfg, &socket_path).await?;
let ws_task = build_ws_task(&cfg, &socket_path)?;

if telegram_tasks.is_empty() && ws_task.is_none() {
    anyhow::bail!("no adapter started; check [websocket] or telegram_enabled config");
}

// Race: exit on first failure of any task.
let mut futures: futures::stream::FuturesUnordered<_> = telegram_tasks
    .into_iter()
    .map(|t| Box::pin(async move { t.await.context("telegram task") }) as _)
    .collect();
if let Some(ws) = ws_task {
    futures.push(Box::pin(async move { ws.await.context("ws task") }) as _);
}

use futures::StreamExt;
if let Some(result) = futures.next().await {
    result??;
}
Ok(())
```

(Add `futures = "0.3"` to `Cargo.toml` if not already there; the project likely already has it indirectly.)

- [ ] **Step 4: Drop the old `build_telegram_task`**

Delete the singular function; remove the `HashSet` and other imports that only it used (if any).

- [ ] **Step 5: Build the workspace**

Run: `cargo build --workspace --all-targets`
Expected: Builds clean. **If `roy_agents::AgentStore::new` is named differently, fix it now** (search `crates/roy-agents/src/store.rs` for the actual constructor; likely `AgentStore::new(pool)` or similar).

- [ ] **Step 6: Run all tests**

Run: `cargo test --workspace --no-fail-fast`
Expected: All PASS

- [ ] **Step 7: Commit**

```bash
git add crates/roy-gateway/Cargo.toml crates/roy-gateway/src/lib.rs crates/roy-gateway/src/config.rs
git commit -m "feat(roy-gateway): multi-bot Telegram driven by connections table"
```

---

### Task 9: Per-guest cwd via management

**Files:**
- Modify: `crates/roy-management/src/roy_client.rs`
- Modify: `crates/roy-gateway/src/orchestrator.rs`
- Modify: `crates/roy-management/src/http.rs`

This task closes the loop: the gateway needs management to resolve the guest cwd (so workspace ownership stays in management) and stamp `session_meta.connection_id`.

Add a new internal endpoint **POST /internal/sessions/guest** in management that:
- Accepts `{ connection_id, external_id }`.
- Looks up the connection, resolves cwd via `CwdScope::AgentChannel { ... }`, mkdirs it.
- Persists a `session_meta` row with `agent_id = connection.agent_id, connection_id = connection.id`.
- Returns `{ cwd: String, system_prompt: Option<String> }` for the gateway to feed into `Spawn`.

This means the gateway calls management's HTTP API (not the daemon's Unix socket) to resolve cwd. This is a new gateway→management dependency.

- [ ] **Step 1: Decide auth**

Internal endpoint should be bypassed from regular JWT auth. Two safe options: (a) require a service token via env (`ROY_INTERNAL_TOKEN`), or (b) bind on loopback only. Pick (a) — symmetric to roy-auth's bootstrap token model. Add a new env var `ROY_INTERNAL_TOKEN`; management compares header `X-Roy-Internal: <token>`.

- [ ] **Step 2: Implement endpoint**

In `crates/roy-management/src/http.rs`, mount on the `public` router (no JWT middleware):

```rust
.route("/internal/sessions/guest", post(internal::resolve_guest_session))
```

In a new `crates/roy-management/src/internal.rs`:

```rust
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::cwd::{resolve_cwd, CwdInput, CwdScope};
use crate::state::AppState;

#[derive(Deserialize)]
pub struct GuestSessionReq {
    pub connection_id: String,
    pub external_id: String,
    pub session_id: String,
}

#[derive(Serialize)]
pub struct GuestSessionResp {
    pub cwd: String,
    pub system_prompt: Option<String>,
}

pub async fn resolve_guest_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<GuestSessionReq>,
) -> Result<Json<GuestSessionResp>, (StatusCode, String)> {
    let want = std::env::var("ROY_INTERNAL_TOKEN")
        .map_err(|_| (StatusCode::SERVICE_UNAVAILABLE, "internal API disabled".into()))?;
    let got = headers
        .get("X-Roy-Internal")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if got != want {
        return Err((StatusCode::UNAUTHORIZED, "bad internal token".into()));
    }

    let store = roy_agents::ConnectionStore::new(state.agents_pool.clone());
    let conn = store
        .get(&req.connection_id)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "connection".into()))?;
    let agent_store = roy_agents::AgentStore::new(state.agents_pool.clone());
    let agent = agent_store
        .get(&conn.agent_id)
        .await
        .map_err(|_| (StatusCode::NOT_FOUND, "agent".into()))?;

    let path = resolve_cwd(
        &state.workspace_dir,
        CwdInput {
            scope: CwdScope::AgentChannel {
                agent_id: conn.agent_id.clone(),
                connection_id: conn.id.clone(),
                external_id: req.external_id.clone(),
            },
            user_id: conn.created_by.clone(),
            team_id: None,
            project_id: None,
            session_id: req.session_id.clone(),
        },
    )
    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    // Stamp session_meta after the daemon spawn returns. We don't know
    // session_id until the daemon mints it, but the gateway will pass us
    // its own pre-allocated UUID (request must include it). Persist the
    // attribution row now so listing UI sees it immediately.
    state
        .meta
        .create_session_meta_for_guest(
            &req.session_id,
            &conn.agent_id,
            &agent.name,
            &conn.id,
            &conn.created_by,
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(GuestSessionResp {
        cwd: path.to_string_lossy().into_owned(),
        system_prompt: if agent.prompt.is_empty() {
            None
        } else {
            Some(agent.prompt)
        },
    }))
}
```

Implement `MetaStore::create_session_meta_for_guest` in `meta_store.rs`: insert a row with `connection_id` set, `created_by = connection.created_by` (the agent owner), `agent_id`, `agent_name`. Empty `tags`, no `project_id`, no `team_id`.

- [ ] **Step 3: Gateway calls management**

In `crates/roy-gateway/src/orchestrator.rs`, before the `conn.spawn` call inside `drive_turn`, add a management call. Pre-generate the session UUID gateway-side and pass it in. Add a small management-client to the gateway:

Create `crates/roy-gateway/src/management.rs`:

```rust
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct GuestSessionReq<'a> {
    pub connection_id: &'a str,
    pub external_id: &'a str,
    pub session_id: &'a str,
}

#[derive(Deserialize, Debug)]
pub struct GuestSessionResp {
    pub cwd: String,
    pub system_prompt: Option<String>,
}

pub struct ManagementClient {
    base: String,
    token: String,
    client: reqwest::Client,
}

impl ManagementClient {
    pub fn new(base: String, token: String) -> Self {
        Self {
            base,
            token,
            client: reqwest::Client::new(),
        }
    }

    pub async fn resolve_guest_session(
        &self,
        req: GuestSessionReq<'_>,
    ) -> Result<GuestSessionResp> {
        let resp = self
            .client
            .post(format!("{}/internal/sessions/guest", self.base))
            .header("X-Roy-Internal", &self.token)
            .json(&req)
            .send()
            .await
            .context("posting guest session")?;
        if !resp.status().is_success() {
            anyhow::bail!("management {}: {}", resp.status(), resp.text().await.unwrap_or_default());
        }
        Ok(resp.json().await.context("decoding guest session resp")?)
    }
}
```

Add `reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }` to `roy-gateway/Cargo.toml` if not already there.

Plumb a `ManagementClient` into `BotDeps`, build it in `build_telegram_tasks` from env (`ROY_MANAGEMENT_URL`, `ROY_INTERNAL_TOKEN`). In the orchestrator's `drive_turn`, on cache miss, call:

```rust
let session_id_guess = uuid::Uuid::new_v4().to_string();
let resolved = management
    .resolve_guest_session(GuestSessionReq {
        connection_id: &cfg.connection_id,
        external_id: &chat_id.to_string(),
        session_id: &session_id_guess,
    })
    .await?;
let session_id = conn
    .spawn(
        &cfg.preset,
        Some(PathBuf::from(resolved.cwd)),
        resolved.system_prompt,
    )
    .await?;
binder.set(&cfg.connection_id, chat_id, session_id.clone()).await?;
```

(Note: `session_id_guess` is passed to management for the metadata row, but the daemon mints its own; per CLAUDE.md the daemon's id is authoritative. **Sanity check:** does the daemon accept a caller-provided id in Spawn? If not, the metadata row stores the daemon's id instead. Read `crates/roy/src/control.rs::ClientCommand::Spawn` to confirm — if there's no `session_id` field, restructure: gateway spawns first, then calls management with the daemon's id to stamp metadata.)

- [ ] **Step 4: Tests**

Add a unit test for `ManagementClient` using `wiremock` or a tokio-based fake server. Sanity-test that the gateway falls back gracefully (logs + drops the message) if management is unreachable — guest sessions should not silently hang.

- [ ] **Step 5: Run all tests**

Run: `cargo test --workspace --no-fail-fast`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/roy-management/src/internal.rs crates/roy-management/src/http.rs crates/roy-management/src/meta_store.rs crates/roy-gateway/src/management.rs crates/roy-gateway/src/orchestrator.rs crates/roy-gateway/src/lib.rs crates/roy-gateway/Cargo.toml
git commit -m "feat(gateway+management): per-guest cwd resolution and session attribution"
```

---

## Phase B: UI in `roy-web`

### Task 10: Connections API client

**Files:**
- Modify: `../roy-web/src/lib/management-client.ts`

- [ ] **Step 1: Add types and namespace**

Append to `management-client.ts`:

```ts
export type ConnectionKind = 'telegram';

export interface Connection {
  id: string;
  agent_id: string;
  kind: ConnectionKind;
  label: string | null;
  credentials: Record<string, unknown>;
  allowed_external_ids: Array<string | number>;
  created_by: string;
  created_at: string;
  updated_at: string;
}

export interface CreateConnectionBody {
  kind: ConnectionKind;
  label?: string | null;
  credentials: Record<string, unknown>;
  allowed_external_ids?: Array<string | number>;
}

export const connections = {
  list: (agentId: string) =>
    request<Connection[]>(`/agents/${encodeURIComponent(agentId)}/connections`),
  create: (agentId: string, body: CreateConnectionBody) =>
    request<Connection>(`/agents/${encodeURIComponent(agentId)}/connections`, {
      method: 'POST',
      body: JSON.stringify(body),
    }),
  delete: (agentId: string, connectionId: string) =>
    request<void>(
      `/agents/${encodeURIComponent(agentId)}/connections/${encodeURIComponent(connectionId)}`,
      { method: 'DELETE' },
    ),
};
```

Mirror the existing `agents`/`projects`/`sessions` namespace style exactly.

- [ ] **Step 2: Type-check**

Run: `cd ../roy-web && npm run check`
Expected: 0 errors

- [ ] **Step 3: Commit**

```bash
cd ../roy-web && git add src/lib/management-client.ts
git commit -m "feat(web): connections API client"
```

---

### Task 11: `AgentConnectionsPanel` component

**Files:**
- Create: `../roy-web/src/lib/AgentConnectionsPanel.svelte`

- [ ] **Step 1: Build the component**

```svelte
<script lang="ts">
  import { Button } from '$lib/components/ui/button';
  import { Card } from '$lib/components/ui/card';
  import * as Dialog from '$lib/components/ui/dialog';
  import { Input } from '$lib/components/ui/input';
  import { Label } from '$lib/components/ui/label';
  import { connections, type Connection } from './management-client';

  let { agentId }: { agentId: string } = $props();

  let list = $state<Connection[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let attachOpen = $state(false);
  let botToken = $state('');
  let label = $state('');
  let attaching = $state(false);
  let attachError = $state<string | null>(null);

  async function refresh() {
    loading = true;
    error = null;
    try {
      list = await connections.list(agentId);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  async function attach() {
    attaching = true;
    attachError = null;
    try {
      await connections.create(agentId, {
        kind: 'telegram',
        label: label || null,
        credentials: { bot_token: botToken },
        allowed_external_ids: [],
      });
      botToken = '';
      label = '';
      attachOpen = false;
      await refresh();
    } catch (e) {
      attachError = e instanceof Error ? e.message : String(e);
    } finally {
      attaching = false;
    }
  }

  async function remove(id: string) {
    if (!confirm('Detach this connection? Existing chats lose the bot.')) return;
    await connections.delete(agentId, id);
    await refresh();
  }

  $effect(() => {
    if (agentId) refresh();
  });
</script>

<div class="space-y-3">
  <div class="flex items-center justify-between">
    <h3 class="text-sm font-semibold">Connections</h3>
    <Button size="sm" onclick={() => (attachOpen = true)}>Attach Telegram bot</Button>
  </div>

  {#if loading}
    <p class="text-xs text-muted-foreground">Loading…</p>
  {:else if error}
    <p class="text-xs text-destructive">{error}</p>
  {:else if list.length === 0}
    <p class="text-xs text-muted-foreground">
      No connections yet. Attach a Telegram bot to let users chat with this agent.
      A gateway restart is required to pick up the new bot.
    </p>
  {:else}
    <ul class="space-y-2">
      {#each list as c (c.id)}
        <li class="flex items-center justify-between rounded border p-2 text-sm">
          <div class="flex flex-col">
            <span class="font-medium">{c.label ?? '(unlabeled)'}</span>
            <span class="text-xs text-muted-foreground">{c.kind} · added {new Date(c.created_at).toLocaleDateString()}</span>
          </div>
          <Button size="sm" variant="ghost" onclick={() => remove(c.id)}>Detach</Button>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<Dialog.Root bind:open={attachOpen}>
  <Dialog.Content>
    <Dialog.Header>
      <Dialog.Title>Attach Telegram bot</Dialog.Title>
      <Dialog.Description>
        Paste a bot token from <a class="underline" href="https://t.me/BotFather" target="_blank" rel="noreferrer">@BotFather</a>.
        Each user who DMs the bot gets their own session with this agent.
      </Dialog.Description>
    </Dialog.Header>
    <div class="space-y-3">
      <div class="space-y-1">
        <Label for="bot-token">Bot token</Label>
        <Input id="bot-token" type="password" bind:value={botToken} placeholder="123456:ABC-DEF..." />
      </div>
      <div class="space-y-1">
        <Label for="bot-label">Label (optional)</Label>
        <Input id="bot-label" bind:value={label} placeholder="@my_bot" />
      </div>
      {#if attachError}
        <p class="text-xs text-destructive">{attachError}</p>
      {/if}
    </div>
    <Dialog.Footer>
      <Button variant="ghost" onclick={() => (attachOpen = false)}>Cancel</Button>
      <Button onclick={attach} disabled={!botToken || attaching}>
        {attaching ? 'Attaching…' : 'Attach'}
      </Button>
    </Dialog.Footer>
  </Dialog.Content>
</Dialog.Root>
```

- [ ] **Step 2: Type-check**

Run: `cd ../roy-web && npm run check`
Expected: 0 errors

- [ ] **Step 3: Commit**

```bash
cd ../roy-web && git add src/lib/AgentConnectionsPanel.svelte
git commit -m "feat(web): AgentConnectionsPanel for attaching Telegram bots"
```

---

### Task 12: Mount the panel inside `AgentsView`

**Files:**
- Modify: `../roy-web/src/lib/AgentsView.svelte`

- [ ] **Step 1: Find the agent-detail/edit section**

Open `AgentsView.svelte` (it's the existing agent management view). Locate the block that shows fields for a selected agent (name, preset, prompt, etc.). The panel should mount under the prompt editor, in the same column.

- [ ] **Step 2: Import and render**

Add to the `<script>`:

```ts
import AgentConnectionsPanel from './AgentConnectionsPanel.svelte';
```

In the markup, after the prompt editor and before any save buttons:

```svelte
{#if selectedAgent?.id}
  <Separator class="my-4" />
  <AgentConnectionsPanel agentId={selectedAgent.id} />
{/if}
```

(Adjust `selectedAgent` to the actual binding name used in this view — `grep` for the existing agent selection state.)

- [ ] **Step 3: Manual UI smoke**

Run: `cd ../roy-web && npm run dev`
- Log in.
- Open Agents view.
- Select any existing agent.
- Verify the "Connections" panel appears below the prompt editor with "No connections yet" message.
- Click "Attach Telegram bot". Paste a fake token (`1234567890:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA1`).
- Confirm the request reaches `/agents/<id>/connections` (check the browser devtools Network tab). With management running, expect a 201 + the connection appearing in the list.

- [ ] **Step 4: Commit**

```bash
cd ../roy-web && git add src/lib/AgentsView.svelte
git commit -m "feat(web): mount AgentConnectionsPanel in agent detail"
```

---

### Task 13: Show connection badge on sessions

**Files:**
- Modify: `../roy-web/src/lib/SessionList.svelte`
- Modify: `../roy-web/src/lib/management-client.ts` (extend `SessionMetaRow`)

- [ ] **Step 1: Add `connection_id` to the session row type**

In `management-client.ts`, find `SessionMetaRow` (around line 101 per the earlier grep). Add:

```ts
connection_id?: string | null;
```

- [ ] **Step 2: Render badge**

In `SessionList.svelte`, find the row template. Next to the existing agent-name display, add:

```svelte
{#if session.connection_id}
  <Badge variant="secondary" class="ml-2">via {session.connection_id.slice(0, 6)}</Badge>
{/if}
```

(If the existing UI has a more elegant connection-label lookup pattern, fetch the connection list once at view mount and resolve `connection_id → label` instead of slicing the UUID.)

- [ ] **Step 3: Type-check + manual smoke**

Run: `cd ../roy-web && npm run check && npm run dev`
- After a guest message hits a connection (Phase A Task 9), reload the Sessions view.
- Verify the new session row shows the "via …" badge.

- [ ] **Step 4: Commit**

```bash
cd ../roy-web && git add src/lib/SessionList.svelte src/lib/management-client.ts
git commit -m "feat(web): show connection badge on guest sessions"
```

---

## Phase C: Integration smoke + docs

### Task 14: End-to-end manual smoke

This is a runbook, not code. Execute it after Tasks 1-13 land.

- [ ] **Step 1: Set up environment**

```bash
# Terminal 1: daemon
cargo run --bin roy -- serve

# Terminal 2: management
export ROY_JWT_SECRET=$(head -c 32 /dev/urandom | base64)
export ROY_INTERNAL_TOKEN=$(head -c 24 /dev/urandom | base64)
cargo run --bin roy -- management --bind 127.0.0.1:9999

# Terminal 3: gateway (with telegram enabled)
cat > /tmp/gateway.toml <<'EOF'
telegram_enabled = true

[binder]
path = "/tmp/roy-binder.json"
EOF
export ROY_MANAGEMENT_URL=http://127.0.0.1:9999
export ROY_INTERNAL_TOKEN=<same as above>
cargo run --bin roy -- gateway --config /tmp/gateway.toml

# Terminal 4: web
cd ../roy-web && npm run dev
```

- [ ] **Step 2: Create two bots via @BotFather**

Get two distinct tokens. Note both usernames.

- [ ] **Step 3: Create two agents in the UI**

Log in. Create "Agent A" (preset claude, prompt "You are extremely terse."). Create "Agent B" (preset claude, prompt "Respond only in haiku.").

- [ ] **Step 4: Attach a different bot to each agent**

- Open Agent A → Attach Telegram bot → paste token A.
- Open Agent B → Attach Telegram bot → paste token B.

- [ ] **Step 5: Restart gateway**

Stop and re-run the gateway command (it doesn't hot-reload connections).

- [ ] **Step 6: DM both bots from two Telegram accounts**

- User1 DMs bot A: "ping" → expect terse reply.
- User1 DMs bot B: "tell me about rain" → expect haiku.
- User2 DMs bot A: "ping" → expect terse reply, **distinct session** from User1.

- [ ] **Step 7: Verify in the UI**

- Sessions view shows 3 new rows.
- Each row labeled with the right agent + a "via …" badge naming the connection.
- Clicking a row opens ChatView with the journal for that conversation.

- [ ] **Step 8: Document the runbook**

Create or extend `docs/agent-connections.md` with the above runbook (without the Telegram-specific token capture). Reference it from `CLAUDE.md`.

---

### Task 15: Update CLAUDE.md

**Files:**
- Modify: `crates/roy/CLAUDE.md` (the repo-root project guide referenced via the working dir)

- [ ] **Step 1: Add a bullet under the roy-gateway description**

After the current paragraph about `roy-gateway`, add:

```markdown
Telegram bots are sourced from the `connections` table (`roy-agents`
crate) at gateway startup; each row spawns one teloxide task. The
`[telegram]` TOML block from v1 is removed — enable Telegram with
`telegram_enabled = true` in the config and manage bot tokens via
`POST /agents/{id}/connections` (the `AgentConnectionsPanel` in
roy-web is the canonical UI). Adding/removing a connection requires
a gateway restart in v1.
```

- [ ] **Step 2: Update the "External crates" paragraph**

Append a sentence noting that `roy-gateway` now also depends on `roy-agents` (read-only on the connections table) and reaches `roy-management` over HTTP for per-guest cwd resolution.

- [ ] **Step 3: Commit**

```bash
git add crates/roy/CLAUDE.md
git commit -m "docs: agent connections / multi-bot Telegram gateway"
```

---

## Success Criteria

- All `cargo test --workspace --no-fail-fast` pass on Phase A completion.
- `cd ../roy-web && npm run check` passes on Phase B completion.
- The Task 14 smoke runbook succeeds end-to-end: two bots on two agents serving two users with isolated sessions, all visible in the web UI.
- No `[telegram]` block remains in any sample config; the gateway refuses to start with the legacy shape.
- A reader of the new CLAUDE.md paragraph can guess the data flow without opening any source file.

## Non-Goals Recap

- **No hot-reload** of connections. Restart gateway after add/remove.
- **No bot-token encryption** at rest in v1.
- **No web channel** in v1, but the schema (`kind` enum), the cwd resolver (`CwdScope::AgentChannel`), and the binder key (`(connection_id, external_id)`) are extension-ready.
- **No group-chat support**: `chat_id` is the binding key and we only document private-DM semantics.
