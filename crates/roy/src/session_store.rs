//! Boot-kit-only SQLite store: minimum fields needed to resume a session
//! after a daemon restart. Lives at `~/.local/state/roy/sessions.db`.

use std::path::{Path, PathBuf};

use chrono::Utc;
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
        let now = Utc::now().timestamp();
        sqlx::query(
            "UPDATE sessions SET closed_at = ? WHERE session_id = ? AND closed_at IS NULL",
        )
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
}
