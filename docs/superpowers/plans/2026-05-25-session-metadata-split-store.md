# Session Metadata Split-Store Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Вытащить session-metadata и projects из `roy` core в split-store: core владеет boot-kit SQLite в `~/.local/state/roy/sessions.db`, `roy-management` владеет rich-метой (projects/session_meta/session_tags) поверх той же `agents.db`.

**Architecture:** Daemon работает с SessionStore (sqlx pool, новая `sessions` таблица) — никаких `.meta.json` и `projects.json` файлов. Management добавляет 3 таблицы в существующую `agents.db` через свои миграции, координирует spawn через POST /sessions (calls daemon over Unix socket, persists rich-meta in tx, rollback through Close). CLI ходит в management для project/tag-aware команд; orphan spawn (`--cwd`) остаётся прямо в daemon.

**Tech Stack:** Rust 2021, sqlx 0.8 (sqlite), axum 0.8, reqwest (новая зависимость в roy-cli), tokio. Wire-protocol — JSON over Unix socket для daemon, HTTP/JSON для management.

**Reference spec:** `docs/superpowers/specs/2026-05-25-session-metadata-split-store-design.md`.

**Upgrade requirement:** Before running anything, разработчик должен очистить старые данные:
```bash
rm -rf ~/.roy/journals
rm -f  ~/.roy/projects.json
```

---

## Phase 1 — Core SessionStore (compiles, unused, tested)

### Task 1: Add sqlx dependency and migration file

**Files:**
- Modify: `crates/roy/Cargo.toml` (add sqlx + chrono)
- Create: `crates/roy/migrations/sqlite/0001_sessions.sql`

- [ ] **Step 1: Add sqlx to `[dependencies]` in `crates/roy/Cargo.toml`**

Add after `agent-client-protocol = "0.12.1"`:

```toml
sqlx = { version = "0.8", default-features = false, features = [
  "runtime-tokio",
  "sqlite",
  "chrono",
  "macros",
  "migrate",
] }
chrono = { version = "0.4", default-features = false, features = ["clock"] }
```

- [ ] **Step 2: Create migration file**

`crates/roy/migrations/sqlite/0001_sessions.sql`:

```sql
CREATE TABLE sessions (
  session_id     TEXT PRIMARY KEY,
  agent          TEXT NOT NULL,
  cwd            TEXT NOT NULL,
  model          TEXT,
  permission     TEXT,
  resume_cursor  TEXT,
  system_prompt  TEXT,
  created_at     INTEGER NOT NULL,
  closed_at      INTEGER
);

CREATE INDEX sessions_live ON sessions(closed_at) WHERE closed_at IS NULL;
```

- [ ] **Step 3: Verify build**

Run: `cargo build -p roy`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/roy/Cargo.toml crates/roy/migrations/
git commit -m "feat(roy): add sqlx + sessions.sql migration"
```

---

### Task 2: SessionStore module — open + insert + get + roundtrip test

**Files:**
- Create: `crates/roy/src/session_store.rs`
- Modify: `crates/roy/src/lib.rs` (add `pub mod session_store;`)

- [ ] **Step 1: Add `pub mod session_store;` to `crates/roy/src/lib.rs`**

Insert in module list (alphabetical after `session_meta`):

```rust
pub mod session_store;
```

- [ ] **Step 2: Write the failing test first**

Create `crates/roy/src/session_store.rs` with the test only:

```rust
//! Boot-kit-only SQLite store: minimum fields needed to resume a session
//! after a daemon restart. Lives at `~/.local/state/roy/sessions.db`.

use std::path::{Path, PathBuf};

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

use crate::error::{Result, RoyError};

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("migrations/sqlite");

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRow {
    pub session_id: String,
    pub agent: String,
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub permission: Option<String>,
    pub resume_cursor: Option<String>,
    pub system_prompt: Option<String>,
    pub created_at: i64,
    pub closed_at: Option<i64>,
}

pub struct SessionStore {
    pool: SqlitePool,
}

impl SessionStore {
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(RoyError::Io)?;
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
            .map_err(|e| RoyError::Protocol(format!("opening sessions.db: {e}")))?;
        MIGRATOR
            .run(&pool)
            .await
            .map_err(|e| RoyError::Protocol(format!("running migrations: {e}")))?;
        Ok(Self { pool })
    }

    pub async fn insert(&self, row: &SessionRow) -> Result<()> {
        sqlx::query(
            "INSERT INTO sessions \
             (session_id, agent, cwd, model, permission, resume_cursor, \
              system_prompt, created_at, closed_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&row.session_id)
        .bind(&row.agent)
        .bind(row.cwd.to_string_lossy().as_ref())
        .bind(&row.model)
        .bind(&row.permission)
        .bind(&row.resume_cursor)
        .bind(&row.system_prompt)
        .bind(row.created_at)
        .bind(row.closed_at)
        .execute(&self.pool)
        .await
        .map_err(|e| RoyError::Protocol(format!("insert session: {e}")))?;
        Ok(())
    }

    pub async fn get(&self, session_id: &str) -> Result<Option<SessionRow>> {
        let row: Option<(
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            i64,
            Option<i64>,
        )> = sqlx::query_as(
            "SELECT session_id, agent, cwd, model, permission, resume_cursor, \
             system_prompt, created_at, closed_at FROM sessions WHERE session_id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| RoyError::Protocol(format!("get session: {e}")))?;
        Ok(row.map(|r| SessionRow {
            session_id: r.0,
            agent: r.1,
            cwd: PathBuf::from(r.2),
            model: r.3,
            permission: r.4,
            resume_cursor: r.5,
            system_prompt: r.6,
            created_at: r.7,
            closed_at: r.8,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_row(sid: &str) -> SessionRow {
        SessionRow {
            session_id: sid.into(),
            agent: "claude".into(),
            cwd: PathBuf::from("/tmp/x"),
            model: Some("claude-opus-4-7".into()),
            permission: Some("allow".into()),
            resume_cursor: Some("cursor-1".into()),
            system_prompt: Some("be terse".into()),
            created_at: 1722345600,
            closed_at: None,
        }
    }

    #[tokio::test]
    async fn insert_and_get_roundtrip() {
        let dir = tempdir().unwrap();
        let store = SessionStore::open(&dir.path().join("sessions.db"))
            .await
            .unwrap();
        let row = sample_row("sid-1");
        store.insert(&row).await.unwrap();
        let back = store.get("sid-1").await.unwrap().unwrap();
        assert_eq!(back, row);
        assert!(store.get("missing").await.unwrap().is_none());
    }
}
```

- [ ] **Step 3: Run the test**

Run: `cargo test -p roy session_store::tests::insert_and_get_roundtrip`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/roy/src/session_store.rs crates/roy/src/lib.rs
git commit -m "feat(roy): SessionStore::open + insert + get"
```

---

### Task 3: SessionStore — list_live / list_archived / mark_closed / delete

**Files:**
- Modify: `crates/roy/src/session_store.rs`

- [ ] **Step 1: Write failing tests for new methods**

Append to `#[cfg(test)] mod tests`:

```rust
#[tokio::test]
async fn list_live_excludes_closed() {
    let dir = tempdir().unwrap();
    let store = SessionStore::open(&dir.path().join("sessions.db"))
        .await
        .unwrap();
    let mut live = sample_row("live");
    live.closed_at = None;
    let mut closed = sample_row("closed");
    closed.closed_at = Some(1722345700);
    store.insert(&live).await.unwrap();
    store.insert(&closed).await.unwrap();

    let live_rows = store.list_live().await.unwrap();
    assert_eq!(live_rows.len(), 1);
    assert_eq!(live_rows[0].session_id, "live");

    let archived = store.list_archived().await.unwrap();
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].session_id, "closed");
}

#[tokio::test]
async fn mark_closed_then_delete() {
    let dir = tempdir().unwrap();
    let store = SessionStore::open(&dir.path().join("sessions.db"))
        .await
        .unwrap();
    store.insert(&sample_row("sid")).await.unwrap();
    store.mark_closed("sid").await.unwrap();
    assert!(store.list_live().await.unwrap().is_empty());
    assert_eq!(store.list_archived().await.unwrap().len(), 1);

    store.delete("sid").await.unwrap();
    assert!(store.get("sid").await.unwrap().is_none());
}

#[tokio::test]
async fn update_cursor_and_model() {
    let dir = tempdir().unwrap();
    let store = SessionStore::open(&dir.path().join("sessions.db"))
        .await
        .unwrap();
    store.insert(&sample_row("sid")).await.unwrap();
    store.update_cursor("sid", Some("cursor-2")).await.unwrap();
    store
        .update_model("sid", Some("claude-haiku-4-5"))
        .await
        .unwrap();
    let row = store.get("sid").await.unwrap().unwrap();
    assert_eq!(row.resume_cursor.as_deref(), Some("cursor-2"));
    assert_eq!(row.model.as_deref(), Some("claude-haiku-4-5"));
}
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test -p roy session_store::tests`
Expected: FAIL — methods don't exist yet

- [ ] **Step 3: Implement methods**

Append to `impl SessionStore`:

```rust
pub async fn update_cursor(&self, session_id: &str, cursor: Option<&str>) -> Result<()> {
    sqlx::query("UPDATE sessions SET resume_cursor = ? WHERE session_id = ?")
        .bind(cursor)
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| RoyError::Protocol(format!("update_cursor: {e}")))?;
    Ok(())
}

pub async fn update_model(&self, session_id: &str, model: Option<&str>) -> Result<()> {
    sqlx::query("UPDATE sessions SET model = ? WHERE session_id = ?")
        .bind(model)
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| RoyError::Protocol(format!("update_model: {e}")))?;
    Ok(())
}

pub async fn mark_closed(&self, session_id: &str) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    sqlx::query("UPDATE sessions SET closed_at = ? WHERE session_id = ? AND closed_at IS NULL")
        .bind(now)
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| RoyError::Protocol(format!("mark_closed: {e}")))?;
    Ok(())
}

pub async fn delete(&self, session_id: &str) -> Result<()> {
    sqlx::query("DELETE FROM sessions WHERE session_id = ?")
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map_err(|e| RoyError::Protocol(format!("delete: {e}")))?;
    Ok(())
}

pub async fn list_live(&self) -> Result<Vec<SessionRow>> {
    self.list_by_state(true).await
}

pub async fn list_archived(&self) -> Result<Vec<SessionRow>> {
    self.list_by_state(false).await
}

async fn list_by_state(&self, live: bool) -> Result<Vec<SessionRow>> {
    let predicate = if live {
        "closed_at IS NULL"
    } else {
        "closed_at IS NOT NULL"
    };
    let sql = format!(
        "SELECT session_id, agent, cwd, model, permission, resume_cursor, \
         system_prompt, created_at, closed_at FROM sessions WHERE {predicate} \
         ORDER BY created_at"
    );
    let rows: Vec<(
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        Option<i64>,
    )> = sqlx::query_as(&sql)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| RoyError::Protocol(format!("list_by_state: {e}")))?;
    Ok(rows
        .into_iter()
        .map(|r| SessionRow {
            session_id: r.0,
            agent: r.1,
            cwd: PathBuf::from(r.2),
            model: r.3,
            permission: r.4,
            resume_cursor: r.5,
            system_prompt: r.6,
            created_at: r.7,
            closed_at: r.8,
        })
        .collect())
}
```

- [ ] **Step 4: Run all session_store tests**

Run: `cargo test -p roy session_store::tests`
Expected: 3 PASS

- [ ] **Step 5: Commit**

```bash
git add crates/roy/src/session_store.rs
git commit -m "feat(roy): SessionStore list/update/mark_closed/delete"
```

---

### Task 4: Add `default_db_path` and integrate SessionStore into SessionManager (passive)

**Files:**
- Modify: `crates/roy/src/session_store.rs` (add helper)
- Modify: `crates/roy/src/manager.rs` (own `Arc<SessionStore>`, but don't use yet)

- [ ] **Step 1: Add `default_db_path` to session_store**

Append after `MIGRATOR`:

```rust
pub fn default_db_path() -> PathBuf {
    if let Some(p) = std::env::var_os("ROY_SESSIONS_DB") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/state/roy/sessions.db")
}
```

- [ ] **Step 2: Add `session_store: Arc<SessionStore>` field to SessionManager**

In `crates/roy/src/manager.rs` modify `SessionManager` struct and `new`:

```rust
use crate::session_store::SessionStore;

pub struct SessionManager {
    journal_dir: PathBuf,
    workspace_dir: PathBuf,
    sessions: RwLock<HashMap<String, Arc<SessionEngine>>>,
    factory: Arc<dyn TransportFactory>,
    projects: Arc<ProjectRegistry>,
    session_store: Arc<SessionStore>,
}

impl SessionManager {
    pub async fn new(
        journal_dir: PathBuf,
        workspace_dir: PathBuf,
        factory: Arc<dyn TransportFactory>,
        session_store: Arc<SessionStore>,
    ) -> Result<Self> {
        std::fs::create_dir_all(&workspace_dir).map_err(RoyError::Io)?;
        let projects = Arc::new(ProjectRegistry::load(&journal_dir, workspace_dir.clone())?);
        Ok(Self {
            journal_dir,
            workspace_dir,
            sessions: RwLock::new(HashMap::new()),
            factory,
            projects,
            session_store,
        })
    }
    ...
}
```

- [ ] **Step 3: Update all `SessionManager::new` callers**

Find callers:
```bash
grep -rn "SessionManager::new\b" /Users/i_strelov/Projects/roy/crates/ --include="*.rs"
```

For each caller in `daemon.rs` and tests — wrap construction:

```rust
let store_path = ...;  // for tests: tempdir().join("sessions.db")
let store = Arc::new(SessionStore::open(&store_path).await?);
let manager = SessionManager::new(journal_dir, workspace_dir, factory, store).await?;
```

For the test helper in `manager.rs` `new_mgr` (line ~375), make it async and pass a per-test tempfile path:

```rust
async fn new_mgr(dir: &PathBuf) -> SessionManager {
    let store_path = dir.join("sessions.db");
    let store = Arc::new(SessionStore::open(&store_path).await.expect("open store"));
    SessionManager::new(dir.clone(), dir.join("workspace"), Arc::new(FakeFactory), store)
        .await
        .expect("registry load")
}
```

Update test bodies to `let mgr = new_mgr(&dir).await;`.

For daemon construction in `daemon.rs`, find `SessionManager::new` call (search for it) and use `SessionStore::default_db_path()` for prod, or a test-supplied path for daemon tests.

- [ ] **Step 4: Build & test**

Run: `cargo build --workspace --all-targets`
Expected: PASS

Run: `cargo test -p roy --lib`
Expected: PASS (no behavioural change yet)

- [ ] **Step 5: Commit**

```bash
git add crates/roy/src/session_store.rs crates/roy/src/manager.rs crates/roy/src/daemon.rs
git commit -m "feat(roy): wire SessionStore into SessionManager (passive)"
```

---

## Phase 2 — Management meta_store (compiles, unused, tested)

### Task 5: Add management migrations file

**Files:**
- Create: `crates/roy-management/migrations/sqlite/0001_projects_and_session_meta.sql`
- Modify: `crates/roy-management/Cargo.toml` (no change — sqlx already there)

- [ ] **Step 1: Create migration**

`crates/roy-management/migrations/sqlite/0001_projects_and_session_meta.sql`:

```sql
CREATE TABLE projects (
  id         TEXT PRIMARY KEY,
  name       TEXT UNIQUE NOT NULL,
  path       TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE session_meta (
  session_id    TEXT PRIMARY KEY,
  project_id    TEXT REFERENCES projects(id) ON DELETE SET NULL,
  agent_id      TEXT REFERENCES agents(id) ON DELETE SET NULL,
  agent_name    TEXT,
  display_label TEXT,
  created_at    INTEGER NOT NULL
);
CREATE INDEX session_meta_project ON session_meta(project_id);

CREATE TABLE session_tags (
  session_id TEXT NOT NULL,
  key        TEXT NOT NULL,
  value      TEXT NOT NULL,
  PRIMARY KEY (session_id, key)
);
CREATE INDEX session_tags_key_value ON session_tags(key, value);
```

- [ ] **Step 2: Verify build (migrations not yet applied — fine)**

Run: `cargo build -p roy-management`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/roy-management/migrations/
git commit -m "feat(roy-management): add projects + session_meta + session_tags migrations"
```

---

### Task 6: meta_store module — Project CRUD with tests

**Files:**
- Create: `crates/roy-management/src/meta_store.rs`
- Modify: `crates/roy-management/src/lib.rs` (add `pub mod meta_store;`)

- [ ] **Step 1: Add module declaration in `crates/roy-management/src/lib.rs`**

```rust
pub mod meta_store;
```

- [ ] **Step 2: Write the failing tests first**

Create `crates/roy-management/src/meta_store.rs`:

```rust
//! Management-owned tables (projects, session_meta, session_tags) on top of
//! the shared `agents.db` SqlitePool. Migrations live in
//! `crates/roy-management/migrations/sqlite/`. Apply with
//! `meta_store::MIGRATOR.run(pool)` after `roy_agents::open` has applied
//! its own migrations.

use std::collections::BTreeMap;

use sqlx::SqlitePool;
use thiserror::Error;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("migrations/sqlite");

#[derive(Debug, Error)]
pub enum MetaError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionMeta {
    pub session_id: String,
    pub project_id: Option<String>,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub display_label: Option<String>,
    pub tags: BTreeMap<String, String>,
    pub created_at: i64,
}

#[derive(Clone)]
pub struct MetaStore {
    pool: SqlitePool,
}

impl MetaStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn apply_migrations(pool: &SqlitePool) -> Result<(), sqlx::Error> {
        MIGRATOR.run(pool).await.map_err(sqlx::Error::from)
    }

    pub async fn create_project(&self, name: &str) -> Result<Project, MetaError> {
        validate_project_name(name)?;
        let id = uuid::Uuid::new_v4().to_string();
        let workspace = workspace_dir_default();
        let path = workspace.join(name).to_string_lossy().into_owned();
        let created_at = chrono::Utc::now().timestamp();
        let result = sqlx::query(
            "INSERT INTO projects (id, name, path, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(name)
        .bind(&path)
        .bind(created_at)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => Ok(Project {
                id,
                name: name.into(),
                path,
                created_at,
            }),
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => Err(MetaError::Conflict(
                format!("project name already exists: {name}"),
            )),
            Err(e) => Err(MetaError::Db(e)),
        }
    }

    pub async fn list_projects(&self) -> Result<Vec<Project>, MetaError> {
        let rows: Vec<(String, String, String, i64)> =
            sqlx::query_as("SELECT id, name, path, created_at FROM projects ORDER BY created_at")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .map(|r| Project {
                id: r.0,
                name: r.1,
                path: r.2,
                created_at: r.3,
            })
            .collect())
    }

    pub async fn delete_project(&self, id: &str) -> Result<(), MetaError> {
        let res = sqlx::query("DELETE FROM projects WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(MetaError::NotFound(id.into()));
        }
        Ok(())
    }
}

fn validate_project_name(name: &str) -> Result<(), MetaError> {
    if name.is_empty() {
        return Err(MetaError::Invalid("name must not be empty".into()));
    }
    if name.starts_with('.') {
        return Err(MetaError::Invalid("name must not start with '.'".into()));
    }
    for ch in name.chars() {
        if !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-' {
            return Err(MetaError::Invalid(format!(
                "name may only contain ASCII letters, digits, '_', '-'; got '{ch}'"
            )));
        }
    }
    Ok(())
}

fn workspace_dir_default() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("ROY_WORKSPACE_DIR") {
        return std::path::PathBuf::from(p);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    std::path::PathBuf::from(home).join(".roy/workspace")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    async fn fresh_store() -> MetaStore {
        let dir = tempdir().unwrap();
        let pool = roy_agents::open(&dir.path().join("agents.db"))
            .await
            .unwrap();
        MetaStore::apply_migrations(&pool).await.unwrap();
        std::mem::forget(dir);
        MetaStore::new(pool)
    }

    #[tokio::test]
    async fn create_then_list_project() {
        let store = fresh_store().await;
        let p = store.create_project("my-proj").await.unwrap();
        assert_eq!(p.name, "my-proj");
        let listed = store.list_projects().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, p.id);
    }

    #[tokio::test]
    async fn create_duplicate_is_conflict() {
        let store = fresh_store().await;
        store.create_project("dup").await.unwrap();
        let err = store.create_project("dup").await.unwrap_err();
        assert!(matches!(err, MetaError::Conflict(_)));
    }

    #[tokio::test]
    async fn invalid_name_rejected() {
        let store = fresh_store().await;
        for bad in ["", ".hidden", "has/slash", "has space"] {
            assert!(matches!(
                store.create_project(bad).await,
                Err(MetaError::Invalid(_))
            ));
        }
    }

    #[tokio::test]
    async fn delete_project() {
        let store = fresh_store().await;
        let p = store.create_project("del-me").await.unwrap();
        store.delete_project(&p.id).await.unwrap();
        assert!(matches!(
            store.delete_project(&p.id).await,
            Err(MetaError::NotFound(_))
        ));
    }
}
```

- [ ] **Step 3: Add uuid + chrono + thiserror to roy-management Cargo.toml deps**

```toml
uuid = { version = "1", features = ["v4"] }
chrono = { version = "0.4", default-features = false, features = ["clock"] }
thiserror = "2"
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p roy-management meta_store::tests`
Expected: 4 PASS

- [ ] **Step 5: Commit**

```bash
git add crates/roy-management/src/lib.rs crates/roy-management/src/meta_store.rs crates/roy-management/Cargo.toml
git commit -m "feat(roy-management): MetaStore project CRUD"
```

---

### Task 7: meta_store — SessionMeta + tags CRUD

**Files:**
- Modify: `crates/roy-management/src/meta_store.rs`

- [ ] **Step 1: Write the failing tests**

Append to `tests` mod:

```rust
#[tokio::test]
async fn upsert_session_meta_and_tags() {
    let store = fresh_store().await;
    let p = store.create_project("p1").await.unwrap();
    let mut tags = BTreeMap::new();
    tags.insert("env".into(), "prod".into());
    tags.insert("owner".into(), "alice".into());
    let meta = SessionMeta {
        session_id: "sid-1".into(),
        project_id: Some(p.id.clone()),
        agent_id: None,
        agent_name: Some("Reviewer".into()),
        display_label: None,
        tags,
        created_at: 1722345600,
    };
    store.upsert_session_meta(&meta).await.unwrap();

    let back = store.get_session_meta("sid-1").await.unwrap().unwrap();
    assert_eq!(back, meta);
}

#[tokio::test]
async fn replace_tags() {
    let store = fresh_store().await;
    let mut tags = BTreeMap::new();
    tags.insert("k1".into(), "v1".into());
    store
        .upsert_session_meta(&SessionMeta {
            session_id: "sid".into(),
            project_id: None,
            agent_id: None,
            agent_name: None,
            display_label: None,
            tags,
            created_at: 1,
        })
        .await
        .unwrap();
    let mut new_tags = BTreeMap::new();
    new_tags.insert("k2".into(), "v2".into());
    store.replace_tags("sid", &new_tags).await.unwrap();
    let back = store.get_session_meta("sid").await.unwrap().unwrap();
    assert_eq!(back.tags, new_tags);
}

#[tokio::test]
async fn delete_project_sets_session_project_id_null() {
    let store = fresh_store().await;
    let p = store.create_project("kill-me").await.unwrap();
    store
        .upsert_session_meta(&SessionMeta {
            session_id: "sid".into(),
            project_id: Some(p.id.clone()),
            agent_id: None,
            agent_name: None,
            display_label: None,
            tags: BTreeMap::new(),
            created_at: 1,
        })
        .await
        .unwrap();
    store.delete_project(&p.id).await.unwrap();
    let back = store.get_session_meta("sid").await.unwrap().unwrap();
    assert!(back.project_id.is_none());
}

#[tokio::test]
async fn delete_session_meta_clears_tags() {
    let store = fresh_store().await;
    let mut tags = BTreeMap::new();
    tags.insert("k".into(), "v".into());
    store
        .upsert_session_meta(&SessionMeta {
            session_id: "sid".into(),
            project_id: None,
            agent_id: None,
            agent_name: None,
            display_label: None,
            tags,
            created_at: 1,
        })
        .await
        .unwrap();
    store.delete_session_meta("sid").await.unwrap();
    assert!(store.get_session_meta("sid").await.unwrap().is_none());
}
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test -p roy-management meta_store::tests`
Expected: 4 FAIL (methods not defined)

- [ ] **Step 3: Implement methods**

Add to `impl MetaStore` (use one tx for upsert + tags):

```rust
pub async fn upsert_session_meta(&self, meta: &SessionMeta) -> Result<(), MetaError> {
    let mut tx = self.pool.begin().await?;
    sqlx::query(
        "INSERT INTO session_meta (session_id, project_id, agent_id, agent_name, \
         display_label, created_at) VALUES (?, ?, ?, ?, ?, ?) \
         ON CONFLICT(session_id) DO UPDATE SET \
         project_id = excluded.project_id, agent_id = excluded.agent_id, \
         agent_name = excluded.agent_name, display_label = excluded.display_label",
    )
    .bind(&meta.session_id)
    .bind(&meta.project_id)
    .bind(&meta.agent_id)
    .bind(&meta.agent_name)
    .bind(&meta.display_label)
    .bind(meta.created_at)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM session_tags WHERE session_id = ?")
        .bind(&meta.session_id)
        .execute(&mut *tx)
        .await?;
    for (k, v) in &meta.tags {
        sqlx::query("INSERT INTO session_tags (session_id, key, value) VALUES (?, ?, ?)")
            .bind(&meta.session_id)
            .bind(k)
            .bind(v)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn replace_tags(
    &self,
    session_id: &str,
    tags: &BTreeMap<String, String>,
) -> Result<(), MetaError> {
    let mut tx = self.pool.begin().await?;
    sqlx::query("DELETE FROM session_tags WHERE session_id = ?")
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    for (k, v) in tags {
        sqlx::query("INSERT INTO session_tags (session_id, key, value) VALUES (?, ?, ?)")
            .bind(session_id)
            .bind(k)
            .bind(v)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn get_session_meta(
    &self,
    session_id: &str,
) -> Result<Option<SessionMeta>, MetaError> {
    let row: Option<(
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
    )> = sqlx::query_as(
        "SELECT session_id, project_id, agent_id, agent_name, display_label, created_at \
         FROM session_meta WHERE session_id = ?",
    )
    .bind(session_id)
    .fetch_optional(&self.pool)
    .await?;
    let Some(r) = row else { return Ok(None) };
    let tags_rows: Vec<(String, String)> =
        sqlx::query_as("SELECT key, value FROM session_tags WHERE session_id = ?")
            .bind(session_id)
            .fetch_all(&self.pool)
            .await?;
    Ok(Some(SessionMeta {
        session_id: r.0,
        project_id: r.1,
        agent_id: r.2,
        agent_name: r.3,
        display_label: r.4,
        tags: tags_rows.into_iter().collect(),
        created_at: r.5,
    }))
}

pub async fn delete_session_meta(&self, session_id: &str) -> Result<(), MetaError> {
    let mut tx = self.pool.begin().await?;
    sqlx::query("DELETE FROM session_tags WHERE session_id = ?")
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM session_meta WHERE session_id = ?")
        .bind(session_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn list_session_metas(
    &self,
    session_ids: &[String],
) -> Result<Vec<SessionMeta>, MetaError> {
    let mut out = Vec::with_capacity(session_ids.len());
    for sid in session_ids {
        if let Some(m) = self.get_session_meta(sid).await? {
            out.push(m);
        }
    }
    Ok(out)
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p roy-management meta_store::tests`
Expected: all PASS

- [ ] **Step 5: Commit**

```bash
git add crates/roy-management/src/meta_store.rs
git commit -m "feat(roy-management): SessionMeta + tags CRUD in MetaStore"
```

---

### Task 8: DaemonClient trait + UnixSocket impl + Mock impl

**Files:**
- Modify: `crates/roy-management/src/roy_client.rs` (replace with trait + impl)
- Modify: `crates/roy-management/src/state.rs` (hold `Arc<dyn DaemonClient>`)
- Modify: `crates/roy-management/Cargo.toml` (add async-trait if not present)

- [ ] **Step 1: Verify async-trait is available**

Check `cargo metadata --format-version 1 | grep async-trait` — it's transitive from roy. Add direct dep:

```toml
async-trait = "0.1"
```

- [ ] **Step 2: Define trait + extract existing impl**

Replace `crates/roy-management/src/roy_client.rs` content:

```rust
//! Daemon-client abstraction: trait `DaemonClient` for management-side
//! coordination, plus the production `UnixSocketDaemonClient` impl. Tests
//! use `MockDaemonClient` (see `meta_store::tests`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use roy::{ClientCommand, ServerEvent};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Debug, Clone)]
pub struct SpawnRequest {
    pub agent: String,
    pub cwd: Option<PathBuf>,
    pub model: Option<String>,
    pub permission: Option<String>,
    pub system_prompt: Option<String>,
}

#[async_trait]
pub trait DaemonClient: Send + Sync {
    async fn spawn(&self, req: SpawnRequest) -> Result<String>;
    async fn close(&self, session_id: &str) -> Result<()>;
    async fn list(&self) -> Result<Vec<String>>;
    async fn list_archived(&self) -> Result<Vec<String>>;
    async fn list_presets(&self) -> Result<serde_json::Value>;
}

pub struct UnixSocketDaemonClient {
    socket: PathBuf,
}

impl UnixSocketDaemonClient {
    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
    }

    async fn connect_and_send(
        &self,
        cmd: &ClientCommand,
    ) -> Result<tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>> {
        connect_and_send(&self.socket, cmd).await
    }
}

#[async_trait]
impl DaemonClient for UnixSocketDaemonClient {
    async fn spawn(&self, req: SpawnRequest) -> Result<String> {
        // NOTE: After Phase 3 lands, `ClientCommand::Spawn` will have `cwd`
        // instead of `project_id`. This impl assumes that final shape.
        let cmd = ClientCommand::Spawn {
            agent: req.agent,
            cwd: req.cwd,
            model: req.model,
            permission: req.permission,
            resume: None,
            system_prompt: req.system_prompt,
        };
        let mut lines = self.connect_and_send(&cmd).await?;
        loop {
            let raw = lines
                .next_line()
                .await?
                .ok_or_else(|| anyhow!("daemon hung up before Spawned"))?;
            match serde_json::from_str::<ServerEvent>(raw.trim())? {
                ServerEvent::Spawning { .. } => continue,
                ServerEvent::Spawned { session, .. } => return Ok(session),
                ServerEvent::Error { code, message, .. } => {
                    return Err(anyhow!("daemon error [{code}]: {message}"))
                }
                _ => continue,
            }
        }
    }

    async fn close(&self, session_id: &str) -> Result<()> {
        let mut lines = self
            .connect_and_send(&ClientCommand::Close {
                session: session_id.into(),
            })
            .await?;
        loop {
            let raw = lines
                .next_line()
                .await?
                .ok_or_else(|| anyhow!("daemon hung up before Closed"))?;
            match serde_json::from_str::<ServerEvent>(raw.trim())? {
                ServerEvent::Closed { .. } => return Ok(()),
                ServerEvent::Error { code, message, .. } => {
                    return Err(anyhow!("daemon error [{code}]: {message}"))
                }
                _ => continue,
            }
        }
    }

    async fn list(&self) -> Result<Vec<String>> {
        list_inner(&self.socket, ClientCommand::List).await
    }

    async fn list_archived(&self) -> Result<Vec<String>> {
        list_inner(&self.socket, ClientCommand::ListArchived).await
    }

    async fn list_presets(&self) -> Result<serde_json::Value> {
        let mut lines = self.connect_and_send(&ClientCommand::ListAgents).await?;
        loop {
            let raw = lines
                .next_line()
                .await?
                .ok_or_else(|| anyhow!("daemon hung up before AgentsList"))?;
            let trimmed = raw.trim();
            match serde_json::from_str::<ServerEvent>(trimmed)? {
                ServerEvent::AgentsList { .. } => return Ok(serde_json::from_str(trimmed)?),
                ServerEvent::Error { code, message, .. } => {
                    return Err(anyhow!("daemon error [{code}]: {message}"))
                }
                _ => continue,
            }
        }
    }
}

async fn list_inner(socket: &Path, cmd: ClientCommand) -> Result<Vec<String>> {
    let mut lines = connect_and_send(socket, &cmd).await?;
    loop {
        let raw = lines
            .next_line()
            .await?
            .ok_or_else(|| anyhow!("daemon hung up"))?;
        match serde_json::from_str::<ServerEvent>(raw.trim())? {
            ServerEvent::Listed { sessions } | ServerEvent::ListedArchived { sessions } => {
                return Ok(sessions.into_iter().map(|s| s.session_id).collect());
            }
            ServerEvent::Error { code, message, .. } => {
                return Err(anyhow!("daemon error [{code}]: {message}"))
            }
            _ => continue,
        }
    }
}

async fn connect_and_send(
    socket: &Path,
    cmd: &ClientCommand,
) -> Result<tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>> {
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connecting to roy daemon at {}", socket.display()))?;
    let (reader, mut writer) = stream.into_split();
    let line = serde_json::to_string(cmd)?;
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(BufReader::new(reader).lines())
}

#[cfg(test)]
pub(crate) mod mock {
    use super::*;
    use std::sync::Mutex;

    /// Configurable mock for HTTP-handler tests. Records all spawn/close
    /// calls and returns scripted responses.
    #[derive(Default)]
    pub struct MockDaemonClient {
        pub spawn_response: Mutex<Option<Result<String, String>>>,
        pub close_response: Mutex<Option<Result<(), String>>>,
        pub list_response: Mutex<Option<Vec<String>>>,
        pub recorded_spawns: Mutex<Vec<SpawnRequest>>,
        pub recorded_closes: Mutex<Vec<String>>,
    }

    impl MockDaemonClient {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn with_spawn(mut self, sid: &str) -> Self {
            self.spawn_response = Mutex::new(Some(Ok(sid.into())));
            self
        }
    }

    #[async_trait]
    impl DaemonClient for MockDaemonClient {
        async fn spawn(&self, req: SpawnRequest) -> Result<String> {
            self.recorded_spawns.lock().unwrap().push(req);
            match self.spawn_response.lock().unwrap().take() {
                Some(Ok(s)) => Ok(s),
                Some(Err(e)) => Err(anyhow!(e)),
                None => Err(anyhow!("MockDaemonClient: no spawn_response set")),
            }
        }
        async fn close(&self, sid: &str) -> Result<()> {
            self.recorded_closes.lock().unwrap().push(sid.into());
            match self.close_response.lock().unwrap().take() {
                Some(Ok(())) => Ok(()),
                Some(Err(e)) => Err(anyhow!(e)),
                None => Ok(()),
            }
        }
        async fn list(&self) -> Result<Vec<String>> {
            Ok(self.list_response.lock().unwrap().clone().unwrap_or_default())
        }
        async fn list_archived(&self) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_presets(&self) -> Result<serde_json::Value> {
            Ok(serde_json::json!({}))
        }
    }
}

// Preserve previous free-function for run_agent until the HTTP migration in
// later tasks; will be removed when POST /agents/{id}/run goes through
// /sessions.
pub async fn spawn(
    socket: &Path,
    preset: &str,
    model: Option<String>,
    system_prompt: Option<String>,
) -> Result<String> {
    UnixSocketDaemonClient::new(socket.to_path_buf())
        .spawn(SpawnRequest {
            agent: preset.into(),
            cwd: None,
            model,
            permission: None,
            system_prompt,
        })
        .await
}

pub async fn list_presets(socket: &Path) -> Result<serde_json::Value> {
    UnixSocketDaemonClient::new(socket.to_path_buf())
        .list_presets()
        .await
}
```

Note: `ClientCommand::Spawn { cwd, ... }` and `BTreeMap` import for `SpawnRequest` will resolve **after Phase 3** lands. Before then this code does **not** compile. Continue with Phase 3 immediately.

- [ ] **Step 3: Skip build verification — it will fail until Phase 3**

This task is intentionally stage-coupled with Phase 3 because the wire-protocol changes. Commit and proceed.

- [ ] **Step 4: Commit**

```bash
git add crates/roy-management/src/roy_client.rs crates/roy-management/Cargo.toml
git commit -m "wip(roy-management): DaemonClient trait + mock (compiles after Phase 3)"
```

---

## Phase 3 — Wire-protocol break (atomic switch)

This phase is **one logical big-bang** broken into ordered tasks. Build does not pass between tasks within this phase. Final task verifies green build.

### Task 9: Remove SetTags / ListProjects / CreateProject / DeleteProject from ClientCommand + ServerEvent

**Files:**
- Modify: `crates/roy/src/control.rs`

- [ ] **Step 1: Delete enum variants in `ClientCommand`**

Remove these entire variants (and their doc comments):
- `SetTags { session, tags }`
- `ListProjects`
- `CreateProject { name }`
- `DeleteProject { project_id }`

- [ ] **Step 2: Delete enum variants in `ServerEvent`**

Remove:
- `ProjectsListed { projects }`
- `ProjectCreated { project }`
- `ProjectDeleted { project_id, deleted_sessions }`
- `SessionUpdated { session, model, tags }`

- [ ] **Step 3: Delete `ErrorCode` variants**

Remove `CreateProjectFailed`, `DeleteProjectFailed`, `InvalidProjectName`, `ProjectExists` from `ErrorCode` enum and the two str-roundtrip match arms.

- [ ] **Step 4: Drop `tags` field from `ClientCommand::Resume`**

Change:
```rust
Resume {
    session: String,
    // (removed) tags: Option<BTreeMap<String, String>>,
}
```

Resume tests must drop the `tags: None` from their constructor calls.

- [ ] **Step 5: Drop `project_id` field from `FireTarget::Spawn`**

```rust
pub enum FireTarget {
    Spawn {
        preset: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<String>,
    },
    Resume { session_id: String },
}
```

- [ ] **Step 6: Delete the dead `Project` import**

Remove `use crate::project::Project;` (or qualified usage) wherever it sits in `control.rs`. The serde-roundtrip tests for project-related variants must also be deleted.

- [ ] **Step 7: Delete serde roundtrip tests for removed variants**

In the `#[cfg(test)] mod tests` block, delete any test invoking `ClientCommand::SetTags`, `ListProjects`, `CreateProject`, `DeleteProject`, `ServerEvent::ProjectsListed`, `ProjectCreated`, `ProjectDeleted`, `SessionUpdated`.

- [ ] **Step 8: Build (will still fail due to handlers + project.rs)**

Run: `cargo build -p roy` — expected to fail until Task 11.

- [ ] **Step 9: Commit**

```bash
git add crates/roy/src/control.rs
git commit -m "wire: remove SetTags/Project commands and SessionUpdated/Project events"
```

---

### Task 10: Change `Spawn` and `Spawned`/`Spawning`/`SessionRecord` — project_id → cwd

**Files:**
- Modify: `crates/roy/src/control.rs`

- [ ] **Step 1: Replace `Spawn` shape**

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
},
```

Note: `tags: BTreeMap<...>` is removed; `project_id` replaced by `cwd: Option<PathBuf>`. Add `use std::path::PathBuf;` if not present.

- [ ] **Step 2: Drop `project_id` from `ServerEvent::Spawned` and `Spawning`**

```rust
Spawned {
    session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resume_cursor: Option<String>,
},
Spawning { agent: String },
```

- [ ] **Step 3: Drop `project_id` from `SessionRecord` (or equivalent `SessionInfo`)**

Find `SessionInfo` struct (line ~452 in control.rs) — delete `pub project_id: Option<String>` field and any `#[serde]` attr.

- [ ] **Step 4: Update remaining serde-roundtrip tests**

Replace `project_id: Some("...")` / `project_id: None` in all surviving tests with `cwd: Some(PathBuf::from("/tmp"))` / `cwd: None`. Replace `tags: BTreeMap::new()` with — well, just remove that line from `Spawn` constructions.

- [ ] **Step 5: Commit**

```bash
git add crates/roy/src/control.rs
git commit -m "wire: Spawn{project_id}->Spawn{cwd}; drop project_id from Spawned/Spawning/SessionInfo"
```

---

### Task 11: Delete project.rs and untangle SessionManager

**Files:**
- Delete: `crates/roy/src/project.rs`
- Modify: `crates/roy/src/lib.rs` (remove `pub mod project;`)
- Modify: `crates/roy/src/manager.rs`

- [ ] **Step 1: Remove module declaration**

In `crates/roy/src/lib.rs` delete `pub mod project;`.

- [ ] **Step 2: Delete project.rs**

```bash
rm crates/roy/src/project.rs
```

- [ ] **Step 3: Rewrite `SessionManager` to drop projects**

Modify `crates/roy/src/manager.rs`:

1. Remove `use crate::project::ProjectRegistry;` and `use crate::session_meta::read_metadata;`
2. Add `use crate::session_store::{SessionStore, SessionRow};`
3. Drop `projects: Arc<ProjectRegistry>` field from `SessionManager` struct
4. Drop `projects` from `new` constructor body
5. Remove `pub fn projects(&self)` accessor
6. Remove `index_existing_sessions` method entirely
7. Rewrite `spawn`, `resume`, `list_archived`, `delete_archive`, `resume_all` to use `SessionStore` instead of file scanning / project registry. Reference implementations:

```rust
pub async fn spawn(
    &self,
    cfg: SessionSpawnConfig,
    broadcast_capacity: usize,
    mem_capacity: usize,
) -> Result<Arc<SessionEngine>> {
    let cwd = match cfg.cwd.clone() {
        Some(p) => p,
        None => {
            // Orphan: allocate <workspace>/<session_id>/ — engine generates id
            // but we need it before we mkdir. Fixed_session_id route handles it
            // (see engine.rs).
            let sid = cfg
                .fixed_session_id
                .clone()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let path = self.workspace_dir.join(&sid);
            std::fs::create_dir_all(&path).map_err(RoyError::Io)?;
            // Stash the id back into cfg for the engine.
            let mut cfg = cfg.clone();
            cfg.fixed_session_id = Some(sid);
            cfg.cwd = Some(path);
            return self.spawn_internal(cfg, broadcast_capacity, mem_capacity).await;
        }
    };
    let cfg = SessionSpawnConfig { cwd: Some(cwd), ..cfg };
    self.spawn_internal(cfg, broadcast_capacity, mem_capacity).await
}

async fn spawn_internal(
    &self,
    cfg: SessionSpawnConfig,
    broadcast_capacity: usize,
    mem_capacity: usize,
) -> Result<Arc<SessionEngine>> {
    let transport = self.factory.build(cfg.agent, cfg.model.as_deref(), cfg.permission.as_deref())?;
    let opts = EngineOpts {
        journal_dir: self.journal_dir.clone(),
        broadcast_capacity,
        mem_capacity,
    };
    let engine = SessionEngine::spawn(transport, opts, cfg, Arc::clone(&self.session_store)).await?;
    let id = engine.id().to_string();
    self.sessions.write().await.insert(id, Arc::clone(&engine));
    Ok(engine)
}
```

```rust
pub async fn resume(
    &self,
    session_id: &str,
    broadcast_capacity: usize,
    mem_capacity: usize,
) -> Result<Arc<SessionEngine>> {
    if self.sessions.read().await.contains_key(session_id) {
        return Err(RoyError::Protocol(format!(
            "session {session_id} is already live"
        )));
    }
    let row = self
        .session_store
        .get(session_id)
        .await?
        .ok_or_else(|| RoyError::Protocol(format!("no session: {session_id}")))?;
    let preset: crate::agents_config::AgentPreset = row.agent.parse().map_err(RoyError::Protocol)?;
    let cfg = SessionSpawnConfig {
        agent: preset,
        cwd: Some(row.cwd),
        model: row.model,
        permission: row.permission,
        resume_cursor: row.resume_cursor,
        fixed_session_id: Some(session_id.to_string()),
        system_prompt: row.system_prompt,
    };
    let transport = self
        .factory
        .build(cfg.agent, cfg.model.as_deref(), cfg.permission.as_deref())?;
    let opts = EngineOpts {
        journal_dir: self.journal_dir.clone(),
        broadcast_capacity,
        mem_capacity,
    };
    let engine = SessionEngine::resume(
        transport,
        opts,
        session_id.to_string(),
        cfg,
        Arc::clone(&self.session_store),
    )
    .await?;
    self.sessions.write().await.insert(session_id.into(), Arc::clone(&engine));
    Ok(engine)
}
```

```rust
pub async fn list_archived(&self) -> Result<Vec<String>> {
    let live: std::collections::HashSet<String> =
        self.sessions.read().await.keys().cloned().collect();
    let rows = self.session_store.list_archived().await?;
    Ok(rows
        .into_iter()
        .map(|r| r.session_id)
        .filter(|sid| !live.contains(sid))
        .collect())
}

pub async fn delete_archive(&self, id: &str) -> Result<()> {
    if self.sessions.read().await.contains_key(id) {
        return Err(RoyError::Protocol(format!(
            "session {id} is live — close it before deleting"
        )));
    }
    let jsonl = self.journal_dir.join(format!("{id}.jsonl"));
    match tokio::fs::remove_file(&jsonl).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(RoyError::Io(e)),
    }
    self.session_store.delete(id).await?;
    Ok(())
}
```

```rust
pub async fn close(&self, id: &str) -> Result<()> {
    let engine = self
        .sessions
        .write()
        .await
        .remove(id)
        .ok_or_else(|| RoyError::Protocol(format!("no such session: {id}")))?;
    self.session_store.mark_closed(id).await?;
    engine.close()
}
```

8. Update `SessionSpawnConfig` (in `engine.rs`) — remove `project_id`, `tags` fields. The fact that `spawn_internal` references `cfg.cwd: Option<PathBuf>` requires `cwd` to become `Option<PathBuf>` instead of `PathBuf` — Task 12 handles this.

- [ ] **Step 4: Commit**

```bash
git add crates/roy/src/lib.rs crates/roy/src/manager.rs
git rm crates/roy/src/project.rs
git commit -m "refactor(roy): drop ProjectRegistry; SessionManager talks to SessionStore"
```

---

### Task 12: Delete session_meta.rs and rewrite SessionEngine

**Files:**
- Delete: `crates/roy/src/session_meta.rs`
- Modify: `crates/roy/src/lib.rs`
- Modify: `crates/roy/src/engine.rs`

- [ ] **Step 1: Remove `pub mod session_meta;` from lib.rs**

- [ ] **Step 2: Delete the file**

```bash
rm crates/roy/src/session_meta.rs
```

- [ ] **Step 3: Rewrite `SessionEngine`**

In `crates/roy/src/engine.rs`:

1. Remove `use crate::session_meta::{write_metadata, SessionMetadata};`
2. Add `use crate::session_store::{SessionStore, SessionRow};`
3. Modify `SessionSpawnConfig`:

```rust
#[derive(Debug, Clone)]
pub struct SessionSpawnConfig {
    pub agent: crate::agents_config::AgentPreset,
    pub cwd: Option<PathBuf>,
    pub model: Option<String>,
    pub permission: Option<String>,
    pub resume_cursor: Option<String>,
    pub fixed_session_id: Option<String>,
    pub system_prompt: Option<String>,
}
```

4. Update `SessionEngine` struct: remove `project_id: Option<String>`, `tags: StdMutex<BTreeMap<String, String>>`. Add `session_store: Arc<SessionStore>`.

5. Update `spawn` and `resume` signatures to accept `Arc<SessionStore>`:

```rust
pub async fn spawn(
    transport: Arc<dyn Transport>,
    opts: EngineOpts,
    cfg: SessionSpawnConfig,
    session_store: Arc<SessionStore>,
) -> Result<Arc<Self>> { /* ... */ }

pub async fn resume(
    transport: Arc<dyn Transport>,
    opts: EngineOpts,
    session_id: String,
    cfg: SessionSpawnConfig,
    session_store: Arc<SessionStore>,
) -> Result<Arc<Self>> { /* ... */ }
```

6. Replace `write_metadata(...)` call in `start()` with:

```rust
let row = SessionRow {
    session_id: session_id.clone(),
    agent: cfg.agent.to_string(),
    cwd: cfg.cwd.clone().expect("manager sets cwd before engine"),
    model: cfg.model.clone(),
    permission: cfg.permission.clone(),
    resume_cursor: initial_cursor.clone(),
    system_prompt: cfg.system_prompt.clone(),
    created_at: chrono::Utc::now().timestamp(),
    closed_at: None,
};
session_store.insert(&row).await?;
```

(But: in `resume`, the row already exists — skip insert, and don't override.)

7. Rewrite `persist_metadata` to use `update_cursor`/`update_model`:

```rust
async fn persist_cursor(&self) -> Result<()> {
    let cursor = self.resume_cursor.lock().unwrap().clone();
    self.session_store
        .update_cursor(&self.session_id, cursor.as_deref())
        .await
}

// In set_model:
pub async fn set_model(&self, model: String) -> Result<String> {
    *self.model.lock().unwrap() = Some(model.clone());
    self.session_store
        .update_model(&self.session_id, Some(&model))
        .await?;
    // publish System event as before
    ...
}
```

8. Remove `pub fn tags()`, `pub async fn set_tags()`, `metadata_snapshot()`, and the `project_id()` accessor. Remove their callers in this file.

- [ ] **Step 4: Commit**

```bash
git add crates/roy/src/lib.rs crates/roy/src/engine.rs
git rm crates/roy/src/session_meta.rs
git commit -m "refactor(roy): SessionEngine writes to SessionStore; drop SessionMetadata"
```

---

### Task 13: Update daemon handlers — drop project commands, fix Spawn handler

**Files:**
- Modify: `crates/roy/src/daemon.rs`

- [ ] **Step 1: Delete handlers for removed commands**

Search and delete:
- `handle_list_projects`, `handle_create_project`, `handle_delete_project`
- `handle_set_tags`
- Match arms in the main command dispatcher for `ClientCommand::SetTags / ListProjects / CreateProject / DeleteProject`

- [ ] **Step 2: Rewrite `handle_spawn`**

The handler now takes `cwd: Option<PathBuf>` directly. If `None`, manager allocates orphan dir.

```rust
ClientCommand::Spawn {
    agent, cwd, model, permission, resume, system_prompt,
} => {
    let preset = agent.parse().map_err(...)?;
    let cfg = SessionSpawnConfig {
        agent: preset,
        cwd,
        model,
        permission,
        resume_cursor: resume,
        fixed_session_id: None,
        system_prompt,
    };
    self.send_event(ServerEvent::Spawning { agent: agent.clone() }).await?;
    let engine = self.manager.spawn(cfg, BROADCAST_CAP, MEM_CAP).await?;
    self.send_event(ServerEvent::Spawned {
        session: engine.id().into(),
        resume_cursor: engine.resume_cursor(),
    }).await?;
}
```

- [ ] **Step 3: Pass SessionStore into manager construction**

Daemon's `run_with_opts` (or `new`) must open `SessionStore` at startup:

```rust
let store_path = ServeOpts::sessions_db_path(&opts);  // default ~/.local/state/roy/sessions.db
let session_store = Arc::new(SessionStore::open(&store_path).await?);
let manager = SessionManager::new(
    journal_dir,
    workspace_dir,
    factory,
    session_store,
).await?;
```

Add to `ServeOpts`:

```rust
pub sessions_db: Option<PathBuf>,  // default via SessionStore::default_db_path()
```

- [ ] **Step 4: Delete `Resume { tags }` handler tag-merge code**

Replace any block that merged `tags` into engine config — just drop it.

- [ ] **Step 5: Build the world**

Run: `cargo build --workspace --all-targets`
Expected: PASS

This is the green-build checkpoint of Phase 3.

- [ ] **Step 6: Run all tests**

Run: `cargo test --workspace --no-fail-fast`
Expected: Some failures in `manager.rs` tests + `daemon` integration tests (they reference removed fields/types) — Task 14 fixes them.

- [ ] **Step 7: Commit**

```bash
git add crates/roy/src/daemon.rs
git commit -m "refactor(roy daemon): handle_spawn takes cwd; drop project/setTags handlers"
```

---

### Task 14: Fix internal tests

**Files:**
- Modify: `crates/roy/src/manager.rs` (`#[cfg(test)] mod tests`)
- Modify: `crates/roy/src/daemon.rs` (`#[cfg(test)] mod tests`)
- Modify: `crates/roy/src/control.rs` (`#[cfg(test)] mod tests`)
- Modify: `crates/roy/tests/acp_transport.rs`

- [ ] **Step 1: Update `manager.rs` tests**

In each test:
- Drop project-registry tests (`registry_lifecycle` with projects, `index_existing_sessions_rebuilds_project_membership`).
- Update `orphan_cfg(...)` helper: remove `project_id: None` and `tags: ...` fields:

```rust
fn orphan_cfg(agent: AgentPreset) -> SessionSpawnConfig {
    SessionSpawnConfig {
        agent,
        cwd: None,
        model: None,
        permission: None,
        resume_cursor: None,
        fixed_session_id: None,
        system_prompt: None,
    }
}
```

- Update `new_mgr` to be async, take `dir`, and create `SessionStore`:

```rust
async fn new_mgr(dir: &PathBuf) -> SessionManager {
    let store = Arc::new(
        SessionStore::open(&dir.join("sessions.db")).await.unwrap(),
    );
    SessionManager::new(dir.clone(), dir.join("workspace"), Arc::new(FakeFactory), store)
        .await
        .expect("manager")
}
```

Convert callers to `.await`.

- [ ] **Step 2: Update daemon.rs tests**

Similar surgery: any test referencing `project_id`, `tags`, `SetTags`, or `Project*` events must be deleted or rewritten without those fields. Use grep:

```bash
grep -n "project_id\|SetTags\|ProjectsListed\|CreateProject\|DeleteProject\|SessionUpdated" crates/roy/src/daemon.rs
```

For each match, decide: delete test, or remove that field assertion.

- [ ] **Step 3: Update control.rs serde tests**

Remove project_id from `Spawn` constructors; remove `tags: BTreeMap::new()`. Drop `tags: None` from `Resume`.

- [ ] **Step 4: Update integration tests**

`crates/roy/tests/acp_transport.rs` — same grep, same surgery.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test --workspace --no-fail-fast`
Expected: PASS (all crates green)

- [ ] **Step 6: Commit**

```bash
git add crates/roy/src/manager.rs crates/roy/src/daemon.rs crates/roy/src/control.rs crates/roy/tests/acp_transport.rs
git commit -m "test: refresh roy tests for split-store"
```

---

## Phase 4 — Management HTTP coordinator

### Task 15: Wire MetaStore into AppState; apply migrations on startup

**Files:**
- Modify: `crates/roy-management/src/state.rs`
- Modify: `crates/roy-management/src/lib.rs` (apply migrations on startup)
- Modify: `crates/roy-management/src/http.rs` (no behavioural change yet)

- [ ] **Step 1: Update AppState**

```rust
use std::sync::Arc;
use std::path::PathBuf;

use roy_agents::Store;

use crate::meta_store::MetaStore;
use crate::roy_client::DaemonClient;

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub meta: MetaStore,
    pub daemon: Arc<dyn DaemonClient>,
    pub socket_path: PathBuf,  // kept for `list_presets` until /presets refactored
}
```

- [ ] **Step 2: Apply migrations in `lib.rs` startup**

After `let pool = roy_agents::open(&db_path).await?;`:

```rust
MetaStore::apply_migrations(&pool).await?;
let meta = MetaStore::new(pool.clone());
let daemon: Arc<dyn DaemonClient> =
    Arc::new(UnixSocketDaemonClient::new(args.socket.clone()));
let state = AppState {
    store: Store::new(pool),
    meta,
    daemon,
    socket_path: args.socket.clone(),
};
```

- [ ] **Step 3: Update tests in http.rs**

The test helper `test_state` must build `meta` + `daemon` (mock):

```rust
async fn test_state() -> AppState {
    let dir = tempfile::tempdir().unwrap();
    let pool = roy_agents::open(&dir.path().join("agents.db")).await.unwrap();
    MetaStore::apply_migrations(&pool).await.unwrap();
    std::mem::forget(dir);
    AppState {
        store: roy_agents::Store::new(pool.clone()),
        meta: MetaStore::new(pool),
        daemon: Arc::new(crate::roy_client::mock::MockDaemonClient::new()),
        socket_path: "/nonexistent.sock".into(),
    }
}
```

- [ ] **Step 4: Build & test**

Run: `cargo test -p roy-management`
Expected: PASS (existing agent tests still green)

- [ ] **Step 5: Commit**

```bash
git add crates/roy-management/src/state.rs crates/roy-management/src/lib.rs crates/roy-management/src/http.rs
git commit -m "feat(roy-management): wire MetaStore + DaemonClient into AppState"
```

---

### Task 16: HTTP — `GET/POST/DELETE /projects`

**Files:**
- Modify: `crates/roy-management/src/http.rs`

- [ ] **Step 1: Add tests**

Append to `mod tests`:

```rust
#[tokio::test]
async fn projects_create_list_delete() {
    let app = router(test_state().await);
    // create
    let resp = app
        .clone()
        .oneshot(
            Request::post("/projects")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&json!({"name":"p1"})).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let p: crate::meta_store::Project =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(p.name, "p1");

    // list
    let resp = app
        .clone()
        .oneshot(Request::get("/projects").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let listed: Vec<crate::meta_store::Project> =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(listed.len(), 1);

    // duplicate is 409
    let dup = app
        .clone()
        .oneshot(
            Request::post("/projects")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&json!({"name":"p1"})).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(dup.status(), StatusCode::CONFLICT);

    // delete
    let del = app
        .oneshot(
            Request::delete(format!("/projects/{}", p.id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::NO_CONTENT);
}
```

- [ ] **Step 2: Add routes + handlers**

In `router` add:
```rust
.route("/projects", get(list_projects).post(create_project))
.route("/projects/{id}", axum::routing::delete(delete_project))
```

Add handlers:

```rust
async fn list_projects(
    State(s): State<AppState>,
) -> Result<Json<Vec<crate::meta_store::Project>>, ApiError> {
    s.meta.list_projects().await.map(Json).map_err(meta_to_api)
}

#[derive(serde::Deserialize)]
struct NewProject { name: String }

async fn create_project(
    State(s): State<AppState>,
    Json(req): Json<NewProject>,
) -> Result<(StatusCode, Json<crate::meta_store::Project>), ApiError> {
    let p = s.meta.create_project(&req.name).await.map_err(meta_to_api)?;
    Ok((StatusCode::CREATED, Json(p)))
}

async fn delete_project(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    s.meta.delete_project(&id).await.map_err(meta_to_api)?;
    Ok(StatusCode::NO_CONTENT)
}

fn meta_to_api(e: crate::meta_store::MetaError) -> ApiError {
    use crate::meta_store::MetaError::*;
    match e {
        NotFound(m) => ApiError(StatusCode::NOT_FOUND, m),
        Conflict(m) => ApiError(StatusCode::CONFLICT, m),
        Invalid(m) => ApiError(StatusCode::BAD_REQUEST, m),
        Db(e) => {
            tracing::error!(error=%e, "meta db error");
            ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
        }
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p roy-management http::tests::projects_create_list_delete`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/roy-management/src/http.rs
git commit -m "feat(roy-management): GET/POST/DELETE /projects"
```

---

### Task 17: HTTP — `POST /sessions` coordinator (happy + rollback paths)

**Files:**
- Modify: `crates/roy-management/src/http.rs`

- [ ] **Step 1: Tests (happy + rollback)**

```rust
#[tokio::test]
async fn sessions_post_happy_path() {
    let mut st = test_state().await;
    let mock = Arc::new(crate::roy_client::mock::MockDaemonClient::new().with_spawn("sid-1"));
    st.daemon = mock.clone();
    let app = router(st);

    let body = serde_json::to_vec(&json!({
        "agent": "claude",
        "tags": {"env": "prod"},
        "agent_name": "Reviewer"
    })).unwrap();
    let resp = app
        .oneshot(
            Request::post("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let v: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["session_id"], "sid-1");

    // mock recorded one spawn
    assert_eq!(mock.recorded_spawns.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn sessions_post_rollback_on_meta_failure() {
    // To force meta failure: pre-insert a session_meta row with the same id
    // that the mock will return.
    let mut st = test_state().await;
    st.meta
        .upsert_session_meta(&crate::meta_store::SessionMeta {
            session_id: "dup-sid".into(),
            project_id: None,
            agent_id: None,
            agent_name: None,
            display_label: None,
            tags: BTreeMap::new(),
            created_at: 1,
        })
        .await
        .unwrap();
    let mock = Arc::new(crate::roy_client::mock::MockDaemonClient::new().with_spawn("dup-sid"));
    st.daemon = mock.clone();
    let app = router(st);

    let body = serde_json::to_vec(&json!({"agent":"claude"})).unwrap();
    let resp = app
        .oneshot(
            Request::post("/sessions")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

    // Mock recorded the compensating Close
    assert_eq!(mock.recorded_closes.lock().unwrap().as_slice(), &["dup-sid"]);
}
```

**Reliable way to force meta-persist failure in test:** the `upsert_session_meta` uses `INSERT ... ON CONFLICT DO UPDATE` for `session_meta`, which won't fail on dup. To force a failure, pass an `agent_id` that does not exist in `agents` — the `REFERENCES agents(id)` FK will reject the INSERT. Update the rollback test:

```rust
let body = serde_json::to_vec(&json!({
    "agent": "claude",
    "agent_name": "X",
    // No agent_id allowed in CreateSessionReq currently; instead patch the
    // test to fail differently — kill the pool before insert. Simpler:
    // pre-fill session_tags with PK (sid, "k") then send tags={"k":"v2"} —
    // the replace_tags DELETE+INSERT inside the tx will succeed, so that
    // does not fail either.
})).unwrap();
```

Practical alternative: temporarily close the pool to force a write error.

```rust
let mut st = test_state().await;
let mock = Arc::new(crate::roy_client::mock::MockDaemonClient::new().with_spawn("sid-X"));
st.daemon = mock.clone();
// Close the pool so the next insert fails
st.meta.pool.close().await;
let app = router(st);

let body = serde_json::to_vec(&json!({"agent":"claude"})).unwrap();
let resp = app
    .oneshot(
        Request::post("/sessions")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap(),
    )
    .await
    .unwrap();
assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
assert_eq!(mock.recorded_closes.lock().unwrap().as_slice(), &["sid-X"]);
```

This requires `MetaStore.pool` to be `pub(crate)` — add visibility modifier in `meta_store.rs`.

- [ ] **Step 2: Add route + handler**

```rust
.route("/sessions", get(list_sessions).post(create_session))
```

```rust
#[derive(serde::Deserialize, Default)]
struct CreateSessionReq {
    agent: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    permission: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    agent_name: Option<String>,
    #[serde(default)]
    tags: BTreeMap<String, String>,
}

async fn create_session(
    State(s): State<AppState>,
    Json(req): Json<CreateSessionReq>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    // Resolve project_id -> cwd if needed
    let cwd: Option<PathBuf> = if let Some(pid) = &req.project_id {
        let projects = s.meta.list_projects().await.map_err(meta_to_api)?;
        let p = projects
            .into_iter()
            .find(|p| &p.id == pid)
            .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, format!("invalid project: {pid}")))?;
        Some(PathBuf::from(p.path))
    } else {
        req.cwd.clone().map(PathBuf::from)
    };

    let sid = s.daemon
        .spawn(crate::roy_client::SpawnRequest {
            agent: req.agent.clone(),
            cwd,
            model: req.model.clone(),
            permission: req.permission.clone(),
            system_prompt: req.system_prompt.clone(),
        })
        .await
        .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("daemon: {e}")))?;

    let meta = crate::meta_store::SessionMeta {
        session_id: sid.clone(),
        project_id: req.project_id.clone(),
        agent_id: None,
        agent_name: req.agent_name.clone(),
        display_label: None,
        tags: req.tags.clone(),
        created_at: chrono::Utc::now().timestamp(),
    };
    if let Err(meta_err) = s.meta.upsert_session_meta(&meta).await {
        tracing::error!(?meta_err, "meta persist failed; closing session");
        let _ = s.daemon.close(&sid).await;
        return Err(ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("meta_persist_failed; session was created and closed: {sid}"),
        ));
    }

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "session_id": sid,
            "project_id": req.project_id,
            "tags": req.tags,
            "agent_name": req.agent_name,
        })),
    ))
}
```

- [ ] **Step 3: Stub `list_sessions` (real impl in Task 18)**

```rust
async fn list_sessions(State(_): State<AppState>) -> Json<Vec<serde_json::Value>> {
    Json(vec![])  // Task 18
}
```

- [ ] **Step 4: Build & test**

Run: `cargo test -p roy-management http::tests::sessions_post_happy_path`
Run: `cargo test -p roy-management http::tests::sessions_post_rollback_on_meta_failure`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/roy-management/src/http.rs
git commit -m "feat(roy-management): POST /sessions coordinator with rollback"
```

---

### Task 18: HTTP — `GET /sessions` and `GET /sessions/{id}`

**Files:**
- Modify: `crates/roy-management/src/http.rs`

- [ ] **Step 1: Add tests**

```rust
#[tokio::test]
async fn list_sessions_joins_live_and_meta() {
    let mut st = test_state().await;
    let mut spawn = Mutex::new(Some(Ok("sid-A".into())));
    let mock = Arc::new(crate::roy_client::mock::MockDaemonClient {
        spawn_response: Mutex::new(Some(Ok("sid-A".into()))),
        list_response: Mutex::new(Some(vec!["sid-A".into(), "sid-B".into()])),
        ..Default::default()
    });
    st.daemon = mock;
    // Pre-insert meta for sid-A only; sid-B is orphan
    st.meta
        .upsert_session_meta(&crate::meta_store::SessionMeta {
            session_id: "sid-A".into(),
            project_id: None,
            agent_id: None,
            agent_name: Some("Rev".into()),
            display_label: None,
            tags: BTreeMap::from([("k".into(), "v".into())]),
            created_at: 1,
        })
        .await
        .unwrap();
    let app = router(st);
    let resp = app
        .oneshot(Request::get("/sessions").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    let a = arr.iter().find(|r| r["session_id"] == "sid-A").unwrap();
    assert_eq!(a["agent_name"], "Rev");
    let b = arr.iter().find(|r| r["session_id"] == "sid-B").unwrap();
    assert_eq!(b["agent_name"], serde_json::Value::Null);
}
```

- [ ] **Step 2: Implement `list_sessions` + add `/sessions/{id}` route**

```rust
async fn list_sessions(State(s): State<AppState>) -> Result<Json<serde_json::Value>, ApiError> {
    let live = s.daemon.list().await.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e.to_string()))?;
    let archived = s.daemon.list_archived().await.unwrap_or_default();
    let mut sids: Vec<String> = live.iter().cloned().chain(archived.iter().cloned()).collect();
    sids.sort();
    sids.dedup();

    let metas = s.meta.list_session_metas(&sids).await.map_err(meta_to_api)?;
    let meta_by_sid: std::collections::HashMap<String, _> =
        metas.into_iter().map(|m| (m.session_id.clone(), m)).collect();

    let out: Vec<serde_json::Value> = sids
        .into_iter()
        .map(|sid| {
            let m = meta_by_sid.get(&sid);
            serde_json::json!({
                "session_id": sid,
                "project_id": m.and_then(|m| m.project_id.clone()),
                "agent_name": m.and_then(|m| m.agent_name.clone()),
                "tags": m.map(|m| m.tags.clone()).unwrap_or_default(),
                "live": live.contains(&sid),
            })
        })
        .collect();
    Ok(Json(serde_json::Value::Array(out)))
}

async fn get_session(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let meta = s.meta.get_session_meta(&id).await.map_err(meta_to_api)?;
    let live = s.daemon.list().await.unwrap_or_default();
    Ok(Json(serde_json::json!({
        "session_id": id,
        "meta": meta,
        "live": live.contains(&id),
    })))
}
```

Update router:
```rust
.route("/sessions/{id}", get(get_session))
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p roy-management http::tests::list_sessions_joins_live_and_meta`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/roy-management/src/http.rs
git commit -m "feat(roy-management): GET /sessions and /sessions/{id}"
```

---

### Task 19: HTTP — `PUT /sessions/{id}/tags` and `PATCH /sessions/{id}`

**Files:**
- Modify: `crates/roy-management/src/http.rs`

- [ ] **Step 1: Tests**

```rust
#[tokio::test]
async fn put_tags_replaces() {
    let st = test_state().await;
    st.meta
        .upsert_session_meta(&crate::meta_store::SessionMeta {
            session_id: "sid".into(),
            project_id: None,
            agent_id: None,
            agent_name: None,
            display_label: None,
            tags: BTreeMap::from([("old".into(), "1".into())]),
            created_at: 1,
        })
        .await
        .unwrap();
    let app = router(st.clone());

    let body = serde_json::to_vec(&json!({"tags": {"new": "2"}})).unwrap();
    let resp = app
        .oneshot(
            Request::builder()
                .method(axum::http::Method::PUT)
                .uri("/sessions/sid/tags")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let back = st.meta.get_session_meta("sid").await.unwrap().unwrap();
    assert_eq!(back.tags, BTreeMap::from([("new".into(), "2".into())]));
}
```

- [ ] **Step 2: Implement**

```rust
#[derive(serde::Deserialize)]
struct TagsBody { tags: BTreeMap<String, String> }

async fn put_tags(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<TagsBody>,
) -> Result<StatusCode, ApiError> {
    s.meta.replace_tags(&id, &body.tags).await.map_err(meta_to_api)?;
    Ok(StatusCode::OK)
}

#[derive(serde::Deserialize)]
struct PatchSession {
    #[serde(default)] agent_name: Option<String>,
    #[serde(default)] display_label: Option<String>,
}

async fn patch_session(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<PatchSession>,
) -> Result<StatusCode, ApiError> {
    // Read existing → merge → upsert
    let mut meta = s.meta.get_session_meta(&id).await.map_err(meta_to_api)?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("session: {id}")))?;
    if body.agent_name.is_some()    { meta.agent_name    = body.agent_name; }
    if body.display_label.is_some() { meta.display_label = body.display_label; }
    s.meta.upsert_session_meta(&meta).await.map_err(meta_to_api)?;
    Ok(StatusCode::OK)
}
```

Update router:
```rust
.route("/sessions/{id}/tags", axum::routing::put(put_tags))
.route("/sessions/{id}", get(get_session).patch(patch_session))
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p roy-management http::tests::put_tags_replaces`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/roy-management/src/http.rs
git commit -m "feat(roy-management): PUT /sessions/{id}/tags + PATCH /sessions/{id}"
```

---

### Task 20: Orphan-row sweep background task

**Files:**
- Create: `crates/roy-management/src/orphan_sweep.rs`
- Modify: `crates/roy-management/src/lib.rs`

- [ ] **Step 1: Implement sweep**

`crates/roy-management/src/orphan_sweep.rs`:

```rust
use std::sync::Arc;
use std::time::Duration;

use crate::meta_store::MetaStore;
use crate::roy_client::DaemonClient;

/// Periodically delete `session_meta` rows whose `session_id` is not present
/// in core (neither live nor archived). Off via `ROY_MGMT_ORPHAN_SWEEP=off`.
pub fn spawn(meta: MetaStore, daemon: Arc<dyn DaemonClient>) {
    if std::env::var("ROY_MGMT_ORPHAN_SWEEP").as_deref() == Ok("off") {
        return;
    }
    tokio::spawn(async move {
        let interval = Duration::from_secs(600);
        loop {
            tokio::time::sleep(interval).await;
            if let Err(e) = run_once(&meta, &*daemon).await {
                tracing::warn!(error = %e, "orphan_sweep iteration failed");
            }
        }
    });
}

async fn run_once(meta: &MetaStore, daemon: &dyn DaemonClient) -> anyhow::Result<()> {
    let live = daemon.list().await?;
    let archived = daemon.list_archived().await?;
    let known: std::collections::HashSet<String> = live.into_iter().chain(archived).collect();
    let all_metas = meta.list_session_metas(&[]).await?;  // (will require a list_all override)
    for m in all_metas {
        if !known.contains(&m.session_id) {
            tracing::info!(session = %m.session_id, "sweeping orphan management row");
            let _ = meta.delete_session_meta(&m.session_id).await;
        }
    }
    Ok(())
}
```

Note: `list_session_metas(&[])` should return everything. Add to MetaStore:

```rust
pub async fn list_all_session_metas(&self) -> Result<Vec<SessionMeta>, MetaError> {
    let rows: Vec<(String,)> =
        sqlx::query_as("SELECT session_id FROM session_meta")
            .fetch_all(&self.pool).await?;
    let ids: Vec<String> = rows.into_iter().map(|r| r.0).collect();
    self.list_session_metas(&ids).await
}
```

Replace the call in `orphan_sweep::run_once` with `meta.list_all_session_metas()`.

- [ ] **Step 2: Wire it up**

In `crates/roy-management/src/lib.rs` after `state` is built:

```rust
crate::orphan_sweep::spawn(state.meta.clone(), Arc::clone(&state.daemon));
```

And in `lib.rs` add `pub mod orphan_sweep;`.

- [ ] **Step 3: Unit test for `run_once`**

In `orphan_sweep.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta_store::SessionMeta;
    use crate::roy_client::mock::MockDaemonClient;
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn deletes_unknown_session_meta() {
        let dir = tempfile::tempdir().unwrap();
        let pool = roy_agents::open(&dir.path().join("agents.db")).await.unwrap();
        MetaStore::apply_migrations(&pool).await.unwrap();
        let meta = MetaStore::new(pool);
        meta.upsert_session_meta(&SessionMeta {
            session_id: "ghost".into(),
            project_id: None, agent_id: None, agent_name: None,
            display_label: None, tags: BTreeMap::new(), created_at: 1,
        }).await.unwrap();
        meta.upsert_session_meta(&SessionMeta {
            session_id: "alive".into(),
            project_id: None, agent_id: None, agent_name: None,
            display_label: None, tags: BTreeMap::new(), created_at: 1,
        }).await.unwrap();

        let mock = MockDaemonClient {
            list_response: std::sync::Mutex::new(Some(vec!["alive".into()])),
            ..Default::default()
        };
        run_once(&meta, &mock).await.unwrap();
        assert!(meta.get_session_meta("ghost").await.unwrap().is_none());
        assert!(meta.get_session_meta("alive").await.unwrap().is_some());
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p roy-management orphan_sweep::tests`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/roy-management/src/orphan_sweep.rs crates/roy-management/src/lib.rs crates/roy-management/src/meta_store.rs
git commit -m "feat(roy-management): orphan-row sweep background task"
```

---

## Phase 5 — CLI integration

### Task 21: Add reqwest to roy-cli + Management HTTP client module

**Files:**
- Modify: `crates/roy-cli/Cargo.toml`
- Create: `crates/roy-cli/src/management.rs`
- Modify: `crates/roy-cli/src/main.rs` (add `mod management;`)

- [ ] **Step 1: Add reqwest dep**

```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
```

- [ ] **Step 2: Create `crates/roy-cli/src/management.rs`**

```rust
//! Thin HTTP client to roy-management for project/tag-aware commands.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub fn url() -> String {
    std::env::var("ROY_MANAGEMENT_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8079".to_string())
}

#[derive(Debug, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Serialize, Default)]
pub struct CreateSessionReq {
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tags: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct CreatedSession {
    pub session_id: String,
}

pub async fn list_projects() -> Result<Vec<Project>> {
    let resp = reqwest::Client::new()
        .get(format!("{}/projects", url()))
        .send().await.context("GET /projects")?;
    if !resp.status().is_success() {
        return Err(anyhow!("management {}: {}", resp.status(), resp.text().await?));
    }
    Ok(resp.json().await?)
}

pub async fn create_project(name: &str) -> Result<Project> {
    let resp = reqwest::Client::new()
        .post(format!("{}/projects", url()))
        .json(&serde_json::json!({"name": name}))
        .send().await?;
    if !resp.status().is_success() {
        return Err(anyhow!("management {}: {}", resp.status(), resp.text().await?));
    }
    Ok(resp.json().await?)
}

pub async fn delete_project(id: &str) -> Result<()> {
    let resp = reqwest::Client::new()
        .delete(format!("{}/projects/{}", url(), id))
        .send().await?;
    if !resp.status().is_success() {
        return Err(anyhow!("management {}: {}", resp.status(), resp.text().await?));
    }
    Ok(())
}

pub async fn create_session(req: CreateSessionReq) -> Result<CreatedSession> {
    let resp = reqwest::Client::new()
        .post(format!("{}/sessions", url()))
        .json(&req)
        .send().await?;
    if !resp.status().is_success() {
        return Err(anyhow!("management {}: {}", resp.status(), resp.text().await?));
    }
    Ok(resp.json().await?)
}

pub async fn put_tags(session_id: &str, tags: &BTreeMap<String, String>) -> Result<()> {
    let resp = reqwest::Client::new()
        .put(format!("{}/sessions/{}/tags", url(), session_id))
        .json(&serde_json::json!({"tags": tags}))
        .send().await?;
    if !resp.status().is_success() {
        return Err(anyhow!("management {}: {}", resp.status(), resp.text().await?));
    }
    Ok(())
}
```

- [ ] **Step 3: Add `mod management;` to main.rs**

- [ ] **Step 4: Build**

Run: `cargo build -p roy-cli`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/roy-cli/Cargo.toml crates/roy-cli/src/management.rs crates/roy-cli/src/main.rs
git commit -m "feat(roy-cli): management HTTP client module"
```

---

### Task 22: Migrate `roy projects` commands and `roy set-tags` to management

**Files:**
- Modify: `crates/roy-cli/src/main.rs`

- [ ] **Step 1: Locate `Projects` subcommand handler**

```bash
grep -n "ProjectsList\|Projects {\|set_tags\|SetTags" crates/roy-cli/src/main.rs
```

- [ ] **Step 2: Replace handlers**

Where the handler currently sends `ClientCommand::ListProjects` over Unix socket, replace with:

```rust
let projects = crate::management::list_projects().await?;
for p in projects {
    println!("{}\t{}\t{}", p.id, p.name, p.path);
}
```

Similarly:
- `CreateProject { name }` → `crate::management::create_project(&name).await?`
- `DeleteProject { id }` → `crate::management::delete_project(&id).await?`
- `SetTags { session, tags }` → `crate::management::put_tags(&session, &tags).await?`

Drop the old `ClientCommand` constructors entirely.

- [ ] **Step 3: Build & smoke**

Run: `cargo build -p roy-cli`
Expected: PASS

Run unit tests (no behavioural tests for CLI args usually):
Run: `cargo test -p roy-cli`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/roy-cli/src/main.rs
git commit -m "feat(roy-cli): route projects + set-tags through management"
```

---

### Task 23: Migrate `roy run --project` to management; keep `--cwd` direct

**Files:**
- Modify: `crates/roy-cli/src/main.rs`

- [ ] **Step 1: Find `Run` subcommand**

```bash
grep -n "Run \|run_cmd\|--project\|--cwd" crates/roy-cli/src/main.rs
```

- [ ] **Step 2: Branch on `--project` presence**

```rust
if let Some(project_id) = args.project {
    let req = crate::management::CreateSessionReq {
        agent: args.agent.clone(),
        project_id: Some(project_id),
        cwd: None,
        model: args.model.clone(),
        permission: args.permission.clone(),
        system_prompt: args.system_prompt.clone(),
        agent_name: args.agent_name.clone(),
        tags: args.tags.clone(),
    };
    let created = crate::management::create_session(req).await?;
    // Then attach as before, using created.session_id
    attach_to_session(&socket, &created.session_id).await?;
} else {
    // Existing path: ClientCommand::Spawn { cwd: args.cwd.map(PathBuf::from), ... }
    spawn_direct(&socket, &args).await?;
}
```

Important — the existing `Spawn` ClientCommand constructor needs `cwd: Option<PathBuf>` (from Task 10), no `project_id`, no `tags`. CLI's "direct" path no longer carries tags either; if user passes `--tag k=v` along with `--cwd` (no project), tell them it requires `--project` (or management `POST /sessions` with explicit `cwd`). Document this explicitly in the help text.

Actually simpler: **always route through management if `--tag` is set**, even with `--cwd`. Refactor:

```rust
let needs_mgmt = args.project.is_some() || !args.tags.is_empty() || args.agent_name.is_some();
if needs_mgmt {
    let req = crate::management::CreateSessionReq {
        agent: args.agent.clone(),
        project_id: args.project.clone(),
        cwd: args.cwd.as_ref().map(|p| p.to_string_lossy().into_owned()),
        model: args.model.clone(),
        permission: args.permission.clone(),
        system_prompt: args.system_prompt.clone(),
        agent_name: args.agent_name.clone(),
        tags: args.tags.clone(),
    };
    let created = crate::management::create_session(req).await?;
    attach_to_session(&socket, &created.session_id).await?;
} else {
    spawn_direct(&socket, &args).await?;
}
```

- [ ] **Step 3: Build**

Run: `cargo build -p roy-cli`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add crates/roy-cli/src/main.rs
git commit -m "feat(roy-cli): roy run with --project/--tag goes through management"
```

---

## Phase 6 — Polish & docs

### Task 24: E2E smoke test

**Files:**
- Create: `crates/roy-management/tests/e2e_spawn.rs`

- [ ] **Step 1: Write the test (under `#[ignore]`)**

```rust
//! E2E smoke: real daemon + real management. Ignored by default because it
//! requires built binaries. Run with: cargo test --test e2e_spawn -- --ignored

use std::process::Stdio;
use std::time::Duration;
use tempfile::tempdir;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;

#[tokio::test]
#[ignore]
async fn project_session_visible_in_both_core_and_management() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("roy.sock");
    let journals = dir.path().join("journals");
    let workspace = dir.path().join("workspace");
    let sessions_db = dir.path().join("sessions.db");
    let agents_db = dir.path().join("agents.db");

    // Start daemon
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_roy"))
        .args(["serve", "--socket", socket.to_str().unwrap(),
               "--journal-dir", journals.to_str().unwrap(),
               "--workspace-dir", workspace.to_str().unwrap()])
        .env("ROY_SESSIONS_DB", &sessions_db)
        .stderr(Stdio::piped())
        .spawn()
        .expect("start daemon");

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Start management
    let mut mgmt = Command::new(env!("CARGO_BIN_EXE_roy"))
        .args(["management", "--socket", socket.to_str().unwrap(),
               "--addr", "127.0.0.1:0"])
        .env("ROY_AGENTS_DB", &agents_db)
        .stdout(Stdio::piped())
        .spawn()
        .expect("start management");

    // Read port from management's first stdout log line:
    //   tracing-subscriber prints "listening on 127.0.0.1:NNNN" — ensure
    //   `crates/roy-management/src/lib.rs` after `TcpListener::bind` logs:
    //     tracing::info!("listening on {}", listener.local_addr()?);
    let stdout = mgmt.stdout.take().expect("stdout piped");
    let mut reader = tokio::io::BufReader::new(stdout);
    let mut line = String::new();
    let port = loop {
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        if let Some(addr) = line.split("listening on ").nth(1) {
            break addr.trim().split(':').nth(1).unwrap().to_string();
        }
    };

    let base = format!("http://127.0.0.1:{}", port);
    let proj: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/projects", base))
        .json(&serde_json::json!({"name": "smoke"}))
        .send().await.unwrap()
        .json().await.unwrap();
    let pid = proj["id"].as_str().unwrap();

    let session: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/sessions", base))
        .json(&serde_json::json!({"agent": "claude", "project_id": pid}))
        .send().await.unwrap()
        .json().await.unwrap();
    let sid = session["session_id"].as_str().unwrap();

    // Verify it's visible in `GET /sessions`
    let sessions: serde_json::Value = reqwest::Client::new()
        .get(format!("{}/sessions", base))
        .send().await.unwrap()
        .json().await.unwrap();
    assert!(sessions.as_array().unwrap().iter().any(|s| s["session_id"] == sid));

    let _ = mgmt.kill().await;
    let _ = daemon.kill().await;
}
```

Add `reqwest` to `roy-management` dev-deps if not present:

```toml
[dev-dependencies]
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
```

- [ ] **Step 2: Build**

Run: `cargo build --workspace --all-targets`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/roy-management/tests/e2e_spawn.rs
git commit -m "test(roy-management): e2e smoke skeleton (ignored)"
```

---

### Task 25: Update docs and CHANGELOG

**Files:**
- Modify: `docs/architecture.md`
- Modify: `docs/persistence.md`
- Modify: `docs/wire-protocol.md`
- Modify: `CLAUDE.md`
- Modify: `README.md` (or create `CHANGELOG.md`)

- [ ] **Step 1: Update docs/architecture.md**

Search for any mention of `ProjectRegistry`, `SessionMetadata`, `.meta.json`, `projects.json`. Replace with the new split-store description (use the design doc's Architecture section as the reference). Drop the project-aware Spawn flow diagram and show the new one (from spec's Data Flow section).

- [ ] **Step 2: Update docs/persistence.md**

Replace the section "Per-session metadata file" with "SessionStore SQLite". Replace "ProjectRegistry / projects.json" with "Management MetaStore". Show the two databases and their paths.

- [ ] **Step 3: Update docs/wire-protocol.md**

Update the `ClientCommand` and `ServerEvent` tables — remove `SetTags`, `ListProjects`, `CreateProject`, `DeleteProject`, `ProjectsListed`, `ProjectCreated`, `ProjectDeleted`, `SessionUpdated`. Change `Spawn` to show `cwd` instead of `project_id` and drop `tags`.

- [ ] **Step 4: Update CLAUDE.md**

Find the "What this is" section listing the crates. Update the `roy` and `roy-management` paragraphs to reflect new responsibilities:
- `roy` no longer owns projects or rich metadata.
- `roy-management` owns projects, session_meta, session_tags.

Add to "Commands" section:

```bash
# Upgrade: clear old data once before first run
rm -rf ~/.roy/journals
rm -f  ~/.roy/projects.json
```

- [ ] **Step 5: Update README.md**

Add a "Breaking change" section at the top with the upgrade steps.

- [ ] **Step 6: Commit**

```bash
git add docs/ CLAUDE.md README.md
git commit -m "docs: split-store session metadata"
```

---

### Task 26: Final green-build verification

- [ ] **Step 1: Run all CI checks**

```bash
cargo fmt --all -- --check
cargo build --workspace --all-targets
cargo test --workspace --no-fail-fast
```

Expected: all PASS

- [ ] **Step 2: Smoke-test manually**

```bash
# Start daemon
cargo run -p roy-cli -- serve --socket /tmp/roy.sock &
# Start management
cargo run -p roy-management -- --socket /tmp/roy.sock --addr 127.0.0.1:8079 &
# Hit endpoints
curl -X POST http://127.0.0.1:8079/projects -H 'content-type: application/json' -d '{"name":"smoke"}'
curl http://127.0.0.1:8079/projects
# Spawn a session via CLI
cargo run -p roy-cli -- run --agent claude --project <id-from-above>
```

- [ ] **Step 3: No commit — verification only**

---

## Self-Review (run after all tasks)

Open the spec at `docs/superpowers/specs/2026-05-25-session-metadata-split-store-design.md` side-by-side with this plan. Verify each spec section has at least one corresponding task:

| Spec section | Task(s) |
|--------------|---------|
| Core SQLite schema | T1, T2, T3 |
| Management SQLite schema | T5 |
| Core SessionStore module | T2, T3, T4 |
| Management meta_store module | T6, T7 |
| DaemonClient trait | T8 |
| Wire-protocol delta (removed commands) | T9 |
| Wire-protocol delta (changed Spawn) | T10 |
| project.rs deletion | T11 |
| session_meta.rs deletion | T12 |
| Daemon handler changes | T13 |
| CLI updates | T21, T22, T23 |
| HTTP `/projects` | T16 |
| HTTP `POST /sessions` | T17 |
| HTTP `GET /sessions` | T18 |
| HTTP `PUT tags` + `PATCH` | T19 |
| Orphan-sweep | T20 |
| E2E smoke | T24 |
| Docs | T25 |
| Upgrade note | T25, T26 |

All sections covered. ✓
