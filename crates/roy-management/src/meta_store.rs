//! Management-owned tables (projects, session_meta, session_tags) on top of
//! the shared `agents.db` SqlitePool. Migrations live in
//! `crates/roy-management/migrations/sqlite/` and share the database's
//! `_sqlx_migrations` table with `roy-agents`. Versions are coordinated
//! across crates: `roy-agents` owns v1, `roy-management` owns v2. Each
//! crate's `Migrator` runs with `set_ignore_missing(true)` so it tolerates
//! rows owned by the other crate. Apply with
//! `MetaStore::apply_migrations(pool)` after `roy_agents::open` has applied
//! its own migrations.

use std::collections::BTreeMap;

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
        let mut migrator = sqlx::migrate!("migrations/sqlite");
        migrator.set_ignore_missing(true);
        migrator.run(pool).await.map_err(sqlx::Error::from)
    }

    pub async fn create_project(&self, name: &str) -> Result<Project, MetaError> {
        validate_project_name(name)?;
        let id = uuid::Uuid::new_v4().to_string();
        let workspace = workspace_dir_default();
        let path = workspace.join(name).to_string_lossy().into_owned();
        let created_at = chrono::Utc::now().timestamp();
        let result =
            sqlx::query("INSERT INTO projects (id, name, path, created_at) VALUES (?, ?, ?, ?)")
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

    pub async fn upsert_session_meta(&self, meta: &SessionMeta) -> Result<(), MetaError> {
        let mut tx = self.pool.begin().await?;

        sqlx::query(
            "INSERT INTO session_meta \
            (session_id, project_id, agent_id, agent_name, display_label, created_at) \
            VALUES (?, ?, ?, ?, ?, ?) \
            ON CONFLICT(session_id) DO UPDATE SET \
                project_id = excluded.project_id, \
                agent_id = excluded.agent_id, \
                agent_name = excluded.agent_name, \
                display_label = excluded.display_label, \
                created_at = excluded.created_at",
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
            i64,
        )> = sqlx::query_as(
            "SELECT session_id, project_id, agent_id, agent_name, display_label, created_at \
            FROM session_meta WHERE session_id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await?;

        match row {
            None => Ok(None),
            Some((sid, proj_id, agent_id, agent_name, display_label, created_at)) => {
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
                    tags,
                    created_at,
                }))
            }
        }
    }

    pub async fn set_tags(
        &self,
        session_id: &str,
        tags: &BTreeMap<String, String>,
    ) -> Result<(), MetaError> {
        let mut tx = self.pool.begin().await?;
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
        assert_eq!(versions, vec![(1,), (2,)]);
    }

    fn meta_with(session_id: &str, tags: &[(&str, &str)]) -> SessionMeta {
        SessionMeta {
            session_id: session_id.into(),
            project_id: None,
            agent_id: None,
            agent_name: Some("claude-sonnet-4-6".into()),
            display_label: Some("test session".into()),
            tags: tags
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
            created_at: 1_700_000_000,
        }
    }

    #[tokio::test]
    async fn upsert_then_get_session_meta() {
        let store = fresh_store().await;
        let meta = meta_with("sess1", &[("env", "prod"), ("team", "platform")]);
        store.upsert_session_meta(&meta).await.unwrap();

        let retrieved = store
            .get_session_meta("sess1")
            .await
            .unwrap()
            .expect("should exist");
        assert_eq!(retrieved.session_id, "sess1");
        assert_eq!(retrieved.agent_name, Some("claude-sonnet-4-6".into()));
        assert_eq!(retrieved.display_label, Some("test session".into()));
        assert_eq!(retrieved.tags.len(), 2);
        assert_eq!(retrieved.tags.get("env"), Some(&"prod".into()));
        assert_eq!(retrieved.tags.get("team"), Some(&"platform".into()));
    }

    #[tokio::test]
    async fn upsert_is_idempotent_and_replaces_tags() {
        let store = fresh_store().await;
        let meta1 = meta_with("sess2", &[("a", "1"), ("b", "2")]);
        store.upsert_session_meta(&meta1).await.unwrap();

        let meta2 = meta_with("sess2", &[("a", "9"), ("c", "3")]);
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
        let store = fresh_store().await;
        let result = store.get_session_meta("nonexistent").await.unwrap();
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn set_tags_replaces_atomically() {
        let store = fresh_store().await;
        let meta = meta_with("sess3", &[("a", "1")]);
        store.upsert_session_meta(&meta).await.unwrap();

        // Replace with empty tags
        store.set_tags("sess3", &BTreeMap::new()).await.unwrap();
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
        store.set_tags("sess3", &new_tags).await.unwrap();
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
    async fn delete_session_meta_removes_row_and_tags() {
        let store = fresh_store().await;
        let meta = meta_with("sess4", &[("env", "test")]);
        store.upsert_session_meta(&meta).await.unwrap();

        // Verify tags exist
        let tag_count: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM session_tags WHERE session_id = ?")
                .bind("sess4")
                .fetch_one(&store.pool)
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
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert_eq!(tag_count.0, 0);

        // Second delete should return NotFound
        let err = store.delete_session_meta("sess4").await.unwrap_err();
        assert!(matches!(err, MetaError::NotFound(_)));
    }
}
