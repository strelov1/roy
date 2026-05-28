# Connections — User-Owned MCP Proxy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users register MCP servers ("connections") on their profile and attach them to chat sessions, so the ACP agent gets those tools available transparently. roy itself runs as the proxy between the agent and the user's upstream MCP servers — no external CLI dependency, single Rust artifact.

**Architecture:** New `connections` table in the shared `agents.db`, owned per-user. `ClientCommand::Spawn` gains `connections: Vec<ConnectionSpec>` carrying inline spawn metadata (the daemon never reads the DB). New `roy mcp serve-connections` subcommand acts as a tiny proxying MCP server: it speaks JSON-RPC 2.0 over stdio to its parent (the ACP agent), spawns each upstream MCP as a child process, aggregates `tools/list`, namespace-prefixes tool names as `<slug>__<tool>`, and proxies `tools/call`. For the claude preset the daemon writes a `.mcp.json` into the session cwd before spawning `claude-code-acp` — that's the project-level MCP config Claude Code reads natively.

**MVP scope (this plan):**
- One transport: **stdio** upstream MCP only (no HTTP / SSE).
- One preset: **claude** only (`AcpConfig::claude`). Other presets get the data plumbed through but no injection — explicit "not yet supported" runtime check.
- Secrets stored as **plain JSON** in the DB row (file mode 0600 already enforced by `roy_agents::open`). Encryption is out of scope; called out as follow-up.
- No `always_attach` flag — connections are attached to a session only via explicit `connection_ids` at create time. Follow-up.
- No `notifications/tools/list_changed` push — tools snapshot at session spawn. Follow-up.
- No real-CLI smoke test — covered by a unit-style integration test using a fake upstream MCP (`tests/scripts/fake-mcp-upstream.py`).

**Tech Stack:** Rust 2021, sqlx 0.8 (sqlite + WAL), axum 0.8, tokio, `serde_json`, JSON-RPC 2.0 over stdio. Existing testing fixture: in-memory SQLite via `tempfile::tempdir`, python3 stdio fakes (same pattern as `tests/scripts/fake-acp-agent.py`).

**Execution order:** Phases A → F strictly sequential. Each phase ends with `cargo test --workspace --no-fail-fast` green. Within a phase, tasks must be done in order.

---

## File map

**New files:**
- `crates/roy-management/migrations/sqlite/0006_connections.sql`
- `crates/roy-management/src/connections.rs`              ─ types + Store + HTTP handlers
- `crates/roy-management/tests/connections_http.rs`       ─ CRUD integration tests
- `crates/roy-mcp/src/serve_connections/mod.rs`           ─ subcommand entry + JSON-RPC dispatcher
- `crates/roy-mcp/src/serve_connections/spec.rs`          ─ `ConnectionSpec` wire shape, args parsing
- `crates/roy-mcp/src/serve_connections/upstream.rs`      ─ one upstream child process + JSON-RPC client
- `crates/roy-mcp/src/serve_connections/registry.rs`      ─ aggregate tools, route `tools/call`
- `crates/roy-mcp/tests/serve_connections.rs`             ─ end-to-end stdio test with fake upstream
- `crates/roy-mcp/tests/scripts/fake-mcp-upstream.py`     ─ minimal stdio MCP fake
- `crates/roy/src/transport/acp/mcp_injection.rs`         ─ build `.mcp.json` body for claude

**Modified files:**
- `crates/roy/src/control.rs`                             ─ add `connections` to `Spawn`, define `ConnectionSpec`
- `crates/roy/src/lib.rs`                                 ─ re-export `ConnectionSpec`
- `crates/roy/src/engine.rs`                              ─ `SessionSpawnConfig` gets `connections`
- `crates/roy/src/manager.rs`                             ─ pass `connections` through factory + resume path
- `crates/roy/src/daemon.rs`                              ─ `handle_spawn` accepts connections; `TransportFactory::build` signature extended
- `crates/roy/src/session_store.rs`                       ─ persist `connections` on `SessionRow` for resume
- `crates/roy/src/transport/acp/mod.rs`                   ─ `AcpConfig` carries `connections`; `claude()` opts into MCP injection
- `crates/roy-mcp/src/lib.rs`                             ─ re-export `serve_connections` entry
- `crates/roy-management/src/lib.rs`                      ─ wire `connections` module
- `crates/roy-management/src/http.rs`                     ─ mount `/connections` routes; extend `CreateSessionReq` + `SpawnRequest` callsite
- `crates/roy-management/src/roy_client.rs`               ─ `SpawnRequest` carries connections; `DaemonClient::spawn` forwards them
- `crates/roy-management/src/meta_store.rs`               ─ persist `connection_ids` on `SessionMeta` for audit/resume
- `crates/roy-management/migrations/sqlite/0006_connections.sql` (already in "new" list — also requires `session_meta` ALTER, see Task A2)
- `crates/roy-cli/src/main.rs`                            ─ surface `roy mcp serve-connections`
- `crates/roy/Cargo.toml`                                 ─ no new deps
- `crates/roy-mcp/Cargo.toml`                             ─ add `clap`, `sqlx` (read-only), `roy-management` no — keep `roy-mcp` independent

---

## Phase A — DB + types + CRUD (no spawn integration)

End state: a logged-in user can `POST /connections`, list, update, delete via HTTP. No daemon plumbing yet. This phase is shippable on its own.

### Task A1: Migration `0006_connections.sql`

**Files:**
- Create: `crates/roy-management/migrations/sqlite/0006_connections.sql`

- [ ] **Step 1: Write the migration**

```sql
-- 0006_connections.sql
--
-- User-owned MCP-server connections. One row = one upstream MCP the user has
-- registered. Inline credentials live in `secrets_json` (plain JSON, file
-- mode 0600 already enforced by roy-agents::open). A follow-up plan will add
-- column-level encryption.
--
-- `kind` is reserved for future transports (mcp_http, mcp_sse, ...). MVP
-- accepts only 'mcp_stdio'; other values are rejected by the store layer.

CREATE TABLE connections (
    id           TEXT PRIMARY KEY,
    owner_id     TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    slug         TEXT NOT NULL,
    kind         TEXT NOT NULL,
    config_json  TEXT NOT NULL,
    secrets_json TEXT,
    description  TEXT,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    UNIQUE (owner_id, slug)
);
CREATE INDEX connections_owner_idx ON connections(owner_id);

-- session_meta needs a column to recall which connections a session was
-- spawned with — required for /sessions GET (UI display) and for future
-- resume support. JSON array of connection ids; empty array = no connections.
ALTER TABLE session_meta ADD COLUMN connection_ids TEXT NOT NULL DEFAULT '[]';
```

- [ ] **Step 2: Verify the migration loads**

Run: `cargo test -p roy-management migrations -- --nocapture`
(There is no dedicated migration test today — the migration is exercised by every existing integration test in `crates/roy-management/tests/`. Run the whole suite to confirm it doesn't break.)

Run: `cargo test -p roy-management --no-fail-fast`
Expected: PASS (existing tests still green; new table created on every test pool).

- [ ] **Step 3: Commit**

```bash
git add crates/roy-management/migrations/sqlite/0006_connections.sql
git commit -m "feat(roy-management): add connections table and session_meta.connection_ids"
```

### Task A2: `Connection` types + slug

**Files:**
- Create: `crates/roy-management/src/connections.rs` (initial — types only)

- [ ] **Step 1: Add the module declaration and types**

In `crates/roy-management/src/lib.rs`, add `pub mod connections;` next to the other `pub mod` declarations.

Create `crates/roy-management/src/connections.rs`:

```rust
//! User-owned MCP connections: types, store, and HTTP handlers.
//!
//! Owner is always a user (no team-shared connections in MVP). Slugs are
//! derived from `name` and made unique per-owner by suffixing (`-2`, `-3`,
//! ...) — same pattern as `roy_agents::store`.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One stored connection. `config_json` and `secrets_json` are kind-specific;
/// the store layer keeps them as opaque JSON and only the
/// `roy-mcp serve-connections` consumer parses them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Connection {
    pub id: String,
    pub owner_id: String,
    pub name: String,
    pub slug: String,
    pub kind: String,
    #[sqlx(rename = "config_json")]
    #[serde(rename = "config")]
    pub config: Value,
    #[sqlx(rename = "secrets_json")]
    #[serde(rename = "secrets")]
    pub secrets: Option<Value>,
    pub description: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewConnection {
    pub name: String,
    pub kind: String,
    pub config: Value,
    #[serde(default)]
    pub secrets: Option<Value>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ConnectionUpdate {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub config: Option<Value>,
    #[serde(default, deserialize_with = "roy_agents::types::deserialize_optional_field")]
    pub secrets: Option<Option<Value>>,
    #[serde(default, deserialize_with = "roy_agents::types::deserialize_optional_field")]
    pub description: Option<Option<String>>,
}

pub const KIND_MCP_STDIO: &str = "mcp_stdio";

pub fn validate_kind(kind: &str) -> Result<(), String> {
    match kind {
        KIND_MCP_STDIO => Ok(()),
        other => Err(format!(
            "unsupported connection kind '{other}'; MVP supports only 'mcp_stdio'"
        )),
    }
}

/// Validate `config_json` shape for a given `kind`. Returns a human-readable
/// reason on failure (mapped to HTTP 400 by the handler layer).
pub fn validate_config(kind: &str, config: &Value) -> Result<(), String> {
    match kind {
        KIND_MCP_STDIO => {
            let obj = config
                .as_object()
                .ok_or_else(|| "config must be an object".to_string())?;
            let cmd = obj
                .get("command")
                .and_then(Value::as_str)
                .ok_or_else(|| "config.command (string) is required".to_string())?;
            if cmd.is_empty() {
                return Err("config.command must be non-empty".to_string());
            }
            if let Some(args) = obj.get("args") {
                if !args.is_array() {
                    return Err("config.args must be an array of strings".to_string());
                }
                for (i, a) in args.as_array().unwrap().iter().enumerate() {
                    if !a.is_string() {
                        return Err(format!("config.args[{i}] must be a string"));
                    }
                }
            }
            if let Some(env) = obj.get("env") {
                if !env.is_object() {
                    return Err("config.env must be an object {KEY: value-string}".to_string());
                }
                for (k, v) in env.as_object().unwrap() {
                    if !v.is_string() {
                        return Err(format!("config.env[{k}] must be a string"));
                    }
                }
            }
            Ok(())
        }
        _ => Err(format!("validation not implemented for kind '{kind}'")),
    }
}

/// Slugify the connection name using the same rules as roy_agents.
pub fn slugify(name: &str) -> String {
    roy_agents::slugify(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_unknown_kind() {
        assert!(validate_kind("nango").is_err());
        assert!(validate_kind("mcp_http").is_err());
        assert!(validate_kind(KIND_MCP_STDIO).is_ok());
    }

    #[test]
    fn rejects_missing_command() {
        let err = validate_config(KIND_MCP_STDIO, &json!({})).unwrap_err();
        assert!(err.contains("command"), "{err}");
    }

    #[test]
    fn accepts_minimal_stdio() {
        validate_config(KIND_MCP_STDIO, &json!({"command": "npx"})).unwrap();
    }

    #[test]
    fn rejects_non_string_env() {
        let err =
            validate_config(KIND_MCP_STDIO, &json!({"command": "x", "env": {"K": 1}})).unwrap_err();
        assert!(err.contains("env"), "{err}");
    }

    #[test]
    fn now() {
        let now = Utc::now().timestamp();
        assert!(now > 0);
    }
}
```

- [ ] **Step 2: Run the unit tests**

Run: `cargo test -p roy-management --lib connections::`
Expected: PASS — `rejects_unknown_kind`, `rejects_missing_command`, `accepts_minimal_stdio`, `rejects_non_string_env`, `now`.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-management/src/lib.rs crates/roy-management/src/connections.rs
git commit -m "feat(roy-management): Connection types + kind/config validators"
```

### Task A3: `connections::Store` — CRUD

**Files:**
- Modify: `crates/roy-management/src/connections.rs`

- [ ] **Step 1: Add Store impl**

Append to `crates/roy-management/src/connections.rs`:

```rust
// ---------------- Store ----------------

use chrono::Utc;
use sqlx::SqlitePool;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("connection not found: {0}")]
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

impl Store {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Insert a new connection for `owner_id`. The slug is derived from `name`
    /// and made unique per-owner by suffixing.
    pub async fn create(&self, owner_id: &str, new: NewConnection) -> Result<Connection, StoreError> {
        validate_kind(&new.kind).map_err(StoreError::Invalid)?;
        validate_config(&new.kind, &new.config).map_err(StoreError::Invalid)?;
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().timestamp();
        let base = slugify(&new.name);
        loop {
            let slug = self.unique_slug(owner_id, &base).await?;
            let cfg_text = serde_json::to_string(&new.config).map_err(|e| {
                StoreError::Invalid(format!("config not serializable: {e}"))
            })?;
            let secrets_text = match &new.secrets {
                Some(v) => Some(serde_json::to_string(v).map_err(|e| {
                    StoreError::Invalid(format!("secrets not serializable: {e}"))
                })?),
                None => None,
            };
            let res = sqlx::query(
                "INSERT INTO connections
                 (id, owner_id, name, slug, kind, config_json, secrets_json, description, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(owner_id)
            .bind(&new.name)
            .bind(&slug)
            .bind(&new.kind)
            .bind(&cfg_text)
            .bind(secrets_text.as_deref())
            .bind(new.description.as_deref())
            .bind(now)
            .bind(now)
            .execute(&self.pool)
            .await;
            match res {
                Ok(_) => {
                    return Ok(Connection {
                        id,
                        owner_id: owner_id.to_string(),
                        name: new.name,
                        slug,
                        kind: new.kind,
                        config: new.config,
                        secrets: new.secrets,
                        description: new.description,
                        created_at: now,
                        updated_at: now,
                    });
                }
                Err(sqlx::Error::Database(d)) if d.is_unique_violation() => continue,
                Err(e) => return Err(StoreError::Db(e)),
            }
        }
    }

    pub async fn list_by_owner(&self, owner_id: &str) -> Result<Vec<Connection>, StoreError> {
        let rows: Vec<(String, String, String, String, String, String, Option<String>, Option<String>, i64, i64)> = sqlx::query_as(
            "SELECT id, owner_id, name, slug, kind, config_json, secrets_json, description, created_at, updated_at
             FROM connections WHERE owner_id = ? ORDER BY created_at DESC",
        )
        .bind(owner_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(row_to_connection).collect()
    }

    pub async fn get(&self, owner_id: &str, id: &str) -> Result<Connection, StoreError> {
        let row: Option<(String, String, String, String, String, String, Option<String>, Option<String>, i64, i64)> = sqlx::query_as(
            "SELECT id, owner_id, name, slug, kind, config_json, secrets_json, description, created_at, updated_at
             FROM connections WHERE owner_id = ? AND id = ?",
        )
        .bind(owner_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.ok_or_else(|| StoreError::NotFound(id.to_string()))
            .and_then(row_to_connection)
    }

    /// Resolve a batch of ids belonging to `owner_id`. Unknown ids produce
    /// `StoreError::NotFound` with the first missing id.
    pub async fn get_many(&self, owner_id: &str, ids: &[String]) -> Result<Vec<Connection>, StoreError> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            out.push(self.get(owner_id, id).await?);
        }
        Ok(out)
    }

    pub async fn update(&self, owner_id: &str, id: &str, upd: ConnectionUpdate) -> Result<Connection, StoreError> {
        let current = self.get(owner_id, id).await?;
        let name = upd.name.clone().unwrap_or(current.name.clone());
        let config = upd.config.clone().unwrap_or(current.config.clone());
        validate_config(&current.kind, &config).map_err(StoreError::Invalid)?;
        let secrets = match upd.secrets {
            Some(Some(v)) => Some(v),
            Some(None) => None,
            None => current.secrets.clone(),
        };
        let description = match upd.description {
            Some(Some(s)) => Some(s),
            Some(None) => None,
            None => current.description.clone(),
        };
        let now = Utc::now().timestamp();
        let cfg_text = serde_json::to_string(&config).map_err(|e| StoreError::Invalid(format!("config not serializable: {e}")))?;
        let secrets_text = match &secrets {
            Some(v) => Some(serde_json::to_string(v).map_err(|e| StoreError::Invalid(format!("secrets not serializable: {e}")))?),
            None => None,
        };
        sqlx::query(
            "UPDATE connections SET name = ?, config_json = ?, secrets_json = ?, description = ?, updated_at = ?
             WHERE owner_id = ? AND id = ?",
        )
        .bind(&name)
        .bind(&cfg_text)
        .bind(secrets_text.as_deref())
        .bind(description.as_deref())
        .bind(now)
        .bind(owner_id)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(Connection {
            updated_at: now,
            name,
            config,
            secrets,
            description,
            ..current
        })
    }

    pub async fn delete(&self, owner_id: &str, id: &str) -> Result<(), StoreError> {
        let res = sqlx::query("DELETE FROM connections WHERE owner_id = ? AND id = ?")
            .bind(owner_id)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(StoreError::NotFound(id.to_string()));
        }
        Ok(())
    }

    async fn unique_slug(&self, owner_id: &str, base: &str) -> Result<String, StoreError> {
        let mut candidate = base.to_string();
        let mut n = 2;
        loop {
            let exists: Option<(i64,)> = sqlx::query_as(
                "SELECT 1 FROM connections WHERE owner_id = ? AND slug = ? LIMIT 1",
            )
            .bind(owner_id)
            .bind(&candidate)
            .fetch_optional(&self.pool)
            .await?;
            if exists.is_none() {
                return Ok(candidate);
            }
            candidate = format!("{base}-{n}");
            n += 1;
        }
    }
}

fn row_to_connection(
    r: (String, String, String, String, String, String, Option<String>, Option<String>, i64, i64),
) -> Result<Connection, StoreError> {
    let (id, owner_id, name, slug, kind, config_json, secrets_json, description, created_at, updated_at) = r;
    let config: Value = serde_json::from_str(&config_json)
        .map_err(|e| StoreError::Invalid(format!("config_json corrupt: {e}")))?;
    let secrets = match secrets_json {
        Some(s) => Some(
            serde_json::from_str::<Value>(&s)
                .map_err(|e| StoreError::Invalid(format!("secrets_json corrupt: {e}")))?,
        ),
        None => None,
    };
    Ok(Connection {
        id,
        owner_id,
        name,
        slug,
        kind,
        config,
        secrets,
        description,
        created_at,
        updated_at,
    })
}
```

- [ ] **Step 2: Add Store unit tests at the bottom of the file**

```rust
#[cfg(test)]
mod store_tests {
    use super::*;
    use roy_auth::test_support::{make_user, temp_pool};
    use serde_json::json;

    #[tokio::test]
    async fn create_list_get_update_delete() {
        let pool = temp_pool().await;
        let user = make_user(&pool, "alice").await;
        let store = Store::new(pool.clone());

        let c = store
            .create(
                &user.id,
                NewConnection {
                    name: "My Linear".into(),
                    kind: KIND_MCP_STDIO.into(),
                    config: json!({"command": "npx", "args": ["-y", "@linear/mcp"]}),
                    secrets: Some(json!({"LINEAR_API_KEY": "lin_xxx"})),
                    description: Some("work".into()),
                },
            )
            .await
            .unwrap();
        assert_eq!(c.slug, "my-linear");

        let listed = store.list_by_owner(&user.id).await.unwrap();
        assert_eq!(listed.len(), 1);

        let got = store.get(&user.id, &c.id).await.unwrap();
        assert_eq!(got.id, c.id);

        let upd = store
            .update(
                &user.id,
                &c.id,
                ConnectionUpdate {
                    description: Some(Some("personal".into())),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(upd.description.as_deref(), Some("personal"));

        store.delete(&user.id, &c.id).await.unwrap();
        assert!(store.list_by_owner(&user.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn slug_collisions_get_suffixed() {
        let pool = temp_pool().await;
        let user = make_user(&pool, "alice").await;
        let store = Store::new(pool.clone());
        let a = store
            .create(&user.id, NewConnection {
                name: "Linear".into(),
                kind: KIND_MCP_STDIO.into(),
                config: json!({"command": "npx"}),
                secrets: None,
                description: None,
            })
            .await
            .unwrap();
        let b = store
            .create(&user.id, NewConnection {
                name: "Linear".into(),
                kind: KIND_MCP_STDIO.into(),
                config: json!({"command": "npx"}),
                secrets: None,
                description: None,
            })
            .await
            .unwrap();
        assert_eq!(a.slug, "linear");
        assert_eq!(b.slug, "linear-2");
    }

    #[tokio::test]
    async fn one_owner_cannot_see_another_users_connections() {
        let pool = temp_pool().await;
        let alice = make_user(&pool, "alice").await;
        let bob = make_user(&pool, "bob").await;
        let store = Store::new(pool.clone());
        store
            .create(&alice.id, NewConnection {
                name: "L".into(),
                kind: KIND_MCP_STDIO.into(),
                config: json!({"command": "npx"}),
                secrets: None,
                description: None,
            })
            .await
            .unwrap();
        assert!(store.list_by_owner(&bob.id).await.unwrap().is_empty());
    }
}
```

- [ ] **Step 3: Run the Store tests**

Run: `cargo test -p roy-management --lib connections::store_tests`
Expected: PASS — `create_list_get_update_delete`, `slug_collisions_get_suffixed`, `one_owner_cannot_see_another_users_connections`.

- [ ] **Step 4: Commit**

```bash
git add crates/roy-management/src/connections.rs
git commit -m "feat(roy-management): Connection store with per-owner slug uniqueness"
```

### Task A4: HTTP CRUD routes

**Files:**
- Modify: `crates/roy-management/src/connections.rs` (append handlers)
- Modify: `crates/roy-management/src/state.rs` (add `connection_store`)
- Modify: `crates/roy-management/src/http.rs` (mount routes)
- Modify: `crates/roy-management/src/lib.rs` (init `connection_store` in `AppState`)

- [ ] **Step 1: Extend `AppState` with `connection_store`**

In `crates/roy-management/src/state.rs`, add:

```rust
pub connections: crate::connections::Store,
```

In `crates/roy-management/src/lib.rs`, where `AppState` is constructed (look for `meta_store::MetaStore::new` — same place), add:

```rust
connections: crate::connections::Store::new(pool.clone()),
```

- [ ] **Step 2: Append HTTP handlers to `connections.rs`**

```rust
// ---------------- HTTP ----------------

use axum::{
    extract::{Path as AxPath, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use crate::auth::AuthUser;
use crate::state::AppState;
use serde_json::json;

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound(id) => ApiError(StatusCode::NOT_FOUND, format!("connection not found: {id}")),
            StoreError::Invalid(msg) => ApiError(StatusCode::BAD_REQUEST, msg),
            StoreError::Db(e) => {
                tracing::error!(error = %e, "connection store db error");
                ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
            }
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/connections", get(list_handler).post(create_handler))
        .route(
            "/connections/{id}",
            get(get_handler).put(update_handler).delete(delete_handler),
        )
}

async fn list_handler(
    axum::extract::Extension(AuthUser(uid)): axum::extract::Extension<AuthUser>,
    State(s): State<AppState>,
) -> Result<Json<Vec<Connection>>, ApiError> {
    Ok(Json(s.connections.list_by_owner(&uid).await?))
}

async fn create_handler(
    axum::extract::Extension(AuthUser(uid)): axum::extract::Extension<AuthUser>,
    State(s): State<AppState>,
    Json(new): Json<NewConnection>,
) -> Result<(StatusCode, Json<Connection>), ApiError> {
    let c = s.connections.create(&uid, new).await?;
    Ok((StatusCode::CREATED, Json(c)))
}

async fn get_handler(
    axum::extract::Extension(AuthUser(uid)): axum::extract::Extension<AuthUser>,
    State(s): State<AppState>,
    AxPath(id): AxPath<String>,
) -> Result<Json<Connection>, ApiError> {
    Ok(Json(s.connections.get(&uid, &id).await?))
}

async fn update_handler(
    axum::extract::Extension(AuthUser(uid)): axum::extract::Extension<AuthUser>,
    State(s): State<AppState>,
    AxPath(id): AxPath<String>,
    Json(upd): Json<ConnectionUpdate>,
) -> Result<Json<Connection>, ApiError> {
    Ok(Json(s.connections.update(&uid, &id, upd).await?))
}

async fn delete_handler(
    axum::extract::Extension(AuthUser(uid)): axum::extract::Extension<AuthUser>,
    State(s): State<AppState>,
    AxPath(id): AxPath<String>,
) -> Result<StatusCode, ApiError> {
    s.connections.delete(&uid, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}
```

- [ ] **Step 3: Mount the connections router into the protected stack**

In `crates/roy-management/src/http.rs`, inside `fn router()`, change the `protected` builder to merge connections router. Find the line `.merge(auth::protected_router())` and add right above it:

```rust
        .merge(crate::connections::router())
```

- [ ] **Step 4: Write an HTTP integration test**

Create `crates/roy-management/tests/connections_http.rs`:

```rust
//! HTTP CRUD for /connections. Reuses the same test harness pattern as
//! crates/roy-management/tests/auth_flow.rs.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;

mod common;
use common::{app_with_user, login_cookie};

#[tokio::test]
async fn create_list_get_update_delete() {
    let (app, _temp, _admin, alice_pwd) = app_with_user("alice").await;
    let cookie = login_cookie(&app, "alice", &alice_pwd).await;

    // Create
    let body = json!({
        "name": "Linear",
        "kind": "mcp_stdio",
        "config": {"command": "npx", "args": ["-y", "@linear/mcp"]},
        "secrets": {"LINEAR_API_KEY": "lin_xxx"}
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/connections")
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let created: Value = serde_json::from_slice(&bytes).unwrap();
    let id = created["id"].as_str().unwrap().to_string();

    // List
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/connections")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let listed: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);

    // Update description
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/connections/{id}"))
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .body(Body::from(json!({"description": "personal"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Delete
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/connections/{id}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn unauthenticated_calls_get_401() {
    let (app, _temp, _admin, _pwd) = app_with_user("alice").await;
    let resp = app
        .oneshot(Request::builder().uri("/connections").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn cross_user_isolation() {
    let (app, temp, _admin, alice_pwd) = app_with_user("alice").await;
    let bob_pwd = common::add_user(&temp, "bob").await;
    let alice_cookie = login_cookie(&app, "alice", &alice_pwd).await;
    let bob_cookie = login_cookie(&app, "bob", &bob_pwd).await;

    // Alice creates
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/connections")
                .header("content-type", "application/json")
                .header("cookie", &alice_cookie)
                .body(Body::from(json!({
                    "name": "L", "kind": "mcp_stdio",
                    "config": {"command": "npx"}
                }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let created: Value = serde_json::from_slice(&bytes).unwrap();
    let id = created["id"].as_str().unwrap();

    // Bob cannot see it
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/connections/{id}"))
                .header("cookie", &bob_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
```

If `crates/roy-management/tests/common/mod.rs` doesn't exist yet, copy the helper pattern from `tests/auth_flow.rs` — `app_with_user`, `login_cookie`, `add_user`. If it does exist, just add `add_user` if missing (it returns the user's password).

- [ ] **Step 5: Run the integration test**

Run: `cargo test -p roy-management --test connections_http`
Expected: PASS — three tests.

- [ ] **Step 6: Commit**

```bash
git add crates/roy-management/src/state.rs crates/roy-management/src/lib.rs \
        crates/roy-management/src/connections.rs crates/roy-management/src/http.rs \
        crates/roy-management/tests/connections_http.rs
git commit -m "feat(roy-management): /connections CRUD HTTP routes"
```

---

## Phase B — `ConnectionSpec` wire type + `Spawn` extension

End state: the daemon's `ClientCommand::Spawn` carries an inline `connections: Vec<ConnectionSpec>`. Each `ConnectionSpec` is self-contained (slug + transport + creds) — daemon never reads the DB. Wire roundtrips and existing tests still pass. No injection logic yet.

### Task B1: `ConnectionSpec` type in `roy::control`

**Files:**
- Modify: `crates/roy/src/control.rs`
- Modify: `crates/roy/src/lib.rs`

- [ ] **Step 1: Add the type and extend `Spawn`**

In `crates/roy/src/control.rs`, add near the top of the file (after the existing `use` block and before `pub enum ClientCommand`):

```rust
/// One MCP connection passed inline by the trigger client into `Spawn`.
/// `kind` is one of: `mcp_stdio` (MVP). The shape is intentionally generic so
/// new transports can be added without changing the daemon-side wire enum —
/// `roy mcp serve-connections` is the only consumer that interprets these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionSpec {
    pub id: String,
    pub slug: String,
    pub kind: String,
    pub config: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets: Option<serde_json::Value>,
}
```

In the same file, modify `pub enum ClientCommand::Spawn` (around line 132) by adding a new field:

```rust
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        connections: Vec<ConnectionSpec>,
```

Place it after `system_prompt` to keep the diff minimal. The full variant should now look like:

```rust
    Spawn {
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        connections: Vec<ConnectionSpec>,
    },
```

In `crates/roy/src/lib.rs`, add `ConnectionSpec` to the `pub use control::{...}` line.

- [ ] **Step 2: Update every callsite that constructs `ClientCommand::Spawn`**

Search and update:

```bash
grep -rn "ClientCommand::Spawn {" crates/ tests/
```

Add `connections: vec![],` to each existing construction. Files affected: `crates/roy/src/control.rs` (roundtrip tests), `crates/roy/src/daemon.rs` (tests at lines ~1199, ~1324, ~1390, ~1536 per earlier grep), `crates/roy-management/src/roy_client.rs`. **Do not** add a default impl on the field — being explicit at every callsite documents the intent.

- [ ] **Step 3: Run wire-protocol roundtrip tests**

Run: `cargo test -p roy control::tests::`
Expected: PASS — including the existing `Spawn` roundtrip tests.

- [ ] **Step 4: Run the workspace**

Run: `cargo test --workspace --no-fail-fast`
Expected: PASS — adding a `#[serde(default)]` field is backwards-compatible.

- [ ] **Step 5: Commit**

```bash
git add crates/roy/src/control.rs crates/roy/src/lib.rs crates/roy/src/daemon.rs \
        crates/roy-management/src/roy_client.rs
git commit -m "feat(roy): ConnectionSpec on ClientCommand::Spawn"
```

### Task B2: Plumb `connections` through `SessionSpawnConfig` and `TransportFactory`

**Files:**
- Modify: `crates/roy/src/engine.rs` (`SessionSpawnConfig`)
- Modify: `crates/roy/src/manager.rs` (factory call)
- Modify: `crates/roy/src/daemon.rs` (`handle_spawn`, `TransportFactory::build` signature)
- Modify: `crates/roy/src/transport/acp/mod.rs` (`AcpConfig::connections` field)

- [ ] **Step 1: Add `connections` to `SessionSpawnConfig`**

In `crates/roy/src/engine.rs`, add to the `pub struct SessionSpawnConfig`:

```rust
    /// MCP connections injected for this session. Empty = no MCP.
    pub connections: Vec<crate::control::ConnectionSpec>,
```

Update every place that constructs `SessionSpawnConfig` (search: `SessionSpawnConfig {`) to set `connections: vec![],` until the daemon learns to forward them.

- [ ] **Step 2: Extend `TransportFactory::build` signature**

In `crates/roy/src/daemon.rs`:

```rust
pub trait TransportFactory: Send + Sync {
    fn build(
        &self,
        agent: AgentPreset,
        model: Option<&str>,
        permission: Option<&str>,
        connections: &[crate::control::ConnectionSpec],
    ) -> Result<Arc<dyn Transport>>;
}
```

Update `DefaultTransportFactory::build`:

```rust
impl TransportFactory for DefaultTransportFactory {
    fn build(
        &self,
        agent: AgentPreset,
        _model: Option<&str>,
        permission: Option<&str>,
        connections: &[crate::control::ConnectionSpec],
    ) -> Result<Arc<dyn Transport>> {
        let mut config = match agent {
            AgentPreset::Claude => AcpConfig::claude(),
            AgentPreset::Gemini => AcpConfig::gemini(),
            AgentPreset::Opencode => AcpConfig::opencode(),
            AgentPreset::Codex => AcpConfig::codex(),
            AgentPreset::Pi => AcpConfig::pi(),
        };
        if let Some(p) = permission {
            config.permission_policy = match p {
                "allow" => PermissionPolicy::AllowAll,
                "deny" => PermissionPolicy::Deny,
                other => return Err(RoyError::Protocol(format!(
                    "permission must be 'allow' or 'deny', got '{other}'"
                ))),
            };
        }
        // MVP: only the claude preset supports MCP injection. Reject any non-
        // claude preset that arrives with non-empty connections so the user
        // sees an actionable error instead of silently-missing tools.
        if !connections.is_empty() && !matches!(agent, AgentPreset::Claude) {
            return Err(RoyError::Protocol(format!(
                "preset '{agent}' does not yet support MCP connections (MVP supports only 'claude')",
            )));
        }
        config.connections = connections.to_vec();
        Ok(Arc::new(AcpTransport::new(config)))
    }
}
```

- [ ] **Step 3: Update fake factories**

Search: `impl TransportFactory for` in `crates/roy/src/daemon.rs` and `crates/roy/src/manager.rs`. There's a `FakeFactory` in `manager.rs` and a `FakeAcpFactory` in `daemon.rs`. Add the `connections` parameter to each — they can ignore it.

- [ ] **Step 4: Add `connections` to `AcpConfig`**

In `crates/roy/src/transport/acp/mod.rs`, add to `pub struct AcpConfig`:

```rust
    /// MCP connections to inject into the agent's project-level config before
    /// spawning. Currently only `AcpConfig::claude()` opts into using this —
    /// other presets ignore it. See `mcp_injection.rs` for the writer.
    pub connections: Vec<crate::control::ConnectionSpec>,
```

Update every preset constructor (`gemini`, `opencode`, `codex`, `claude`, `pi`) to initialize `connections: Vec::new(),`.

- [ ] **Step 5: Forward connections through `handle_spawn` and `manager.spawn`**

In `crates/roy/src/daemon.rs`, modify the dispatch arm for `ClientCommand::Spawn` to pass `connections` through to `handle_spawn`, and add the parameter to `handle_spawn`:

```rust
    async fn handle_spawn(
        self: &Arc<Self>,
        agent_label: String,
        agent: AgentPreset,
        cwd: Option<PathBuf>,
        model: Option<String>,
        permission: Option<String>,
        resume: Option<String>,
        system_prompt: Option<String>,
        connections: Vec<crate::control::ConnectionSpec>,
        event_tx: &EventTx,
    ) {
        let _ = event_tx.send(ServerEvent::Spawning { agent: agent_label });
        let cfg = SessionSpawnConfig {
            agent,
            cwd,
            model,
            permission,
            resume_cursor: resume,
            fixed_session_id: None,
            system_prompt,
            connections,
        };
        match self.manager.spawn(cfg, 256, 1024).await { ... }
    }
```

And update the dispatch arm at line ~284:

```rust
        ClientCommand::Spawn {
            agent,
            cwd,
            model,
            permission,
            resume,
            system_prompt,
            connections,
        } => { ... self.handle_spawn(..., connections, event_tx).await }
```

In `crates/roy/src/manager.rs`, `spawn_internal` calls `self.factory.build(cfg.agent, cfg.model.as_deref(), cfg.permission.as_deref())` — extend that to:

```rust
        let transport = self.factory.build(
            cfg.agent,
            cfg.model.as_deref(),
            cfg.permission.as_deref(),
            &cfg.connections,
        )?;
```

The resume path (`manager::resume`) reconstructs `SessionSpawnConfig` from a `SessionRow`. For MVP, resume does NOT re-attach connections — Phase E persists `connection_ids` for audit only. Set `connections: Vec::new()` in the resume reconstruction and add a `tracing::warn!` if the original spawn had connections, so the user sees an explicit message.

- [ ] **Step 6: Forward connections through `roy-management`**

In `crates/roy-management/src/roy_client.rs`, add to `SpawnRequest`:

```rust
    pub connections: Vec<roy::ConnectionSpec>,
```

And in the `UnixSocketDaemonClient::spawn` body:

```rust
        let cmd = ClientCommand::Spawn {
            agent: req.agent,
            cwd: req.cwd,
            model: req.model,
            permission: req.permission,
            resume: None,
            system_prompt: req.system_prompt,
            connections: req.connections,
        };
```

Update `DaemonClient` trait callsites that don't yet set this — they pass `connections: vec![]`.

- [ ] **Step 7: Run the workspace**

Run: `cargo test --workspace --no-fail-fast`
Expected: PASS — all existing tests continue to work; new field defaults to empty.

- [ ] **Step 8: Commit**

```bash
git add crates/roy/src/engine.rs crates/roy/src/manager.rs crates/roy/src/daemon.rs \
        crates/roy/src/transport/acp/mod.rs crates/roy-management/src/roy_client.rs
git commit -m "feat(roy): plumb connections through SessionSpawnConfig and TransportFactory"
```

---

## Phase C — `roy mcp serve-connections` subcommand

End state: a standalone subcommand that takes a JSON file of `ConnectionSpec`s on argv (or stdin), spawns each upstream MCP as a child, and speaks JSON-RPC 2.0 on its own stdio to behave as a single MCP server with namespaced tools.

### Task C1: CLI subcommand wiring

**Files:**
- Modify: `crates/roy-mcp/src/lib.rs`
- Create: `crates/roy-mcp/src/serve_connections/mod.rs`
- Create: `crates/roy-mcp/src/serve_connections/spec.rs`
- Modify: `crates/roy-cli/src/main.rs` (or wherever `roy mcp` is dispatched)
- Modify: `crates/roy-mcp/Cargo.toml` (add `clap`)

- [ ] **Step 1: Add `clap` to `roy-mcp/Cargo.toml`**

```toml
clap = { version = "4", features = ["derive"] }
```

(Match the version `roy-cli` uses — open its Cargo.toml first to keep the version aligned.)

- [ ] **Step 2: Add the subcommand entry**

Create `crates/roy-mcp/src/serve_connections/spec.rs`:

```rust
//! Wire shape for the connections passed to `roy mcp serve-connections`.
//! Mirrors `roy::control::ConnectionSpec` but lives in roy-mcp to avoid a hard
//! dependency cycle (roy-mcp must stay buildable without spawning a daemon).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionSpec {
    pub id: String,
    pub slug: String,
    pub kind: String,
    pub config: serde_json::Value,
    #[serde(default)]
    pub secrets: Option<serde_json::Value>,
}

/// Bundle passed in via `--specs <path>` (the path holds JSON-encoded `Bundle`)
/// or `--specs-stdin` (read the JSON from stdin before switching to JSON-RPC).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    pub session_id: String,
    pub connections: Vec<ConnectionSpec>,
}
```

Create `crates/roy-mcp/src/serve_connections/mod.rs`:

```rust
//! `roy mcp serve-connections`: a proxying MCP server.
//!
//! Speaks JSON-RPC 2.0 over its own stdio (acts as the MCP server for its
//! parent — the ACP agent), and spawns each upstream MCP as a child process
//! with its own stdio pipe. `tools/list` aggregates all upstream tools with
//! a `<slug>__<tool>` prefix; `tools/call` strips the prefix and routes to
//! the owning upstream.

use anyhow::{Context, Result};
use clap::Args;
use std::path::PathBuf;

pub mod spec;
pub mod upstream;
pub mod registry;

#[derive(Args, Debug)]
pub struct ServeConnectionsArgs {
    /// Path to a JSON file containing a `Bundle` (session_id + connections).
    /// Mutually exclusive with --specs-stdin.
    #[arg(long, conflicts_with = "specs_stdin")]
    pub specs: Option<PathBuf>,
    /// Read the spec bundle as the first line on stdin before switching to
    /// JSON-RPC framing for the rest of the conversation. Use when the spec
    /// contains secrets you don't want on disk.
    #[arg(long)]
    pub specs_stdin: bool,
}

pub async fn run(args: ServeConnectionsArgs) -> Result<()> {
    let bundle = load_bundle(&args).await.context("loading spec bundle")?;
    let registry = registry::Registry::start(bundle).await?;
    crate::serve_connections::dispatch::run(registry).await
}

async fn load_bundle(args: &ServeConnectionsArgs) -> Result<spec::Bundle> {
    if let Some(path) = &args.specs {
        let text = tokio::fs::read_to_string(path).await
            .with_context(|| format!("reading {}", path.display()))?;
        Ok(serde_json::from_str(&text)?)
    } else if args.specs_stdin {
        use tokio::io::AsyncBufReadExt;
        let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
        let first = lines.next_line().await?
            .ok_or_else(|| anyhow::anyhow!("EOF before spec bundle"))?;
        Ok(serde_json::from_str(&first)?)
    } else {
        anyhow::bail!("either --specs <path> or --specs-stdin is required");
    }
}

pub mod dispatch;
```

Create `crates/roy-mcp/src/serve_connections/dispatch.rs`:

```rust
//! JSON-RPC 2.0 server loop. Speaks MCP to the parent ACP-agent process.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::registry::Registry;

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "roy-connections";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub async fn run(registry: Registry) -> Result<()> {
    let mut stdin_lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = stdin_lines.next_line().await.context("reading stdin")? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                write_response(
                    &mut stdout,
                    &error_response(Value::Null, -32700, &format!("parse error: {e}")),
                ).await?;
                continue;
            }
        };
        if let Some(resp) = handle_request(&req, &registry).await {
            write_response(&mut stdout, &resp).await?;
        }
    }
    registry.shutdown().await;
    Ok(())
}

async fn handle_request(req: &Value, registry: &Registry) -> Option<Value> {
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let id = req.get("id").cloned();
    let is_notification = id.is_none();

    match method {
        "initialize" if !is_notification => Some(initialize_result(id.unwrap_or(Value::Null))),
        "notifications/initialized" => None,
        "ping" if !is_notification => Some(ok_response(id.unwrap_or(Value::Null), json!({}))),
        "tools/list" if !is_notification => Some(ok_response(
            id.unwrap_or(Value::Null),
            json!({ "tools": registry.tools_list() }),
        )),
        "tools/call" if !is_notification => {
            let id = id.unwrap_or(Value::Null);
            let params = req.get("params").cloned().unwrap_or(json!({}));
            let result = registry.call_tool(params).await;
            match result {
                Ok(value) => Some(ok_response(id, value)),
                Err(e) => Some(error_response(id, -32000, &e.to_string())),
            }
        }
        _ if is_notification => None,
        _ => Some(error_response(
            id.unwrap_or(Value::Null),
            -32601,
            &format!("method not found: {method}"),
        )),
    }
}

fn initialize_result(id: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
        }
    })
}

fn ok_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

async fn write_response<W: AsyncWriteExt + Unpin>(out: &mut W, v: &Value) -> Result<()> {
    let s = serde_json::to_string(v)?;
    out.write_all(s.as_bytes()).await?;
    out.write_all(b"\n").await?;
    out.flush().await?;
    Ok(())
}
```

- [ ] **Step 3: Expose subcommand in `roy-mcp/src/lib.rs`**

Add to the top of `crates/roy-mcp/src/lib.rs`:

```rust
pub mod serve_connections;
```

- [ ] **Step 4: Wire `roy mcp serve-connections` in `roy-cli`**

Find where `roy mcp` is dispatched in `crates/roy-cli/src/main.rs`. Add the subcommand under it:

```rust
#[derive(clap::Subcommand)]
enum McpCmd {
    /// Original tool server (daemon control).
    Serve { ... },
    /// Proxying MCP server that aggregates user-owned upstream MCPs.
    ServeConnections(roy_mcp::serve_connections::ServeConnectionsArgs),
}
```

And in the match:

```rust
McpCmd::ServeConnections(args) => roy_mcp::serve_connections::run(args).await?,
```

- [ ] **Step 5: Add a minimal Registry stub to make it compile**

Create `crates/roy-mcp/src/serve_connections/registry.rs`:

```rust
use anyhow::Result;
use serde_json::Value;

use super::spec::Bundle;

pub struct Registry;

impl Registry {
    pub async fn start(_bundle: Bundle) -> Result<Self> {
        Ok(Self)
    }
    pub fn tools_list(&self) -> Vec<Value> {
        vec![]
    }
    pub async fn call_tool(&self, _params: Value) -> Result<Value> {
        anyhow::bail!("no upstream registered yet")
    }
    pub async fn shutdown(self) {}
}
```

And create `crates/roy-mcp/src/serve_connections/upstream.rs` empty for now (will fill in C3):

```rust
//! One upstream MCP child process.
```

- [ ] **Step 6: Verify it compiles**

Run: `cargo build -p roy-mcp -p roy-cli`
Expected: clean build.

- [ ] **Step 7: Commit**

```bash
git add crates/roy-mcp/Cargo.toml crates/roy-mcp/src/lib.rs \
        crates/roy-mcp/src/serve_connections/ \
        crates/roy-cli/src/main.rs
git commit -m "feat(roy-mcp): serve-connections subcommand skeleton"
```

### Task C2: Fake upstream MCP for tests

**Files:**
- Create: `crates/roy-mcp/tests/scripts/fake-mcp-upstream.py`

- [ ] **Step 1: Write the python fake**

```python
#!/usr/bin/env python3
"""Minimal MCP-over-stdio fake. Configurable via env:
  FAKE_TOOLS - JSON array of tool descriptors (default: one "echo" tool)
  FAKE_NAME  - server name (default: fake-upstream)
"""
import json
import os
import sys

DEFAULT_TOOLS = [
    {
        "name": "echo",
        "description": "Echo input back as text.",
        "inputSchema": {
            "type": "object",
            "properties": {"msg": {"type": "string"}},
            "required": ["msg"],
        },
    }
]

def main():
    tools = json.loads(os.environ.get("FAKE_TOOLS", json.dumps(DEFAULT_TOOLS)))
    name = os.environ.get("FAKE_NAME", "fake-upstream")
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError:
            continue
        method = req.get("method", "")
        rid = req.get("id")
        if method == "initialize":
            resp = {
                "jsonrpc": "2.0", "id": rid,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": name, "version": "0"},
                },
            }
        elif method == "notifications/initialized":
            continue
        elif method == "tools/list":
            resp = {"jsonrpc": "2.0", "id": rid, "result": {"tools": tools}}
        elif method == "tools/call":
            params = req.get("params", {})
            tool = params.get("name", "")
            args = params.get("arguments", {})
            if tool == "echo":
                resp = {"jsonrpc": "2.0", "id": rid, "result": {
                    "content": [{"type": "text", "text": str(args.get("msg", ""))}],
                    "isError": False,
                }}
            else:
                resp = {"jsonrpc": "2.0", "id": rid,
                        "error": {"code": -32602, "message": f"unknown tool {tool}"}}
        elif rid is None:
            continue
        else:
            resp = {"jsonrpc": "2.0", "id": rid,
                    "error": {"code": -32601, "message": f"method not found: {method}"}}
        sys.stdout.write(json.dumps(resp) + "\n")
        sys.stdout.flush()

if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Make it executable**

```bash
chmod +x crates/roy-mcp/tests/scripts/fake-mcp-upstream.py
```

- [ ] **Step 3: Smoke test by hand**

```bash
echo '{"jsonrpc":"2.0","id":1,"method":"initialize"}' | python3 crates/roy-mcp/tests/scripts/fake-mcp-upstream.py
```
Expected: one line of JSON with `result.protocolVersion == "2024-11-05"`.

- [ ] **Step 4: Commit**

```bash
git add crates/roy-mcp/tests/scripts/fake-mcp-upstream.py
git commit -m "test(roy-mcp): fake stdio MCP upstream for tests"
```

### Task C3: `upstream::Upstream` — child process + JSON-RPC client

**Files:**
- Modify: `crates/roy-mcp/src/serve_connections/upstream.rs`

- [ ] **Step 1: Implement the upstream child wrapper**

```rust
//! One upstream MCP child process. Speaks JSON-RPC 2.0 over the child's
//! stdin/stdout. Stateless except for an autoincrementing request id.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, Mutex};

use super::spec::ConnectionSpec;

pub struct Upstream {
    pub slug: String,
    child: Mutex<Child>,
    writer: Mutex<tokio::process::ChildStdin>,
    next_id: Mutex<i64>,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>,
    /// Cached tool list captured at startup.
    pub tools: Vec<Value>,
}

impl Upstream {
    pub async fn start(spec: &ConnectionSpec) -> Result<Self> {
        if spec.kind != "mcp_stdio" {
            return Err(anyhow!("upstream kind '{}' not supported (mcp_stdio only)", spec.kind));
        }
        let cfg = &spec.config;
        let command = cfg.get("command").and_then(Value::as_str)
            .ok_or_else(|| anyhow!("connection '{}': config.command missing", spec.slug))?;
        let args: Vec<String> = cfg.get("args")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let env_pairs: Vec<(String, String)> = cfg.get("env")
            .and_then(Value::as_object)
            .map(|o| o.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
            .unwrap_or_default();
        let secret_env: Vec<(String, String)> = spec.secrets.as_ref()
            .and_then(Value::as_object)
            .map(|o| o.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
            .unwrap_or_default();

        let mut cmd = Command::new(command);
        cmd.args(&args)
            .envs(env_pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .envs(secret_env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);

        let mut child = cmd.spawn()
            .with_context(|| format!("spawning upstream '{}': {}", spec.slug, command))?;
        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");

        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>> = Arc::new(Mutex::new(HashMap::new()));
        let pending_for_reader = Arc::clone(&pending);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let v: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let Some(id) = v.get("id").and_then(Value::as_i64) {
                    if let Some(tx) = pending_for_reader.lock().await.remove(&id) {
                        let _ = tx.send(v);
                    }
                }
                // Notifications are dropped (MVP: no tools/list_changed propagation).
            }
        });

        let mut up = Upstream {
            slug: spec.slug.clone(),
            child: Mutex::new(child),
            writer: Mutex::new(stdin),
            next_id: Mutex::new(1),
            pending,
            tools: Vec::new(),
        };

        up.request("initialize", json!({"protocolVersion": "2024-11-05",
                                         "capabilities": {},
                                         "clientInfo": {"name": "roy-connections", "version": env!("CARGO_PKG_VERSION")}}))
            .await
            .with_context(|| format!("initialize '{}'", spec.slug))?;
        up.notify("notifications/initialized", json!({})).await?;
        let tools_resp = up.request("tools/list", json!({})).await
            .with_context(|| format!("tools/list '{}'", spec.slug))?;
        up.tools = tools_resp
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(up)
    }

    pub async fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<Value> {
        self.request("tools/call", json!({"name": tool_name, "arguments": arguments})).await
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = {
            let mut n = self.next_id.lock().await;
            let cur = *n;
            *n += 1;
            cur
        };
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.write_line(&req).await?;
        let resp = rx.await.context("upstream closed before responding")?;
        if let Some(err) = resp.get("error") {
            return Err(anyhow!("upstream error: {}", err));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let req = json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.write_line(&req).await
    }

    async fn write_line(&self, v: &Value) -> Result<()> {
        let s = serde_json::to_string(v)?;
        let mut w = self.writer.lock().await;
        w.write_all(s.as_bytes()).await?;
        w.write_all(b"\n").await?;
        w.flush().await?;
        Ok(())
    }

    pub async fn shutdown(self) {
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p roy-mcp`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-mcp/src/serve_connections/upstream.rs
git commit -m "feat(roy-mcp): Upstream wrapper for one stdio MCP child"
```

### Task C4: `Registry` — aggregate + route

**Files:**
- Modify: `crates/roy-mcp/src/serve_connections/registry.rs`

- [ ] **Step 1: Replace the stub Registry**

```rust
//! Registry of started upstreams. Owns tool aggregation and tool-call routing.

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use super::spec::{Bundle, ConnectionSpec};
use super::upstream::Upstream;

pub struct Registry {
    upstreams: HashMap<String, Arc<Upstream>>,
    /// `<slug>__<tool>` -> upstream slug. Avoids splitting the prefixed name
    /// on every call.
    routes: HashMap<String, (String, String)>,
}

impl Registry {
    pub async fn start(bundle: Bundle) -> Result<Self> {
        let mut upstreams: HashMap<String, Arc<Upstream>> = HashMap::new();
        let mut routes: HashMap<String, (String, String)> = HashMap::new();
        for spec in &bundle.connections {
            let up = match Upstream::start(spec).await {
                Ok(u) => Arc::new(u),
                Err(e) => {
                    // Don't kill the whole session for one bad upstream — log
                    // and continue. The agent will simply not see its tools.
                    tracing::warn!(slug = %spec.slug, error = %e, "upstream failed to start");
                    continue;
                }
            };
            for tool in &up.tools {
                let name = tool.get("name").and_then(Value::as_str).unwrap_or("");
                if name.is_empty() {
                    continue;
                }
                let prefixed = format!("{}__{}", spec.slug, name);
                routes.insert(prefixed, (spec.slug.clone(), name.to_string()));
            }
            upstreams.insert(spec.slug.clone(), up);
        }
        Ok(Self { upstreams, routes })
    }

    pub fn tools_list(&self) -> Vec<Value> {
        let mut out = Vec::new();
        for (slug, up) in &self.upstreams {
            for tool in &up.tools {
                if let Some(obj) = tool.as_object() {
                    let mut prefixed = obj.clone();
                    let original = obj.get("name").and_then(Value::as_str).unwrap_or("");
                    prefixed.insert(
                        "name".into(),
                        Value::String(format!("{slug}__{original}")),
                    );
                    out.push(Value::Object(prefixed));
                }
            }
        }
        out
    }

    pub async fn call_tool(&self, params: Value) -> Result<Value> {
        let name = params.get("name").and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing tool name"))?;
        let arguments = params.get("arguments").cloned().unwrap_or(Value::Object(Default::default()));
        let (slug, original) = self.routes.get(name)
            .ok_or_else(|| anyhow!("unknown tool '{name}'"))?;
        let up = self.upstreams.get(slug)
            .ok_or_else(|| anyhow!("upstream '{slug}' is gone"))?;
        up.call_tool(original, arguments).await
    }

    pub async fn shutdown(self) {
        for (_, up) in self.upstreams {
            // Each Arc::try_unwrap should normally succeed (registry is the
            // only owner once dispatch loop exits), but if a slow tools/call
            // is still in flight, fall through and let `kill_on_drop` clean
            // up when the Arc drops.
            if let Ok(up) = Arc::try_unwrap(up) {
                up.shutdown().await;
            }
        }
    }
}
```

- [ ] **Step 2: Add an integration test**

Create `crates/roy-mcp/tests/serve_connections.rs`:

```rust
//! End-to-end stdio test of `roy mcp serve-connections`.
//!
//! Drives the binary directly via tokio::process and speaks MCP JSON-RPC to it
//! the same way the ACP agent would.

use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

fn fake_upstream_path() -> String {
    let crate_root = env!("CARGO_MANIFEST_DIR");
    format!("{crate_root}/tests/scripts/fake-mcp-upstream.py")
}

fn bin_path() -> String {
    // cargo test sets CARGO_BIN_EXE_<name> when the test crate depends on the
    // binary. roy-mcp itself doesn't build a binary — roy-cli does. Use the
    // workspace target dir.
    let exe = env!("CARGO_BIN_EXE_roy");
    exe.to_string()
}

async fn proto_send(stdin: &mut tokio::process::ChildStdin, v: &Value) {
    let s = serde_json::to_string(v).unwrap();
    stdin.write_all(s.as_bytes()).await.unwrap();
    stdin.write_all(b"\n").await.unwrap();
    stdin.flush().await.unwrap();
}

async fn proto_recv(stdout: &mut BufReader<tokio::process::ChildStdout>) -> Value {
    let mut line = String::new();
    stdout.read_line(&mut line).await.unwrap();
    serde_json::from_str(line.trim()).unwrap()
}

#[tokio::test]
async fn aggregates_and_proxies_one_upstream() {
    let bundle = json!({
        "session_id": "test-session",
        "connections": [
            {
                "id": "conn-1",
                "slug": "fake",
                "kind": "mcp_stdio",
                "config": {
                    "command": "python3",
                    "args": [fake_upstream_path()]
                }
            }
        ]
    });

    let mut child = Command::new(bin_path())
        .args(["mcp", "serve-connections", "--specs-stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // 1. Send the bundle.
    stdin.write_all(bundle.to_string().as_bytes()).await.unwrap();
    stdin.write_all(b"\n").await.unwrap();
    stdin.flush().await.unwrap();

    // 2. Initialize.
    proto_send(&mut stdin, &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"})).await;
    let resp = proto_recv(&mut stdout).await;
    assert_eq!(resp["id"], json!(1));
    assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");

    // 3. List tools.
    proto_send(&mut stdin, &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).await;
    let resp = proto_recv(&mut stdout).await;
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1, "expected one tool, got: {tools:?}");
    assert_eq!(tools[0]["name"], "fake__echo");

    // 4. Call the tool.
    proto_send(&mut stdin, &json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "fake__echo", "arguments": {"msg": "hello"}}
    })).await;
    let resp = proto_recv(&mut stdout).await;
    assert_eq!(resp["result"]["content"][0]["text"], "hello");

    child.kill().await.unwrap();
}
```

- [ ] **Step 3: Run the integration test**

Run: `cargo test -p roy-mcp --test serve_connections -- --nocapture`
Expected: PASS — three assertions in one test.

- [ ] **Step 4: Commit**

```bash
git add crates/roy-mcp/src/serve_connections/registry.rs \
        crates/roy-mcp/tests/serve_connections.rs
git commit -m "feat(roy-mcp): aggregate tools and proxy tools/call across upstreams"
```

---

## Phase D — `.mcp.json` injection for the claude preset

End state: when the daemon spawns `claude-code-acp` with non-empty `connections`, it writes a `.mcp.json` to the session cwd that registers `roy-connections` as the only MCP server. Claude Code reads this on launch and presents the aggregated tools to the agent.

### Task D1: `mcp_injection.rs` — build `.mcp.json` body

**Files:**
- Create: `crates/roy/src/transport/acp/mcp_injection.rs`
- Modify: `crates/roy/src/transport/acp/mod.rs` (import + use)

- [ ] **Step 1: Write the helper**

```rust
//! Build the `.mcp.json` Claude Code project-config that points the agent at
//! our `roy mcp serve-connections` proxy. Bundle of `ConnectionSpec`s is
//! passed to the proxy via stdin (no on-disk file, so secrets never touch the
//! filesystem inside the session cwd).

use crate::control::ConnectionSpec;
use anyhow::Result;
use serde_json::{json, Value};

/// Path under cwd where Claude Code looks for project-level MCP config.
pub const MCP_CONFIG_FILENAME: &str = ".mcp.json";

/// Bundle written to a sibling file (not under cwd) and piped into the proxy
/// at startup via `--specs <path>`. Keeping the secrets out of the project
/// cwd prevents leakage into the agent's tool sandbox / git status / etc.
pub fn build_mcp_config(roy_binary: &str, bundle_path: &std::path::Path) -> Value {
    json!({
        "mcpServers": {
            "roy-connections": {
                "command": roy_binary,
                "args": ["mcp", "serve-connections", "--specs", bundle_path.to_string_lossy()],
            }
        }
    })
}

pub fn build_bundle(session_id: &str, connections: &[ConnectionSpec]) -> Value {
    json!({
        "session_id": session_id,
        "connections": connections,
    })
}

/// Pick the executable the daemon should hand to Claude Code. Honors
/// `ROY_BIN` for tests; otherwise defaults to `roy` (assumes on PATH).
pub fn roy_binary_path() -> String {
    std::env::var("ROY_BIN").unwrap_or_else(|_| "roy".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn config_shape() {
        let v = build_mcp_config("/usr/local/bin/roy", &PathBuf::from("/tmp/bundle.json"));
        assert_eq!(v["mcpServers"]["roy-connections"]["command"], "/usr/local/bin/roy");
        let args = v["mcpServers"]["roy-connections"]["args"].as_array().unwrap();
        assert_eq!(args[0], "mcp");
        assert_eq!(args[1], "serve-connections");
        assert_eq!(args[2], "--specs");
        assert_eq!(args[3], "/tmp/bundle.json");
    }

    #[test]
    fn bundle_includes_secrets() {
        let specs = vec![ConnectionSpec {
            id: "id1".into(),
            slug: "linear".into(),
            kind: "mcp_stdio".into(),
            config: json!({"command": "npx"}),
            secrets: Some(json!({"LINEAR_API_KEY": "lin_xxx"})),
        }];
        let bundle = build_bundle("sess-1", &specs);
        assert_eq!(bundle["connections"][0]["secrets"]["LINEAR_API_KEY"], "lin_xxx");
    }
}
```

- [ ] **Step 2: Run the unit tests**

Run: `cargo test -p roy --lib transport::acp::mcp_injection::`
Expected: PASS — `config_shape`, `bundle_includes_secrets`.

- [ ] **Step 3: Commit**

```bash
git add crates/roy/src/transport/acp/mcp_injection.rs
git commit -m "feat(roy): build_mcp_config / build_bundle helpers"
```

### Task D2: Wire injection into `AcpTransport::open`

**Files:**
- Modify: `crates/roy/src/transport/acp/mod.rs`

- [ ] **Step 1: Declare the module**

In `crates/roy/src/transport/acp/mod.rs`, near the top:

```rust
pub mod mcp_injection;
```

- [ ] **Step 2: Add an `inject_mcp` flag to `AcpConfig`**

Below the existing `AcpConfig` fields, add:

```rust
    /// When true, the transport materializes a `.mcp.json` in cwd and a sibling
    /// bundle file in a temp dir before spawning the child, so the agent's
    /// project-level MCP config points at `roy mcp serve-connections`. Only
    /// honored by presets whose underlying CLI reads `<cwd>/.mcp.json`
    /// (currently: claude-code-acp).
    pub inject_mcp: bool,
```

In `AcpConfig::claude()`, set `inject_mcp: true`. In every other preset constructor, set `inject_mcp: false`.

- [ ] **Step 3: Inject before spawning the child**

In `AcpTransport::open`, locate the section where the child is spawned (the `let mut cmd = Command::new(&self.config.command);` block — around line 202).

Before spawning, add the injection block:

```rust
        // MCP injection: write project-level config + sibling bundle. Bundle
        // lives in a temp dir so secrets stay out of cwd (which the agent can
        // read with file tools). The .mcp.json itself is benign — it just
        // points at our proxy.
        let _injection_guard = if self.config.inject_mcp && !self.config.connections.is_empty() {
            let bundle = mcp_injection::build_bundle(_session_id, &self.config.connections);
            let bundle_path = std::env::temp_dir()
                .join(format!("roy-mcp-bundle-{_session_id}.json"));
            std::fs::write(&bundle_path, serde_json::to_vec(&bundle).map_err(|e| {
                RoyError::Protocol(format!("bundle serialize: {e}"))
            })?).map_err(RoyError::Io)?;
            let mcp_cfg = mcp_injection::build_mcp_config(
                &mcp_injection::roy_binary_path(),
                &bundle_path,
            );
            let mcp_cfg_path = cwd.join(mcp_injection::MCP_CONFIG_FILENAME);
            std::fs::write(&mcp_cfg_path, serde_json::to_vec_pretty(&mcp_cfg).map_err(|e| {
                RoyError::Protocol(format!("mcp config serialize: {e}"))
            })?).map_err(RoyError::Io)?;
            // Guard cleans up the bundle (which holds secrets) when the
            // session closes. Cwd's .mcp.json is left in place — the user's
            // cwd is theirs to manage.
            Some(BundleGuard { path: bundle_path })
        } else {
            None
        };
```

Add `BundleGuard` near the top of the module:

```rust
struct BundleGuard {
    path: PathBuf,
}
impl Drop for BundleGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
```

The `_injection_guard` must outlive the spawn — keep it bound in scope until the function returns. Since `AcpTransport::open` returns a `Box<dyn Handle>`, hand the guard off to the handle so it's dropped only when the session closes. Search for `impl Handle for AcpHandle` (or whatever the concrete handle is called) and add a `_mcp_bundle_guard: Option<BundleGuard>` field. Pass it through.

- [ ] **Step 4: Write a unit test for the injection**

Add at the bottom of `crates/roy/src/transport/acp/mod.rs` `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn claude_preset_has_inject_mcp_on_by_default() {
        assert!(AcpConfig::claude().inject_mcp, "claude must opt into MCP injection");
        assert!(!AcpConfig::gemini().inject_mcp);
        assert!(!AcpConfig::opencode().inject_mcp);
        assert!(!AcpConfig::codex().inject_mcp);
        assert!(!AcpConfig::pi().inject_mcp);
    }
```

- [ ] **Step 5: Verify**

Run: `cargo test -p roy --lib transport::acp::`
Expected: PASS — including the new injection-flag test.

Run: `cargo test --workspace --no-fail-fast`
Expected: PASS — no regressions in fake-agent tests.

- [ ] **Step 6: Commit**

```bash
git add crates/roy/src/transport/acp/mod.rs
git commit -m "feat(roy-acp): write .mcp.json + bundle when claude preset has connections"
```

---

## Phase E — `roy-management` session creation accepts `connection_ids`

End state: `POST /sessions` accepts `connection_ids: [...]`, the handler resolves them to specs via the user-scoped store, forwards to the daemon, and records the ids on `session_meta`.

### Task E1: Persist `connection_ids` on `SessionMeta`

**Files:**
- Modify: `crates/roy-management/src/meta_store.rs`

- [ ] **Step 1: Extend `SessionMeta` struct**

Find `pub struct SessionMeta` and add:

```rust
    pub connection_ids: Vec<String>,
```

- [ ] **Step 2: Update SQL writes and reads**

In `meta_store::upsert_session_meta` (and `update`/`get`), include the new column. The column already exists thanks to the migration in Task A1; we just need to round-trip it as JSON.

For the INSERT/UPDATE binding:

```rust
let ids_json = serde_json::to_string(&meta.connection_ids).unwrap_or_else(|_| "[]".to_string());
// ... .bind(&ids_json) at the appropriate position
```

For the SELECT mapping, parse the JSON column back into `Vec<String>` with `serde_json::from_str(&col_value).unwrap_or_default()`.

- [ ] **Step 3: Adjust callsites that construct `SessionMeta`**

Search: `SessionMeta {` across `crates/roy-management/src`. Add `connection_ids: vec![],` everywhere it's constructed today.

- [ ] **Step 4: Run management tests**

Run: `cargo test -p roy-management --no-fail-fast`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-management/src/meta_store.rs
git commit -m "feat(roy-management): persist connection_ids on session_meta"
```

### Task E2: Accept `connection_ids` on `POST /sessions`

**Files:**
- Modify: `crates/roy-management/src/http.rs`
- Modify: `crates/roy-management/src/roy_client.rs` (SpawnRequest already has connections from B2)

- [ ] **Step 1: Extend `CreateSessionReq`**

```rust
    #[serde(default)]
    connection_ids: Vec<String>,
```

- [ ] **Step 2: Resolve ids → specs → forward**

In `create_session`, after ACL checks and before the daemon `spawn` call, add:

```rust
    // Resolve connections (validates ownership: store filters by user_id).
    let specs = if req.connection_ids.is_empty() {
        Vec::new()
    } else {
        let conns = s
            .connections
            .get_many(&user_id, &req.connection_ids)
            .await
            .map_err(|e| match e {
                crate::connections::StoreError::NotFound(id) => ApiError(
                    StatusCode::BAD_REQUEST,
                    format!("unknown connection: {id}"),
                ),
                other => other.into(),
            })?;
        conns
            .into_iter()
            .map(|c| roy::ConnectionSpec {
                id: c.id,
                slug: c.slug,
                kind: c.kind,
                config: c.config,
                secrets: c.secrets,
            })
            .collect()
    };
```

Pass `connections: specs.clone()` to the `SpawnRequest`, and `connection_ids: req.connection_ids.clone()` to the `SessionMeta` built right after.

- [ ] **Step 3: Test**

Add a test in `crates/roy-management/tests/connections_http.rs` that:
1. Creates a connection,
2. Creates a session referencing it,
3. Verifies the daemon was called with the connection in the `SpawnRequest`.

Use a mock `DaemonClient` if one isn't available yet — `roy_client::DaemonClient` is a trait, so a test impl can be dropped in.

```rust
#[tokio::test]
async fn session_create_forwards_connection_specs() {
    use std::sync::Arc;
    use tokio::sync::Mutex;

    struct CapturingDaemon {
        last: Arc<Mutex<Option<crate::common::ObservedSpawn>>>,
    }
    // ... see existing tests for the shared mock pattern; add `observed_connections` field
    // to `ObservedSpawn` if needed.
}
```

(If the existing test mock doesn't capture connections, extend its `ObservedSpawn` shape to do so as part of this task.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p roy-management --no-fail-fast`
Expected: PASS — new test + existing ones.

- [ ] **Step 5: Commit**

```bash
git add crates/roy-management/src/http.rs crates/roy-management/tests/connections_http.rs
git commit -m "feat(roy-management): resolve connection_ids and forward to daemon on session create"
```

---

## Phase F — End-to-end smoke + docs

End state: a documented end-to-end path proven by an integration test that drives the whole stack with a fake upstream MCP and a fake ACP transport.

### Task F1: End-to-end test

**Files:**
- Create: `crates/roy-management/tests/connections_e2e.rs`

- [ ] **Step 1: Write a single end-to-end test**

The test uses the existing fake-acp-agent (which speaks ACP) plus the fake-mcp-upstream from Phase C. It:

1. Starts a real daemon process with a `DefaultTransportFactory` patched to substitute the claude preset's `command` with `python3 tests/scripts/fake-acp-agent.py` (it should already honor `ROY_FAKE_ACP_BIN` — if not, add the override in this task).
2. Spawns a session via the management HTTP API with one connection.
3. Verifies that the session cwd contains a `.mcp.json` referencing `roy mcp serve-connections`.
4. Cleans up.

A simpler MVP version that exercises the data path without spawning a real daemon: in-process `Daemon` with a custom `TransportFactory` that recognizes the claude preset and just inspects the connection list it received. This is what the test does — verifies plumbing, not the live ACP/MCP handshake. The dispatch-level handshake is already covered by Phase C's test.

```rust
//! End-to-end plumbing: HTTP /sessions -> daemon -> TransportFactory observes
//! the right ConnectionSpec list. The MCP child & upstream handshake live in
//! crates/roy-mcp/tests/serve_connections.rs.

use serde_json::{json, Value};
// ... full test body following the auth_flow.rs pattern.
```

Skipping the exact code body here would violate "no placeholders" — write a self-contained ~100-line test that:
- Spins up `app_with_user`,
- POSTs `/connections` (gets id),
- POSTs `/sessions` with `connection_ids: [id]`,
- Asserts the captured daemon SpawnRequest has one `ConnectionSpec` matching the created connection.

Pattern is the same as the Phase E test — this one verifies the full HTTP→daemon path end-to-end with a `CapturingDaemon`.

- [ ] **Step 2: Run**

Run: `cargo test -p roy-management --test connections_e2e -- --nocapture`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/roy-management/tests/connections_e2e.rs
git commit -m "test(roy-management): end-to-end /connections -> /sessions -> daemon spawn"
```

### Task F2: Update CLAUDE.md and add ops note

**Files:**
- Modify: `CLAUDE.md`
- Create (optional): `docs/connections.md`

- [ ] **Step 1: Add a short section to `CLAUDE.md`**

Under the existing per-crate descriptions, in the section that lists what each crate owns, add:

```markdown
- **`crates/roy-mcp`** — library. MCP (Model Context Protocol) server.
  Hosts two subcommands:
  - `roy mcp serve` — exposes daemon control operations as MCP tools.
  - `roy mcp serve-connections` — proxying MCP server: aggregates user-owned
    upstream stdio MCPs (registered via `roy-management`'s `/connections`)
    into a single namespace-prefixed tool set. Spawned by the daemon as a
    child of the ACP agent (currently: claude preset only) via the
    project-level `.mcp.json` written into the session cwd at spawn time.
```

- [ ] **Step 2: Note the MVP limits**

Add a small "Status" block to the same section:

```markdown
  **MVP status (2026-05-27):** stdio upstream only; claude preset only;
  plain-text secrets in DB (file mode 0600); tools snapshot at spawn (no
  list_changed push); no `always_attach` flag yet.
```

- [ ] **Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: describe roy mcp serve-connections and MVP status"
```

---

## Follow-ups (out of MVP scope; tracked but not implemented)

These are deliberate deferrals — listed so future work has a clear backlog:

1. **HTTP/SSE upstream transports.** Extend `Upstream` with a second variant; surface `kind = "mcp_http"` and `kind = "mcp_sse"` in `validate_kind`.
2. **OAuth credentials.** Add a new connection kind `mcp_oauth_http`; persist refresh tokens in `secrets_json`; implement PKCE flow callback under `roy-management`.
3. **`always_attach` column.** One-line schema + `Spawn`-side expansion in `roy-management`. Per the conversation, resume must NOT re-expand — `connection_ids` on `session_meta` is the source of truth at resume.
4. **`notifications/tools/list_changed` push.** Watch the DB (sqlite update hook or polling), refresh upstream's `tools` cache, emit `notifications/tools/list_changed` over the proxy's stdout. Lets the agent see new tools without reopening the session.
5. **Secrets encryption.** Wrap `secrets_json` reads/writes in a `ROY_SECRETS_KEY`-derived cipher. Single migration path: re-encrypt-in-place.
6. **`POST /connections/{id}/test`.** Spawn an ephemeral upstream against the user's config + creds, drive `initialize` + `tools/list`, return `{ok: true, tools: [...]}` or a structured error.
7. **Other ACP presets.** Codex: write to `$CODEX_HOME/config.toml` overlay. Opencode: write `<cwd>/opencode.json`. Gemini: per-session `$HOME` overlay with `~/.gemini/settings.json`. Each gets its own task; the `inject_mcp` flag lights up per preset.

---

## Self-review

**Spec coverage:**
- DB + per-user ownership ✅ (A1, A3)
- HTTP CRUD ✅ (A4)
- Wire protocol carries inline specs (daemon stays DB-ignorant) ✅ (B1, B2)
- Proxy server with namespacing ✅ (C1, C3, C4)
- Per-preset injection (claude only in MVP) ✅ (D1, D2)
- Session metadata audit trail ✅ (E1, E2)
- End-to-end test ✅ (F1)

**Type consistency:** `ConnectionSpec` is the wire type used everywhere — `roy::control::ConnectionSpec` in the daemon/management/CLI boundary, mirrored as `roy_mcp::serve_connections::spec::ConnectionSpec` in the proxy (separate copy to keep `roy-mcp` cycle-free). The two are serde-compatible by construction.

**Placeholder scan:** Two soft references survive:
- Task E2 Step 3 references "existing test mock" — if it doesn't exist in current code, extend `tests/common/mod.rs` with the same `CapturingDaemon` pattern used here.
- Task F1 Step 1 prose-describes the test body. Reason: implementing the exact test requires inspecting the management test harness which depends on choices made in E2. The body must be written from scratch following the documented assertions; the operative checks are concrete (file existence, captured SpawnRequest equality).

If you want either to be a fully-baked code block before execution, raise it during plan review.
