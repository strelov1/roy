//! Management-owned tables (projects, session_meta, session_tags) on top of
//! the shared `agents.db` SqlitePool. Migrations live in
//! `crates/roy-management/migrations/sqlite/` and share the database's
//! `_sqlx_migrations` table with `roy-agents`. Versions are coordinated
//! across crates: `roy-agents` currently owns v1-v3; `roy-management` starts
//! at v4 (`migrations/sqlite/0004_*`). Each crate's `Migrator` runs with
//! `set_ignore_missing(true)` so it tolerates rows owned by the other
//! crate. Apply with
//! `MetaStore::apply_migrations(pool)` after `roy_agents::open` has applied
//! its own migrations.

use std::collections::BTreeMap;
use std::path::PathBuf;

use sqlx::SqlitePool;
use thiserror::Error;

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
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub created_by: String,
    pub team_id: Option<String>,
    pub created_at: i64,
}

impl Project {
    fn from_row(row: (String, String, String, String, Option<String>, i64)) -> Self {
        let (id, name, path, created_by, team_id, created_at) = row;
        Self {
            id,
            name,
            path,
            created_by,
            team_id,
            created_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionMeta {
    pub session_id: String,
    pub project_id: Option<String>,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub display_label: Option<String>,
    pub created_by: String,
    pub team_id: Option<String>,
    pub tags: BTreeMap<String, String>,
    pub created_at: i64,
}

#[derive(Clone)]
pub struct MetaStore {
    pool: SqlitePool,
    workspace_dir: PathBuf,
}

impl MetaStore {
    /// `workspace_dir` is where new project directories are created (one
    /// child dir per project name). Production callers should resolve this
    /// once at startup from `$ROY_WORKSPACE_DIR` / `~/.roy/workspace` and
    /// pass it in; tests inject a tempdir.
    pub fn new(pool: SqlitePool, workspace_dir: PathBuf) -> Self {
        Self {
            pool,
            workspace_dir,
        }
    }

    /// Test seam: hands out a clone of the inner pool for direct SQL probes
    /// and failure-injection (e.g. closing it to force write errors).
    #[cfg(test)]
    pub(crate) fn pool(&self) -> SqlitePool {
        self.pool.clone()
    }

    pub async fn apply_migrations(pool: &SqlitePool) -> Result<(), sqlx::Error> {
        let mut migrator = sqlx::migrate!("migrations/sqlite");
        migrator.set_ignore_missing(true);
        migrator.run(pool).await.map_err(sqlx::Error::from)
    }

    pub async fn create_project(
        &self,
        name: &str,
        created_by: &str,
        team_id: Option<&str>,
    ) -> Result<Project, MetaError> {
        validate_project_name(name)?;
        let id = uuid::Uuid::new_v4().to_string();
        let dir = self.workspace_dir.join(name);
        std::fs::create_dir_all(&dir)?;
        let path = dir.to_string_lossy().into_owned();
        let created_at = chrono::Utc::now().timestamp();
        let result = sqlx::query(
            "INSERT INTO projects (id, name, path, created_by, team_id, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(name)
        .bind(&path)
        .bind(created_by)
        .bind(team_id)
        .bind(created_at)
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => Ok(Project::from_row((
                id,
                name.into(),
                path,
                created_by.into(),
                team_id.map(String::from),
                created_at,
            ))),
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => Err(MetaError::Conflict(
                format!("project name already exists: {name}"),
            )),
            Err(e) => Err(MetaError::Db(e)),
        }
    }

    pub async fn list_projects(&self) -> Result<Vec<Project>, MetaError> {
        let rows: Vec<(String, String, String, String, Option<String>, i64)> = sqlx::query_as(
            "SELECT id, name, path, created_by, team_id, created_at \
             FROM projects ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Project::from_row).collect())
    }

    /// Projects visible to `user_id`: their personal projects (team_id IS NULL
    /// AND created_by = user_id) plus every project belonging to a team in
    /// `team_ids`. Callers compute `team_ids` from `TeamStore::list_for_user`.
    pub async fn list_projects_for_user(
        &self,
        user_id: &str,
        team_ids: &[String],
    ) -> Result<Vec<Project>, MetaError> {
        let mut q = String::from(
            "SELECT id, name, path, created_by, team_id, created_at FROM projects \
             WHERE (team_id IS NULL AND created_by = ?)",
        );
        if !team_ids.is_empty() {
            q.push_str(" OR team_id IN (");
            for (i, _) in team_ids.iter().enumerate() {
                if i > 0 {
                    q.push(',');
                }
                q.push('?');
            }
            q.push(')');
        }
        q.push_str(" ORDER BY created_at");

        let mut query =
            sqlx::query_as::<_, (String, String, String, String, Option<String>, i64)>(&q)
                .bind(user_id);
        for tid in team_ids {
            query = query.bind(tid);
        }
        let rows = query.fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(Project::from_row).collect())
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

    /// Rename a project. `name` is validated by `validate_project_name` (same
    /// rules as `create_project`); a unique-violation surfaces as
    /// `MetaError::Conflict`. The `path` is left untouched — the on-disk
    /// workspace dir keeps its original name so existing sessions don't see
    /// their `cwd` move out from under them.
    pub async fn update_project(&self, id: &str, name: &str) -> Result<Project, MetaError> {
        validate_project_name(name)?;
        let result = sqlx::query_as::<_, (String, String, String, String, Option<String>, i64)>(
            "UPDATE projects SET name = ? WHERE id = ? \
                 RETURNING id, name, path, created_by, team_id, created_at",
        )
        .bind(name)
        .bind(id)
        .fetch_optional(&self.pool)
        .await;
        match result {
            Ok(Some(row)) => Ok(Project::from_row(row)),
            Ok(None) => Err(MetaError::NotFound(id.into())),
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => Err(MetaError::Conflict(
                format!("project name already exists: {name}"),
            )),
            Err(e) => Err(MetaError::Db(e)),
        }
    }

    /// Move a project between ownership scopes. Only the `team_id` column
    /// changes — existing session cwds stay put because `resolve_cwd` keys
    /// each session's path on its own captured `(scope, user, team, project,
    /// session)` tuple. New sessions land at the new scope.
    pub async fn set_project_team(
        &self,
        id: &str,
        team_id: Option<&str>,
    ) -> Result<Project, MetaError> {
        let row = sqlx::query_as::<_, (String, String, String, String, Option<String>, i64)>(
            "UPDATE projects SET team_id = ? WHERE id = ? \
             RETURNING id, name, path, created_by, team_id, created_at",
        )
        .bind(team_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(Project::from_row)
            .ok_or(MetaError::NotFound(id.into()))
    }

    pub async fn upsert_session_meta(&self, meta: &SessionMeta) -> Result<(), MetaError> {
        let mut tx = self.pool.begin().await?;

        // On conflict we keep ownership (`created_by`, `team_id`, `created_at`)
        // pinned to the original insert — these identify *who* created the
        // session and must not be silently rewritten by an upsert from a
        // different caller.
        sqlx::query(
            "INSERT INTO session_meta \
            (session_id, project_id, agent_id, agent_name, display_label, \
             created_by, team_id, created_at) \
            VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
            ON CONFLICT(session_id) DO UPDATE SET \
                project_id = excluded.project_id, \
                agent_id = excluded.agent_id, \
                agent_name = excluded.agent_name, \
                display_label = excluded.display_label",
        )
        .bind(&meta.session_id)
        .bind(&meta.project_id)
        .bind(&meta.agent_id)
        .bind(&meta.agent_name)
        .bind(&meta.display_label)
        .bind(&meta.created_by)
        .bind(&meta.team_id)
        .bind(meta.created_at)
        .execute(&mut *tx)
        .await?;

        sqlx::query("DELETE FROM session_tags WHERE session_id = ?")
            .bind(&meta.session_id)
            .execute(&mut *tx)
            .await?;

        for (key, value) in &meta.tags {
            sqlx::query("INSERT INTO session_tags (session_id, key, value) VALUES (?, ?, ?)")
                .bind(&meta.session_id)
                .bind(key)
                .bind(value)
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
            String,
            Option<String>,
            i64,
        )> = sqlx::query_as(
            "SELECT session_id, project_id, agent_id, agent_name, display_label, \
                    created_by, team_id, created_at \
            FROM session_meta WHERE session_id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            None => Ok(None),
            Some((
                sid,
                proj_id,
                agent_id,
                agent_name,
                display_label,
                created_by,
                team_id,
                created_at,
            )) => {
                let tag_rows: Vec<(String, String)> = sqlx::query_as(
                    "SELECT key, value FROM session_tags WHERE session_id = ? ORDER BY key",
                )
                .bind(session_id)
                .fetch_all(&self.pool)
                .await?;

                let tags = tag_rows.into_iter().collect();

                Ok(Some(SessionMeta {
                    session_id: sid,
                    project_id: proj_id,
                    agent_id,
                    agent_name,
                    display_label,
                    created_by,
                    team_id,
                    tags,
                    created_at,
                }))
            }
        }
    }

    /// Bulk read of session_meta rows for a known set of session ids. Tags are
    /// included. Returns only rows that match — missing ids are omitted (the
    /// caller is expected to fold them with empty meta).
    pub async fn list_session_metas(
        &self,
        session_ids: &[String],
    ) -> Result<Vec<SessionMeta>, MetaError> {
        if session_ids.is_empty() {
            return Ok(Vec::new());
        }
        // We can't easily bind a slice as `IN (?)` in sqlx — build the in-clause manually.
        let placeholders = (0..session_ids.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let meta_sql = format!(
            "SELECT session_id, project_id, agent_id, agent_name, display_label, \
                    created_by, team_id, created_at \
             FROM session_meta WHERE session_id IN ({placeholders})"
        );
        let tag_sql = format!(
            "SELECT session_id, key, value FROM session_tags WHERE session_id IN ({placeholders})"
        );

        let mut meta_q = sqlx::query_as::<
            _,
            (
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
                String,
                Option<String>,
                i64,
            ),
        >(&meta_sql);
        for sid in session_ids {
            meta_q = meta_q.bind(sid);
        }
        let meta_rows = meta_q.fetch_all(&self.pool).await?;

        let mut tag_q = sqlx::query_as::<_, (String, String, String)>(&tag_sql);
        for sid in session_ids {
            tag_q = tag_q.bind(sid);
        }
        let tag_rows = tag_q.fetch_all(&self.pool).await?;

        let mut tags_by_sid: std::collections::HashMap<String, BTreeMap<String, String>> =
            std::collections::HashMap::new();
        for (sid, k, v) in tag_rows {
            tags_by_sid.entry(sid).or_default().insert(k, v);
        }

        Ok(meta_rows
            .into_iter()
            .map(
                |(
                    sid,
                    project_id,
                    agent_id,
                    agent_name,
                    display_label,
                    created_by,
                    team_id,
                    created_at,
                )| SessionMeta {
                    tags: tags_by_sid.remove(&sid).unwrap_or_default(),
                    session_id: sid,
                    project_id,
                    agent_id,
                    agent_name,
                    display_label,
                    created_by,
                    team_id,
                    created_at,
                },
            )
            .collect())
    }

    /// All session_meta rows (tags joined). Used by orphan-sweep.
    pub async fn list_all_session_metas(&self) -> Result<Vec<SessionMeta>, MetaError> {
        let rows: Vec<(String,)> = sqlx::query_as("SELECT session_id FROM session_meta")
            .fetch_all(&self.pool)
            .await?;
        let ids: Vec<String> = rows.into_iter().map(|r| r.0).collect();
        self.list_session_metas(&ids).await
    }

    pub async fn replace_tags(
        &self,
        session_id: &str,
        tags: &BTreeMap<String, String>,
    ) -> Result<(), MetaError> {
        let mut tx = self.pool.begin().await?;
        // Guard against orphan tag rows: session_tags has no FK to
        // session_meta in SQLite, so we enforce the parent-must-exist
        // invariant here. `upsert_session_meta` short-circuits this path
        // because it writes the meta row in the same tx.
        let exists: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM session_meta WHERE session_id = ?")
                .bind(session_id)
                .fetch_one(&mut *tx)
                .await?;
        if exists.0 == 0 {
            return Err(MetaError::NotFound(session_id.into()));
        }
        sqlx::query("DELETE FROM session_tags WHERE session_id = ?")
            .bind(session_id)
            .execute(&mut *tx)
            .await?;

        for (key, value) in tags {
            sqlx::query("INSERT INTO session_tags (session_id, key, value) VALUES (?, ?, ?)")
                .bind(session_id)
                .bind(key)
                .bind(value)
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    pub async fn delete_session_meta(&self, session_id: &str) -> Result<(), MetaError> {
        let mut tx = self.pool.begin().await?;

        let res = sqlx::query("DELETE FROM session_meta WHERE session_id = ?")
            .bind(session_id)
            .execute(&mut *tx)
            .await?;

        if res.rows_affected() == 0 {
            return Err(MetaError::NotFound(session_id.into()));
        }

        sqlx::query("DELETE FROM session_tags WHERE session_id = ?")
            .bind(session_id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
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

/// `$ROY_WORKSPACE_DIR`, else `~/.roy/workspace`. Resolved at startup by
/// callers (e.g. `roy_management::run`) and passed into `MetaStore::new`.
pub fn default_workspace_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("ROY_WORKSPACE_DIR") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".roy/workspace")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Returns the store and the id of a freshly-seeded user (`"alice"`).
    /// Tests need a real `users` row because `projects.created_by` and
    /// `session_meta.created_by` are NOT NULL FKs into `users(id)`.
    async fn fresh_store() -> (MetaStore, String) {
        let dir = tempdir().unwrap();
        let pool = roy_agents::open(&dir.path().join("agents.db"))
            .await
            .unwrap();
        MetaStore::apply_migrations(&pool).await.unwrap();
        roy_auth::apply_migrations(&pool).await.unwrap();
        let user = roy_auth::test_support::make_user(&pool, "alice").await;
        let workspace = dir.path().join("workspace");
        // Leak the tempdir: the SqlitePool inside MetaStore must keep reading
        // the file for the rest of the test, but the dir would otherwise be
        // dropped when this function returns.
        std::mem::forget(dir);
        (MetaStore::new(pool, workspace), user.id)
    }

    #[tokio::test]
    async fn create_then_list_project() {
        let (store, uid) = fresh_store().await;
        let p = store.create_project("my-proj", &uid, None).await.unwrap();
        assert_eq!(p.name, "my-proj");
        assert_eq!(p.created_by, uid);
        assert_eq!(p.team_id, None);
        let listed = store.list_projects().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, p.id);
        assert_eq!(listed[0].created_by, uid);
    }

    #[tokio::test]
    async fn create_duplicate_is_conflict() {
        let (store, uid) = fresh_store().await;
        store.create_project("dup", &uid, None).await.unwrap();
        let err = store.create_project("dup", &uid, None).await.unwrap_err();
        assert!(matches!(err, MetaError::Conflict(_)));
    }

    #[tokio::test]
    async fn invalid_name_rejected() {
        let (store, uid) = fresh_store().await;
        for bad in ["", ".hidden", "has/slash", "has space"] {
            assert!(matches!(
                store.create_project(bad, &uid, None).await,
                Err(MetaError::Invalid(_))
            ));
        }
    }

    #[tokio::test]
    async fn delete_project() {
        let (store, uid) = fresh_store().await;
        let p = store.create_project("del-me", &uid, None).await.unwrap();
        store.delete_project(&p.id).await.unwrap();
        assert!(matches!(
            store.delete_project(&p.id).await,
            Err(MetaError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn update_project_renames_in_place() {
        let (store, uid) = fresh_store().await;
        let p = store.create_project("old-name", &uid, None).await.unwrap();
        let renamed = store.update_project(&p.id, "new-name").await.unwrap();
        assert_eq!(renamed.id, p.id);
        assert_eq!(renamed.name, "new-name");
        // path stays unchanged — on-disk dir keeps its original name
        assert_eq!(renamed.path, p.path);
        assert_eq!(renamed.created_at, p.created_at);
        // ownership is preserved
        assert_eq!(renamed.created_by, p.created_by);
        assert_eq!(renamed.team_id, p.team_id);
        let listed = store.list_projects().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "new-name");
    }

    #[tokio::test]
    async fn update_project_unknown_id_is_not_found() {
        let (store, _uid) = fresh_store().await;
        let err = store
            .update_project("nonexistent", "whatever")
            .await
            .unwrap_err();
        assert!(matches!(err, MetaError::NotFound(_)));
    }

    #[tokio::test]
    async fn update_project_invalid_name_rejected() {
        let (store, uid) = fresh_store().await;
        let p = store.create_project("ok", &uid, None).await.unwrap();
        for bad in ["", ".hidden", "has space", "has/slash"] {
            assert!(matches!(
                store.update_project(&p.id, bad).await,
                Err(MetaError::Invalid(_))
            ));
        }
    }

    #[tokio::test]
    async fn update_project_to_existing_name_is_conflict() {
        let (store, uid) = fresh_store().await;
        store.create_project("a", &uid, None).await.unwrap();
        let b = store.create_project("b", &uid, None).await.unwrap();
        let err = store.update_project(&b.id, "a").await.unwrap_err();
        assert!(matches!(err, MetaError::Conflict(_)));
    }

    /// `roy-agents` and `roy-management` share `_sqlx_migrations` and each
    /// crate's migrator runs with `set_ignore_missing(true)`. This test
    /// simulates a second process start (re-open agents after management
    /// already wrote v2; re-apply management) and asserts neither side
    /// errors with `VersionMissing` on the foreign-owned rows.
    #[tokio::test]
    async fn shared_migrations_table_idempotent_across_crates() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("agents.db");
        let pool = roy_agents::open(&db).await.unwrap();
        MetaStore::apply_migrations(&pool).await.unwrap();
        pool.close().await;
        let pool = roy_agents::open(&db).await.unwrap();
        MetaStore::apply_migrations(&pool).await.unwrap();
        let versions: Vec<(i64,)> =
            sqlx::query_as("SELECT version FROM _sqlx_migrations ORDER BY version")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(versions, vec![(1,), (2,), (3,), (4,), (5,)]);
    }

    fn meta_with(session_id: &str, created_by: &str, tags: &[(&str, &str)]) -> SessionMeta {
        SessionMeta {
            session_id: session_id.into(),
            project_id: None,
            agent_id: None,
            agent_name: Some("claude-sonnet-4-6".into()),
            display_label: Some("test session".into()),
            created_by: created_by.into(),
            team_id: None,
            tags: tags
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
            created_at: 1_700_000_000,
        }
    }

    #[tokio::test]
    async fn upsert_then_get_session_meta() {
        let (store, uid) = fresh_store().await;
        let meta = meta_with("sess1", &uid, &[("env", "prod"), ("team", "platform")]);
        store.upsert_session_meta(&meta).await.unwrap();

        let retrieved = store
            .get_session_meta("sess1")
            .await
            .unwrap()
            .expect("should exist");
        assert_eq!(retrieved.session_id, "sess1");
        assert_eq!(retrieved.agent_name, Some("claude-sonnet-4-6".into()));
        assert_eq!(retrieved.display_label, Some("test session".into()));
        assert_eq!(retrieved.created_by, uid);
        assert_eq!(retrieved.team_id, None);
        assert_eq!(retrieved.tags.len(), 2);
        assert_eq!(retrieved.tags.get("env"), Some(&"prod".into()));
        assert_eq!(retrieved.tags.get("team"), Some(&"platform".into()));
    }

    #[tokio::test]
    async fn upsert_is_idempotent_and_replaces_tags() {
        let (store, uid) = fresh_store().await;
        let meta1 = meta_with("sess2", &uid, &[("a", "1"), ("b", "2")]);
        store.upsert_session_meta(&meta1).await.unwrap();

        let meta2 = meta_with("sess2", &uid, &[("a", "9"), ("c", "3")]);
        store.upsert_session_meta(&meta2).await.unwrap();

        let retrieved = store
            .get_session_meta("sess2")
            .await
            .unwrap()
            .expect("should exist");
        assert_eq!(retrieved.tags.len(), 2);
        assert_eq!(retrieved.tags.get("a"), Some(&"9".into()));
        assert_eq!(retrieved.tags.get("c"), Some(&"3".into()));
        assert!(!retrieved.tags.contains_key("b"));
    }

    #[tokio::test]
    async fn get_session_meta_missing_returns_none() {
        let (store, _uid) = fresh_store().await;
        let result = store.get_session_meta("nonexistent").await.unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn replace_tags_replaces_atomically() {
        let (store, uid) = fresh_store().await;
        let meta = meta_with("sess3", &uid, &[("a", "1")]);
        store.upsert_session_meta(&meta).await.unwrap();

        // Replace with empty tags
        store.replace_tags("sess3", &BTreeMap::new()).await.unwrap();
        let retrieved = store
            .get_session_meta("sess3")
            .await
            .unwrap()
            .expect("session should still exist");
        assert_eq!(retrieved.tags.len(), 0);

        // Replace with new tags
        let mut new_tags = BTreeMap::new();
        new_tags.insert("x".into(), "y".into());
        new_tags.insert("p".into(), "q".into());
        store.replace_tags("sess3", &new_tags).await.unwrap();
        let retrieved = store
            .get_session_meta("sess3")
            .await
            .unwrap()
            .expect("session should still exist");
        assert_eq!(retrieved.tags.len(), 2);
        assert_eq!(retrieved.tags.get("x"), Some(&"y".into()));
        assert_eq!(retrieved.tags.get("p"), Some(&"q".into()));
    }

    #[tokio::test]
    async fn replace_tags_on_unknown_session_is_not_found() {
        let (store, _uid) = fresh_store().await;
        let mut tags = BTreeMap::new();
        tags.insert("k".into(), "v".into());
        let err = store
            .replace_tags("never-existed", &tags)
            .await
            .unwrap_err();
        assert!(matches!(err, MetaError::NotFound(_)));
        // And no orphan tag rows landed in the table.
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM session_tags")
            .fetch_one(&store.pool())
            .await
            .unwrap();
        assert_eq!(count.0, 0);
    }

    #[tokio::test]
    async fn list_session_metas_returns_known_only() {
        let (store, uid) = fresh_store().await;
        store
            .upsert_session_meta(&meta_with("a", &uid, &[("k", "v")]))
            .await
            .unwrap();
        store
            .upsert_session_meta(&meta_with("b", &uid, &[]))
            .await
            .unwrap();
        let rows = store
            .list_session_metas(&["a".into(), "b".into(), "missing".into()])
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        let a = rows.iter().find(|m| m.session_id == "a").unwrap();
        assert_eq!(a.tags.get("k").unwrap(), "v");
    }

    #[tokio::test]
    async fn delete_session_meta_removes_row_and_tags() {
        let (store, uid) = fresh_store().await;
        let meta = meta_with("sess4", &uid, &[("env", "test")]);
        store.upsert_session_meta(&meta).await.unwrap();

        // Verify tags exist
        let tag_count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM session_tags WHERE session_id = ?")
                .bind("sess4")
                .fetch_one(&store.pool())
                .await
                .unwrap();
        assert_eq!(tag_count.0, 1);

        // Delete
        store.delete_session_meta("sess4").await.unwrap();

        // Session should be gone
        let result = store.get_session_meta("sess4").await.unwrap();
        assert_eq!(result, None);

        // Tags should also be gone
        let tag_count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM session_tags WHERE session_id = ?")
                .bind("sess4")
                .fetch_one(&store.pool())
                .await
                .unwrap();
        assert_eq!(tag_count.0, 0);

        // Second delete should return NotFound
        let err = store.delete_session_meta("sess4").await.unwrap_err();
        assert!(matches!(err, MetaError::NotFound(_)));
    }
}
