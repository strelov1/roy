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
