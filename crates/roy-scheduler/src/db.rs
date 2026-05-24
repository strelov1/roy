//! SQLite pool + auto-migrate. The pool is configured with WAL mode and
//! a 5-second busy timeout per spec §4. File is created with mode 0600
//! since `config` columns hold plain JSON that may contain webhook
//! tokens.

use std::path::Path;

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;

/// Run the bundled SQLite migrations against this pool.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("migrations/sqlite");

/// Open or create the SQLite database at `path`, apply migrations, and
/// return a connection pool. Sets mode 0600 on Unix.
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn open_creates_db_and_applies_migrations() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.db");
        let pool = open(&path).await.unwrap();

        // Verify every expected table exists.
        let tables: Vec<(String,)> =
            sqlx::query_as("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .fetch_all(&pool)
                .await
                .unwrap();
        let names: Vec<&str> = tables.iter().map(|(n,)| n.as_str()).collect();

        assert!(names.contains(&"agents"));
        assert!(names.contains(&"triggers"));
        assert!(names.contains(&"fires"));
        assert!(names.contains(&"fire_subscribers"));
        assert!(names.contains(&"fire_subscriber_runs"));
    }

    #[tokio::test]
    async fn open_is_idempotent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.db");
        let _pool = open(&path).await.unwrap();
        // Re-open the same file.
        let _pool2 = open(&path).await.unwrap();
    }
}
