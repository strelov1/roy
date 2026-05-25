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
}
